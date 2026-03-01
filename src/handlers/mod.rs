mod error;
mod response;

pub use error::AppError;
use response::{cacheable_binary_response, css_response, js_response, not_found, ok_response};

use crate::app_state::AppState;
use crate::config::DictInfo;
use crate::lucky;
use crate::query::{
    QueryError, query, query_aggregate, query_specific_entry, query_specific_resource,
    query_with_trace, suggest,
};
use serde_derive::Deserialize;
use std::collections::HashSet;
use std::path::Path as FsPath;

use axum::{
    body::Bytes,
    extract::{Form, Path, Query, State},
    http::{HeaderMap, Uri},
    response::{Json, Response},
};
use tokio::fs;

const RESOURCE_CACHE_CONTROL: &str = "public, max-age=86400, immutable";

#[derive(Deserialize, Debug)]
pub struct SuggestQuery {
    q: String,
}

#[derive(Deserialize, Debug)]
pub struct QueryForm {
    word: String,
}

pub(crate) async fn handle_query(
    State(state): State<AppState>,
    Form(params): Form<QueryForm>,
) -> Result<Response, AppError> {
    let word = params.word.trim().to_string();
    tracing::info!("Processing query for word: {}", word);

    let cache_key = cache_key_aggregate_entry(&word);
    let negative_key = negative_key(&cache_key);
    if let Some((data, content_type)) = state.get_entry_cached(&cache_key) {
        return Ok(ok_response(data, &content_type));
    }
    if state.is_negative_cached(&negative_key) {
        return Err(AppError::NotFound);
    }

    let query_state = state.clone();
    let query_word = word.clone();
    let result = tokio::task::spawn_blocking(move || query_aggregate(&query_state, query_word))
        .await
        .map_err(|e| AppError::Internal(format!("query task failed: {}", e)))?;

    match result {
        Ok((data, content_type)) => {
            let data = Bytes::from(data);
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

pub(crate) async fn handle_lucky(State(state): State<AppState>) -> Result<Response, AppError> {
    let word = lucky::lucky_word();
    tracing::info!("Lucky query for word: {}", word);

    let cache_key = cache_key_aggregate_entry(&word);
    let negative_key = negative_key(&cache_key);
    if let Some((data, content_type)) = state.get_entry_cached(&cache_key) {
        return Ok(ok_response(data, &content_type));
    }
    if state.is_negative_cached(&negative_key) {
        return Err(AppError::NotFound);
    }

    let query_state = state.clone();
    let query_word = word.clone();
    let result = tokio::task::spawn_blocking(move || query_aggregate(&query_state, query_word))
        .await
        .map_err(|e| AppError::Internal(format!("query task failed: {}", e)))?;

    match result {
        Ok((data, content_type)) => {
            let data = Bytes::from(data);
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

pub(crate) async fn handle_suggest(
    State(state): State<AppState>,
    Query(params): Query<SuggestQuery>,
) -> Json<Vec<String>> {
    let q = params.q;
    let state = state.clone();
    let result = tokio::task::spawn_blocking(move || suggest(&state, q, 10)).await;
    match result {
        Ok(Ok(suggestions)) => Json(suggestions),
        Ok(Err(e)) => {
            tracing::warn!("Suggest failed: {}", e);
            Json(vec![])
        }
        Err(e) => {
            tracing::warn!("Suggest task failed: {}", e);
            Json(vec![])
        }
    }
}

/// Debug endpoint to show @@@LINK redirect chain
/// Usage: GET /trace?q=whams
pub(crate) async fn handle_trace(
    State(state): State<AppState>,
    Query(params): Query<SuggestQuery>,
) -> Json<serde_json::Value> {
    let q = params.q;
    let state = state.clone();
    let result = tokio::task::spawn_blocking(move || query_with_trace(&state, q)).await;
    match result {
        Ok(Ok((chain, final_word))) => Json(serde_json::json!({
            "chain": chain,
            "depth": chain.len() - 1,
            "final_word": final_word,
        })),
        Ok(Err(e)) => Json(serde_json::json!({
            "error": e.to_string(),
        })),
        Err(e) => Json(serde_json::json!({
            "error": format!("trace task failed: {}", e),
        })),
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

    if let Some((data, content_type)) =
        read_static_file(state.static_dir(), path.trim_start_matches('/'), true).await
    {
        let data = Bytes::from(data);
        state.put_resource_cached(cache_key, data.clone(), content_type.clone());
        state.clear_negative_cache(&negative_key);
        return cacheable_binary_response(
            data,
            &content_type,
            RESOURCE_CACHE_CONTROL,
            Some(&headers),
        );
    }

    let candidates = build_resource_candidates(path);
    if candidates.is_empty() {
        state.put_negative_cache(negative_key);
        return not_found();
    }

    let query_state = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        for candidate in &candidates {
            tracing::debug!("resource try key: {}", candidate);
            if let Ok((data, content_type)) = query(&query_state, candidate.clone()) {
                return Some((data, content_type));
            }
        }
        None
    })
    .await;

    match result {
        Ok(Some((data, content_type))) => {
            let data = Bytes::from(data);
            state.put_resource_cached(cache_key, data.clone(), content_type.clone());
            state.clear_negative_cache(&negative_key);
            cacheable_binary_response(data, &content_type, RESOURCE_CACHE_CONTROL, Some(&headers))
        }
        Ok(None) => {
            state.put_negative_cache(negative_key);
            not_found()
        }
        Err(e) => {
            tracing::warn!("Resource query task failed: {}", e);
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

    let candidates = build_entry_candidates(&word);
    if candidates.is_empty() {
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

    let state_cloned = state.clone();
    let dict_id_for_query = dict_id.clone();
    let result = tokio::task::spawn_blocking(move || {
        for file in files {
            for candidate in &candidates {
                if let Ok(Some((data, content_type))) =
                    query_specific_entry(&state_cloned, &file, candidate, &dict_id_for_query)
                {
                    return Some((data, content_type));
                }
            }
        }
        None
    })
    .await;

    match result {
        Ok(Some((data, content_type))) => {
            let data = Bytes::from(data);
            state.put_entry_cached(cache_key, data.clone(), content_type.clone());
            state.clear_negative_cache(&negative_key);
            ok_response(data, &content_type)
        }
        Ok(None) => {
            state.put_negative_cache(negative_key);
            not_found()
        }
        Err(e) => {
            tracing::warn!("dict entry query task failed: {}", e);
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
        let query_state = state.clone();
        let result = tokio::task::spawn_blocking(move || {
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
        .await;

        match result {
            Ok(Some((data, content_type))) => {
                let data = Bytes::from(data);
                state.put_resource_cached(cache_key, data.clone(), content_type.clone());
                state.clear_negative_cache(&negative_key);
                return cacheable_binary_response(
                    data,
                    &content_type,
                    RESOURCE_CACHE_CONTROL,
                    Some(&headers),
                );
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("dict resource query task failed: {}", e);
            }
        }
    }

    // Fallback: serve static assets for dictionary-rendered pages (e.g. lm6.css/lm6.js).
    if let Some((data, content_type)) = read_static_file(state.static_dir(), &path, false).await {
        let data = Bytes::from(data);
        state.put_resource_cached(cache_key, data.clone(), content_type.clone());
        state.clear_negative_cache(&negative_key);
        return cacheable_binary_response(
            data,
            &content_type,
            RESOURCE_CACHE_CONTROL,
            Some(&headers),
        );
    }

    state.put_negative_cache(negative_key);
    not_found()
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

    let slash_form = normalized.clone();
    let backslash_form = normalized.replace('/', "\\");

    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    for candidate in [
        slash_form.clone(),
        format!("/{}", slash_form),
        backslash_form.clone(),
        format!("\\{}", backslash_form),
    ] {
        if candidate.is_empty() || !seen.insert(candidate.clone()) {
            continue;
        }
        candidates.push(candidate);
    }

    candidates
}

fn build_entry_candidates(word: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();

    let raw = word.trim();
    let trimmed = raw
        .trim_start_matches('/')
        .trim_start_matches('\\')
        .trim_end_matches('/')
        .trim_end_matches('\\');

    for candidate in [raw, trimmed] {
        if candidate.is_empty() {
            continue;
        }
        if seen.insert(candidate.to_string()) {
            candidates.push(candidate.to_string());
        }
    }

    candidates
}

async fn read_static_file(
    base_static_dir: &FsPath,
    relative_path: &str,
    index_for_directory: bool,
) -> Option<(Vec<u8>, String)> {
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
    if let Ok(canonical) = static_file.canonicalize() {
        if let Ok(base_canonical) = base_static_dir.canonicalize() {
            if !canonical.starts_with(&base_canonical) {
                tracing::warn!("Path escape attempt blocked: {:?}", static_file);
                return None;
            }
        }
    }

    if !static_file.exists() || !static_file.is_file() {
        return None;
    }

    match fs::read(&static_file).await {
        Ok(bytes) => {
            let mime_type = mime_guess::from_path(&static_file)
                .first_or_octet_stream()
                .essence_str()
                .to_string();
            Some((bytes, mime_type))
        }
        Err(e) => {
            tracing::warn!("Failed to read static file {:?}: {}", static_file, e);
            None
        }
    }
}

fn cache_key_aggregate_entry(word: &str) -> String {
    format!("entry:aggregate:{}", word.trim().to_lowercase())
}

fn cache_key_dict_entry(dict_id: &str, word: &str) -> String {
    format!(
        "entry:dict:{}:{}",
        dict_id.trim().to_lowercase(),
        word.trim().to_lowercase()
    )
}

fn cache_key_global_resource(path: &str) -> String {
    format!("resource:global:{}", path.trim())
}

fn cache_key_dict_resource(dict_id: &str, path: &str) -> String {
    format!(
        "resource:dict:{}:{}",
        dict_id.trim().to_lowercase(),
        path.trim()
    )
}

fn negative_key(cache_key: &str) -> String {
    format!("negative:{}", cache_key)
}

// ============ Dictionary Config API ============

#[derive(Deserialize, Debug)]
pub struct DictQuery {
    /// Dictionary ID (stable hash from /api/dicts, path is still accepted for compatibility)
    pub id: Option<String>,
}

/// GET /api/dicts - Get list of all dictionaries with their configs
pub(crate) async fn handle_dict_list(State(state): State<AppState>) -> Json<Vec<DictInfo>> {
    Json(state.get_all_dict_info())
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
