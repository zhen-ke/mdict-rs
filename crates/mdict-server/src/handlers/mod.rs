mod error;
mod response;

pub use error::AppError;
use response::{
    cacheable_binary_response, css_response, js_response, not_found, ok_response,
    service_unavailable, stream_file_response,
};

use crate::app_state::AppState;
use crate::config::DictInfo;
use crate::lucky;
use crate::query::{
    DictFilter, QueryError, detect_content_type, fuzzy_suggest, query, query_aggregate,
    query_specific_entry, query_specific_resource, query_with_trace, suggest,
};
use mdict_core::css_scope::scope_css;
use mdict_core::indexing::index_status;
use mdict_core::rewrite::rewrite_css_urls;
use serde_derive::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path as FsPath, PathBuf};

use axum::{
    body::Bytes,
    extract::{Form, Path, Query, State},
    http::{HeaderMap, Uri},
    response::{Json, Response},
};
use tokio::fs;

const RESOURCE_CACHE_CONTROL: &str = "public, max-age=86400, immutable";
/// `index.html` 是 SPA 入口：前端更新后必须让浏览器重拉，故不能 immutable。
/// 改为每次发条件请求（cacheable_binary_response 已带 ETag、支持 If-None-Match
/// 返回 304）；HTML 体积小、代价可忽略。其余静态资源（index.js/css / lm6.* /
/// 字体）保留 `immutable,24h`，享受长缓存——它们变更紫疾 hashed 现阶段依据此头。
const HTML_CACHE_CONTROL: &str = "max-age=0, must-revalidate";
const MAX_CACHEABLE_RESOURCE_BYTES: usize = 1024 * 1024;
const MAX_CACHEABLE_MEDIA_BYTES: usize = 256 * 1024;

/// 按请求路径选择静态资源的缓存头。`index.html`（直接访问或目录索引 `/`）
/// 走可重验策略；其余路径走 immutable 长缓存。词典资源（/dict/{id}/res/…）
/// 不会以 `index.html` 结尾，故也可安全调用——退回 RESOURCE_CACHE_CONTROL。
fn static_cache_control(request_path: &str) -> &'static str {
    if request_path == "/" || request_path.ends_with("index.html") {
        HTML_CACHE_CONTROL
    } else {
        RESOURCE_CACHE_CONTROL
    }
}

#[derive(Deserialize, Debug)]
pub struct SuggestQuery {
    q: String,
    /// Optional comma-separated dict IDs to restrict search scope.
    dicts: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct QueryForm {
    word: String,
    /// Optional comma-separated dict IDs to restrict search scope.
    dicts: Option<String>,
}

/// Parse an optional comma-separated dict-id string into a `DictFilter`.
///
/// Returns `None` (= all dicts) when the input is absent or empty.
fn parse_dict_filter(raw: &Option<String>) -> DictFilter {
    let Some(s) = raw else { return None };
    let ids: HashSet<String> = s
        .split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if ids.is_empty() { None } else { Some(ids) }
}

async fn spawn_blocking_query<T, F>(
    state: &AppState,
    context: &'static str,
    task: F,
) -> Result<T, AppError>
where
    T: Send + 'static,
    F: FnOnce(AppState) -> T + Send + 'static,
{
    let _query_slot = state.try_acquire_query_slot().ok_or(AppError::Overloaded)?;
    let task_state = state.clone();
    tokio::task::spawn_blocking(move || task(task_state))
        .await
        .map_err(|e| AppError::Internal(format!("{context} task failed: {e}")))
}

async fn query_aggregate_cached(
    state: &AppState,
    word: String,
    filter: DictFilter,
) -> Result<Response, AppError> {
    let cache_key = cache_key_aggregate_entry(&word, &filter);
    let negative_key = negative_key(&cache_key);
    if let Some((data, content_type)) = state.get_entry_cached(&cache_key) {
        return Ok(ok_response(data, &content_type));
    }
    if state.is_negative_cached(&negative_key) {
        return Err(AppError::NotFound);
    }

    let query_word = word.clone();
    let result = spawn_blocking_query(state, "aggregate query", move |task_state| {
        query_aggregate(&task_state, query_word, &filter)
    })
    .await?;

    match result {
        Ok((data, content_type)) => {
            state.put_entry_cached(cache_key, data.clone(), content_type.clone());
            state.clear_negative_cache(&negative_key);
            Ok(ok_response(data, &content_type))
        }
        Err(e) => {
            if matches!(e, QueryError::NotFound) {
                state.put_negative_cache(negative_key);
            }
            Err(AppError::from(e))
        }
    }
}

pub(crate) async fn handle_query(
    State(state): State<AppState>,
    Form(params): Form<QueryForm>,
) -> Result<Response, AppError> {
    let word = params.word.trim().to_string();
    let filter = parse_dict_filter(&params.dicts);
    tracing::info!("Processing query for word: {}, filter: {:?}", word, filter);
    query_aggregate_cached(&state, word, filter).await
}

pub(crate) async fn handle_lucky(State(state): State<AppState>) -> Result<Response, AppError> {
    let word = lucky::lucky_word(&state);
    tracing::info!("Lucky query for word: {}", word);
    // Lucky always queries all dicts — it's about discovery.
    query_aggregate_cached(&state, word, None).await
}

pub(crate) async fn handle_suggest(
    State(state): State<AppState>,
    Query(params): Query<SuggestQuery>,
) -> Result<Json<Vec<String>>, AppError> {
    let q = params.q;
    let filter = parse_dict_filter(&params.dicts);
    let result = match spawn_blocking_query(&state, "suggest", move |task_state| {
        suggest(&task_state, q, 10, &filter)
    })
    .await
    {
        Ok(result) => result,
        Err(AppError::Overloaded) => return Err(AppError::Overloaded),
        Err(e) => {
            tracing::warn!("Suggest task failed: {}", e);
            return Ok(Json(vec![]));
        }
    };
    match result {
        Ok(suggestions) => Ok(Json(suggestions)),
        Err(e) => {
            tracing::warn!("Suggest failed: {}", e);
            Ok(Json(vec![]))
        }
    }
}

/// GET /suggest/fuzzy?q=...&dicts=... — did-you-mean 近邻建议。
///
/// `/query` 未命中时由前端调用，返回与查询词编辑距离 ≤ 2 的词条。跨词典
/// 并行执行（rayon）后按最小距离合并、排序、去重，最多 10 条。
pub(crate) async fn handle_suggest_fuzzy(
    State(state): State<AppState>,
    Query(params): Query<SuggestQuery>,
) -> Result<Json<Vec<String>>, AppError> {
    let q = params.q;
    let filter = parse_dict_filter(&params.dicts);
    let result = match spawn_blocking_query(&state, "fuzzy suggest", move |task_state| {
        fuzzy_suggest(&task_state, q, 10, &filter)
    })
    .await
    {
        Ok(result) => result,
        Err(AppError::Overloaded) => return Err(AppError::Overloaded),
        Err(e) => {
            tracing::warn!("Fuzzy suggest task failed: {}", e);
            return Ok(Json(vec![]));
        }
    };
    match result {
        Ok(suggestions) => Ok(Json(suggestions)),
        Err(e) => {
            tracing::warn!("Fuzzy suggest failed: {}", e);
            Ok(Json(vec![]))
        }
    }
}

/// Debug endpoint to show @@@LINK redirect chain
/// Usage: GET /trace?q=whams
pub(crate) async fn handle_trace(
    State(state): State<AppState>,
    Query(params): Query<SuggestQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let q = params.q;
    let result = match spawn_blocking_query(&state, "trace", move |task_state| {
        query_with_trace(&task_state, q)
    })
    .await
    {
        Ok(result) => result,
        Err(AppError::Overloaded) => return Err(AppError::Overloaded),
        Err(e) => {
            return Ok(Json(serde_json::json!({
                "error": format!("trace task failed: {}", e),
            })));
        }
    };
    match result {
        Ok((chain, final_word)) => Ok(Json(serde_json::json!({
            "chain": chain,
            "depth": chain.len() - 1,
            "final_word": final_word,
        }))),
        Err(e) => Ok(Json(serde_json::json!({
            "error": e.to_string(),
        }))),
    }
}

pub(crate) async fn handle_resource(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let path = uri.path();
    let cache_key = cache_key_global_resource(path);
    let negative_key = negative_key(&cache_key);
    // 依据请求路径选择缓存头：index.html 可重验、其它 immutable。缓存命中与下游
    // 分发同一条头策略，避免命中后取回首选项不一致。
    let cache_control = static_cache_control(path);

    if let Some((data, content_type)) = state.get_resource_cached(&cache_key) {
        return cacheable_binary_response(data, &content_type, cache_control, Some(&headers));
    }
    if state.is_negative_cached(&negative_key) {
        return not_found();
    }

    if let Some(static_file) =
        resolve_static_file(state.static_dir(), path.trim_start_matches('/'), true).await
    {
        if should_cache_resource(&static_file.content_type, static_file.size) {
            match fs::read(&static_file.path).await {
                Ok(data) => {
                    let data = Bytes::from(data);
                    state.put_resource_cached(
                        cache_key,
                        data.clone(),
                        static_file.content_type.clone(),
                    );
                    state.clear_negative_cache(&negative_key);
                    return cacheable_binary_response(
                        data,
                        &static_file.content_type,
                        cache_control,
                        Some(&headers),
                    );
                }
                Err(e) => {
                    tracing::warn!("Failed to read static file {:?}: {}", static_file.path, e);
                }
            }
        } else if let Some(resp) =
            stream_file_response(&static_file.path, &static_file.content_type, cache_control).await
        {
            state.clear_negative_cache(&negative_key);
            return resp;
        }
    }

    let candidates = build_resource_candidates(path);
    if candidates.is_empty() {
        state.put_negative_cache(negative_key);
        return not_found();
    }

    let result = match spawn_blocking_query(&state, "resource query", move |query_state| {
        for candidate in &candidates {
            tracing::debug!("resource try key: {}", candidate);
            if let Ok((data, content_type)) = query(&query_state, candidate.clone()) {
                return Some((data, content_type));
            }
        }
        None
    })
    .await
    {
        Ok(result) => result,
        Err(AppError::Overloaded) => return service_unavailable(),
        Err(e) => {
            tracing::warn!("Resource query failed: {}", e);
            state.put_negative_cache(negative_key);
            return not_found();
        }
    };

    match result {
        Some((data, content_type)) => {
            if should_cache_resource(&content_type, data.len()) {
                state.put_resource_cached(cache_key, data.clone(), content_type.clone());
            }
            state.clear_negative_cache(&negative_key);
            cacheable_binary_response(data, &content_type, cache_control, Some(&headers))
        }
        None => {
            state.put_negative_cache(negative_key);
            not_found()
        }
    }
}

/// GET /dict/{id}/entry/{*word}
pub(crate) async fn handle_dict_entry(
    State(state): State<AppState>,
    Path((dict_id, word)): Path<(String, String)>,
) -> Response {
    let files = state.get_dict_text_files_by_id(&dict_id);
    if files.is_empty() {
        return not_found();
    }

    let word = word.trim().to_string();
    if word.is_empty() {
        return not_found();
    }

    let cache_key = cache_key_dict_entry(&dict_id, &word);
    let negative_key = negative_key(&cache_key);
    if let Some((data, content_type)) = state.get_entry_cached(&cache_key) {
        return ok_response(data, &content_type);
    }
    if state.is_negative_cached(&negative_key) {
        return not_found();
    }

    let dict_id_for_query = dict_id.clone();
    let query_word = word.clone();
    let result = match spawn_blocking_query(&state, "dict entry query", move |state_cloned| {
        for file in files {
            if let Ok(Some((data, content_type))) =
                query_specific_entry(&state_cloned, &file, &query_word, &dict_id_for_query)
            {
                return Some((data, content_type));
            }
        }
        None
    })
    .await
    {
        Ok(result) => result,
        Err(AppError::Overloaded) => return service_unavailable(),
        Err(e) => {
            tracing::warn!("dict entry query failed: {}", e);
            state.put_negative_cache(negative_key);
            return not_found();
        }
    };

    match result {
        Some((data, content_type)) => {
            state.put_entry_cached(cache_key, data.clone(), content_type.clone());
            state.clear_negative_cache(&negative_key);
            ok_response(data, &content_type)
        }
        None => {
            state.put_negative_cache(negative_key);
            not_found()
        }
    }
}

/// GET /dict/{id}/res/{*path}
pub(crate) async fn handle_dict_res(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((dict_id, path)): Path<(String, String)>,
) -> Response {
    query_dict_resource(state, headers, dict_id, path).await
}

/// GET /dict/{id}/audio/{*path}
pub(crate) async fn handle_dict_audio(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((dict_id, path)): Path<(String, String)>,
) -> Response {
    query_dict_resource(state, headers, dict_id, path).await
}

/// Legacy route compatibility: GET /resource/{id}/{*path}
pub(crate) async fn handle_dict_resource(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((dict_id, path)): Path<(String, String)>,
) -> Response {
    query_dict_resource(state, headers, dict_id, path).await
}

async fn query_dict_resource(
    state: AppState,
    headers: HeaderMap,
    dict_id: String,
    path: String,
) -> Response {
    if path.contains("..") {
        tracing::warn!("Rejected dict resource traversal attempt: {}", path);
        return not_found();
    }

    let cache_key = cache_key_dict_resource(&dict_id, &path);
    let negative_key = negative_key(&cache_key);
    if let Some((data, content_type)) = state.get_resource_cached(&cache_key) {
        return cacheable_binary_response(
            data,
            &content_type,
            RESOURCE_CACHE_CONTROL,
            Some(&headers),
        );
    }
    if state.is_negative_cached(&negative_key) {
        return not_found();
    }

    let files = state.get_dict_resource_files_by_id(&dict_id);
    let candidates = build_resource_candidates(&path);

    if !files.is_empty() && !candidates.is_empty() {
        let result = match spawn_blocking_query(&state, "dict resource query", move |query_state| {
            for file in files {
                for candidate in &candidates {
                    if let Ok(Some((data, content_type))) =
                        query_specific_resource(&query_state, &file, candidate)
                    {
                        return Some((data, content_type));
                    }
                }
            }
            None
        })
        .await
        {
            Ok(result) => result,
            Err(AppError::Overloaded) => return service_unavailable(),
            Err(e) => {
                tracing::warn!("dict resource query failed: {}", e);
                state.put_negative_cache(negative_key);
                return not_found();
            }
        };

        if let Some((data, content_type)) = result {
            // CSS is scoped per-dict *before* caching so the cache stores
            // already-scoped bytes and cache hits return them verbatim (scope_css
            // is not idempotent — re-scoping would double-prefix selectors).
            let data = scope_dict_css_if_css(&dict_id, data, &content_type);
            if should_cache_resource(&content_type, data.len()) {
                state.put_resource_cached(cache_key, data.clone(), content_type.clone());
            }
            state.clear_negative_cache(&negative_key);
            return cacheable_binary_response(
                data,
                &content_type,
                RESOURCE_CACHE_CONTROL,
                Some(&headers),
            );
        }
    }

    // 词典文件夹资源：同名词典文件夹 + 词典根目录（`mdict/`）优先于 static
    // fallback 加载（优先级：MDD 索引 > 同名子文件夹 > 词典根目录 > static
    // 目录）。词典作者自带的 css/js/图片放这两个位置均可生效——同名子文件夹
    // 归属单本词典，根目录平铺的文件对词典根目录下所有词典共享。
    let mut folder_candidates: Vec<PathBuf> = Vec::new();
    if let Some(folder) = state.get_dict_folder(&dict_id) {
        folder_candidates.push(folder);
    }
    folder_candidates.push(state.dict_dir().to_path_buf());
    for folder in folder_candidates {
        if let Some(folder_file) = resolve_dict_folder_file(&folder, &path) {
            if let Some(resp) = serve_dict_folder_resource(
                &state,
                &dict_id,
                folder_file,
                &path,
                &cache_key,
                &negative_key,
                &headers,
            )
            .await
            {
                return resp;
            }
        }
    }

    // Fallback: serve static assets for dictionary-rendered pages (e.g. lm6.css/lm6.js).
    if let Some(static_file) = resolve_static_file(state.static_dir(), &path, false).await {
        if should_cache_resource(&static_file.content_type, static_file.size) {
            match fs::read(&static_file.path).await {
                Ok(read) => {
                    let data = scope_dict_css_if_css(
                        &dict_id,
                        Bytes::from(read),
                        &static_file.content_type,
                    );
                    state.put_resource_cached(
                        cache_key,
                        data.clone(),
                        static_file.content_type.clone(),
                    );
                    state.clear_negative_cache(&negative_key);
                    return cacheable_binary_response(
                        data,
                        &static_file.content_type,
                        RESOURCE_CACHE_CONTROL,
                        Some(&headers),
                    );
                }
                Err(e) => {
                    tracing::warn!("Failed to read static file {:?}: {}", static_file.path, e);
                }
            }
        } else if let Some(resp) = stream_file_response(
            &static_file.path,
            &static_file.content_type,
            RESOURCE_CACHE_CONTROL,
        )
        .await
        {
            state.clear_negative_cache(&negative_key);
            return resp;
        }
    }

    state.put_negative_cache(negative_key);
    not_found()
}

/// 在词典文件夹内安全解析资源路径，返回可读取的文件路径。
///
/// 规则：
/// - 拒绝含 `..` 的路径、绝对路径、空路径（防路径穿越）。
/// - `\` 分隔符规范化为 `/`。
/// - 最终候选文件 canonicalize 后必须仍位于词典文件夹内（防符号链接逃逸）。
///
/// 命中（文件存在）返回 `Some(PathBuf)`；否则返回 `None`。
fn resolve_dict_folder_file(folder: &FsPath, request_path: &str) -> Option<PathBuf> {
    let raw = request_path.trim().trim_start_matches(['/', '\\']);
    if raw.is_empty() {
        return None;
    }
    // 拒绝遍历与绝对路径
    if raw.contains("..") || raw.starts_with(['/', '\\']) {
        return None;
    }
    let rel = raw.replace('\\', "/");
    let candidate = folder.join(&rel);

    // 只接受存在的常规文件
    if !candidate.is_file() {
        return None;
    }

    // canonicalize 后校验仍位于词典文件夹内
    let canonical_folder = folder.canonicalize().ok()?;
    let canonical_candidate = candidate.canonicalize().ok()?;
    if canonical_candidate.starts_with(&canonical_folder) {
        Some(canonical_candidate)
    } else {
        tracing::warn!(
            "Rejected dict folder resource escaping folder: {:?} not under {:?}",
            canonical_candidate,
            canonical_folder
        );
        None
    }
}

/// 读取词典文件夹资源并返回响应（CSS 经词典作用域化后缓存）。返回 `None`
/// 表示文件读取失败（调用方应继续尝试下一个候选目录）。
async fn serve_dict_folder_resource(
    state: &AppState,
    dict_id: &str,
    folder_file: PathBuf,
    request_path: &str,
    cache_key: &str,
    negative_key: &str,
    headers: &HeaderMap,
) -> Option<Response> {
    let content_type = detect_content_type(request_path);
    match fs::read(&folder_file).await {
        Ok(read) => {
            // CSS 作用域化与缓存策略与 MDD 内资源一致。
            let data = scope_dict_css_if_css(dict_id, Bytes::from(read), &content_type);
            if should_cache_resource(&content_type, data.len()) {
                state.put_resource_cached(
                    cache_key.to_string(),
                    data.clone(),
                    content_type.clone(),
                );
            }
            state.clear_negative_cache(negative_key);
            Some(cacheable_binary_response(
                data,
                &content_type,
                RESOURCE_CACHE_CONTROL,
                Some(headers),
            ))
        }
        Err(e) => {
            tracing::warn!(
                "Failed to read dict folder resource {:?}: {}",
                folder_file,
                e
            );
            None
        }
    }
}

/// Build the CSS scope selector for a dictionary section's body.
///
/// Dictionary-bundled CSS served from `/dict/{id}/res/*.css` is rewritten so
/// every top-level style-rule selector is prefixed with this scope, preventing
/// leakage into the app shell and sibling dictionary sections. The selector
/// matches the `[data-dict-id="…"] .mdict-dict-body` wrapper emitted by
/// [`mdict_core::presenter::render_aggregate_html`].
///
/// `@media` / `@supports` / `@container` blocks recurse (inner rules scoped);
/// global at-rules (`@font-face`, `@keyframes`, `@page`, …) and statement
/// at-rules (`@import`, `@charset`, `@namespace`) pass through untouched.
fn dict_css_scope(dict_id: &str) -> String {
    // Escape backslash and double-quote for safe embedding inside a CSS
    // attribute-selector quoted value. dict_ids are normally plain slugs, but
    // this keeps the selector well-formed for arbitrary identifiers.
    let escaped = dict_id.replace('\\', "\\\\").replace('"', "\\\"");
    format!("[data-dict-id=\"{}\"] .mdict-dict-body", escaped)
}

/// If `content_type` is CSS, return `data` with every top-level selector
/// scoped under the per-dictionary scope *and* every `url(...)` reference
/// rewritten to this dict's `/dict/{id}/res/...` route; otherwise return
/// `data` unchanged. Non-UTF-8 CSS (effectively unheard of) is left untouched
/// rather than risk a lossy rewrite.
///
/// Both transforms run exactly once, before the bytes enter the resource
/// cache, so cache hits return already-scoped + already-rewritten bytes
/// verbatim. Neither transform is idempotent on its own output (re-scoping
/// would double-prefix selectors; re-rewriting a `/dict/...` url would
/// re-root it), so we never call this on already-processed bytes — the
/// raw MDD CSS only ever contains relative `url()` references.
fn scope_dict_css_if_css(dict_id: &str, data: Bytes, content_type: &str) -> Bytes {
    if !content_type.starts_with("text/css") {
        return data;
    }
    match std::str::from_utf8(&data) {
        Ok(css) => {
            let scoped = scope_css(css, &dict_css_scope(dict_id));
            Bytes::from(rewrite_css_urls(&scoped, dict_id))
        }
        Err(_) => data,
    }
}

fn build_resource_candidates(path: &str) -> Vec<String> {
    let normalized = path
        .trim()
        .trim_start_matches('/')
        .trim_start_matches('\\')
        .replace('\\', "/");

    if normalized.is_empty() {
        return Vec::new();
    }

    let mut candidates = Vec::with_capacity(4);
    push_unique_candidate(&mut candidates, normalized.clone());
    push_prefixed_candidate(&mut candidates, '/', &normalized);

    let backslash_form = if normalized.contains('/') {
        normalized.replace('/', "\\")
    } else {
        normalized.clone()
    };
    push_unique_candidate(&mut candidates, backslash_form.clone());
    push_prefixed_candidate(&mut candidates, '\\', &backslash_form);

    candidates
}

fn push_unique_candidate(candidates: &mut Vec<String>, candidate: String) {
    if candidate.is_empty() || candidates.iter().any(|existing| existing == &candidate) {
        return;
    }
    candidates.push(candidate);
}

fn push_prefixed_candidate(candidates: &mut Vec<String>, prefix: char, value: &str) {
    let mut candidate = String::with_capacity(value.len() + 1);
    candidate.push(prefix);
    candidate.push_str(value);
    push_unique_candidate(candidates, candidate);
}

struct StaticFileRef {
    path: PathBuf,
    content_type: String,
    size: usize,
}

fn should_cache_resource(content_type: &str, size: usize) -> bool {
    let is_media = content_type.starts_with("image/")
        || content_type.starts_with("audio/")
        || content_type.starts_with("video/");
    let limit = if is_media {
        MAX_CACHEABLE_MEDIA_BYTES
    } else {
        MAX_CACHEABLE_RESOURCE_BYTES
    };
    size <= limit
}

async fn resolve_static_file(
    base_static_dir: &FsPath,
    relative_path: &str,
    index_for_directory: bool,
) -> Option<StaticFileRef> {
    let normalized = relative_path.trim().replace('\\', "/");
    if normalized.contains("..") {
        tracing::warn!("Rejected static path traversal attempt: {}", relative_path);
        return None;
    }

    let mut static_file = base_static_dir.to_path_buf();
    let relative_path = normalized.trim_start_matches('/');

    if relative_path.is_empty() {
        if index_for_directory {
            static_file.push("index.html");
        } else {
            return None;
        }
    } else if index_for_directory && relative_path.ends_with('/') {
        static_file.push(relative_path);
        static_file.push("index.html");
    } else {
        static_file.push(relative_path);
    }

    // Security: verify canonical path remains under static root when possible.
    // Use tokio::fs::canonicalize to avoid blocking the async runtime.
    if let (Ok(canonical), Ok(base_canonical)) = (
        fs::canonicalize(&static_file).await,
        fs::canonicalize(base_static_dir).await,
    ) {
        if !canonical.starts_with(&base_canonical) {
            tracing::warn!("Path escape attempt blocked: {:?}", static_file);
            return None;
        }
    }

    let metadata = match fs::metadata(&static_file).await {
        Ok(meta) if meta.is_file() => meta,
        _ => return None,
    };

    let size = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
    let mime_type = mime_guess::from_path(&static_file)
        .first_or_octet_stream()
        .essence_str()
        .to_string();

    Some(StaticFileRef {
        path: static_file,
        content_type: mime_type,
        size,
    })
}

fn cache_key_aggregate_entry(word: &str, filter: &DictFilter) -> String {
    let mut key = String::with_capacity("entry:aggregate:".len() + word.len() + 32);
    key.push_str("entry:aggregate:");
    push_trimmed_lowercase(&mut key, word);
    if let Some(ids) = filter {
        // Deterministic key: sort dict IDs so {a,b} and {b,a} produce the same key.
        let mut sorted: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        sorted.sort_unstable();
        key.push(':');
        key.push_str(&sorted.join(","));
    }
    key
}

fn cache_key_dict_entry(dict_id: &str, word: &str) -> String {
    let mut key = String::with_capacity("entry:dict:".len() + dict_id.len() + word.len() + 1);
    key.push_str("entry:dict:");
    push_trimmed_lowercase(&mut key, dict_id);
    key.push(':');
    push_trimmed_lowercase(&mut key, word);
    key
}

fn cache_key_global_resource(path: &str) -> String {
    let trimmed = path.trim();
    let mut key = String::with_capacity("resource:global:".len() + trimmed.len());
    key.push_str("resource:global:");
    key.push_str(trimmed);
    key
}

fn cache_key_dict_resource(dict_id: &str, path: &str) -> String {
    let trimmed_path = path.trim();
    let mut key =
        String::with_capacity("resource:dict:".len() + dict_id.len() + trimmed_path.len() + 1);
    key.push_str("resource:dict:");
    push_trimmed_lowercase(&mut key, dict_id);
    key.push(':');
    key.push_str(trimmed_path);
    key
}

fn negative_key(cache_key: &str) -> String {
    let mut key = String::with_capacity("negative:".len() + cache_key.len());
    key.push_str("negative:");
    key.push_str(cache_key);
    key
}

fn push_trimmed_lowercase(buf: &mut String, value: &str) {
    for ch in value.trim().chars() {
        for lowered in ch.to_lowercase() {
            buf.push(lowered);
        }
    }
}

// ============ Dictionary Config API ============

#[derive(Deserialize, Debug)]
pub struct DictQuery {
    /// Dictionary ID (stable hash from /api/dicts, path is still accepted for compatibility)
    pub id: Option<String>,
}

#[derive(Serialize)]
pub struct DictIndexStatus {
    pub id: String,
    pub name: String,
    pub file: String,
    pub db_exists: bool,
    pub up_to_date: bool,
    pub has_fts: bool,
    pub fts_enabled: bool,
    /// 后台构建索引过程中（重试仍失败）的最末失败原因；一切就绪时为 None。
    /// 前端 setup 页可据此提示“词典 X 建立失败：原因”，避免静默 pending。
    pub last_error: Option<String>,
    /// 索引后台已试过的总次数（含首轮）。0 表示尚无任何失败被记录。
    pub index_attempts: u32,
}

/// GET /api/dicts - Get list of all dictionaries with their configs
pub(crate) async fn handle_dict_list(State(state): State<AppState>) -> Json<Vec<DictInfo>> {
    Json(state.get_all_dict_info())
}

/// GET /api/index/status - Get index/FTS status for dictionaries
pub(crate) async fn handle_index_status(
    State(state): State<AppState>,
) -> Json<Vec<DictIndexStatus>> {
    let mut items = Vec::new();
    for file in state.dict_text_files() {
        let status = match index_status(&file) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("failed to read index status for {:?}: {}", file, e);
                continue;
            }
        };
        let id = state
            .get_dict_id(&file)
            .unwrap_or_else(|| file.to_string_lossy().to_string());
        let fts_enabled = state
            .get_dict_config(&id)
            .map(|cfg| cfg.is_fts_enabled())
            .unwrap_or(true);

        let (last_error, index_attempts) = state
            .get_index_failure(&file)
            .map(|f| (Some(f.error), f.attempts))
            .unwrap_or((None, 0));
        items.push(DictIndexStatus {
            id,
            name: state.get_dict_display_name(&file),
            file: file.to_string_lossy().to_string(),
            db_exists: status.db_exists,
            up_to_date: status.up_to_date,
            has_fts: status.has_fts,
            fts_enabled,
            last_error,
            index_attempts,
        });
    }
    Json(items)
}

/// GET /api/dict/style?id=xxx - Get custom CSS for a dictionary
pub(crate) async fn handle_dict_style(
    State(state): State<AppState>,
    Query(params): Query<DictQuery>,
) -> Result<Response, AppError> {
    let id = params
        .id
        .ok_or_else(|| AppError::BadRequest("Missing 'id' parameter".to_string()))?;

    if let Some(config) = state.get_dict_config(&id) {
        let css_content = config.get_css_content(state.dict_dir());
        return Ok(css_response(css_content));
    }

    tracing::warn!("Dictionary config not found for id: {}", id);
    Err(AppError::NotFound)
}

/// GET /api/dict/script?id=xxx - Get custom JavaScript for a dictionary
pub(crate) async fn handle_dict_script(
    State(state): State<AppState>,
    Query(params): Query<DictQuery>,
) -> Result<Response, AppError> {
    let id = params
        .id
        .ok_or_else(|| AppError::BadRequest("Missing 'id' parameter".to_string()))?;

    if let Some(config) = state.get_dict_config(&id) {
        let js_content = config.get_js_content(state.dict_dir());
        return Ok(js_response(js_content));
    }

    tracing::warn!("Dictionary config not found for id: {}", id);
    Err(AppError::NotFound)
}
