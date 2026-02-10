mod error;
mod response;

pub use error::AppError;
use response::{css_response, js_response, not_found, ok_response};

use crate::app_state::AppState;
use crate::config::DictInfo;
use crate::lucky;
use crate::query::{
    query, query_specific_entry, query_specific_resource, query_with_trace, suggest,
};
use serde_derive::Deserialize;
use std::collections::HashSet;
use std::path::{Path as FsPath, PathBuf};

use axum::{
    extract::{Form, Path, Query, State},
    http::Uri,
    response::{Json, Response},
};
use tokio::fs;

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
    let word = params.word;
    tracing::info!("Processing query for word: {}", word);
    let state = state.clone();
    let result = tokio::task::spawn_blocking(move || query(&state, word))
        .await
        .map_err(|e| AppError::Internal(format!("query task failed: {}", e)))?;
    let (data, content_type) = result.map_err(AppError::from)?;
    Ok(ok_response(data, &content_type))
}

pub(crate) async fn handle_lucky(State(state): State<AppState>) -> Result<Response, AppError> {
    let word = lucky::lucky_word();
    tracing::info!("Lucky query for word: {}", word);
    let state = state.clone();
    let result = tokio::task::spawn_blocking(move || query(&state, word))
        .await
        .map_err(|e| AppError::Internal(format!("query task failed: {}", e)))?;
    let (data, content_type) = result.map_err(AppError::from)?;
    Ok(ok_response(data, &content_type))
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
            "error": e,
        })),
        Err(e) => Json(serde_json::json!({
            "error": format!("trace task failed: {}", e),
        })),
    }
}

pub(crate) async fn handle_resource(State(state): State<AppState>, uri: Uri) -> Response {
    let path = uri.path();
    if let Some(response) =
        read_static_file_response(state.static_dir(), path.trim_start_matches('/'), true).await
    {
        return response;
    }

    // 2. Fallback to unscoped dictionary resources (legacy behavior).
    let candidates = build_resource_candidates(path);
    if candidates.is_empty() {
        return not_found();
    }

    let state = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        for candidate in &candidates {
            tracing::debug!("resource try key: {}", candidate);
            if let Ok((data, content_type)) = query(&state, candidate.clone()) {
                return Some(ok_response(data, &content_type));
            }
        }
        None
    })
    .await;

    match result {
        Ok(Some(response)) => response,
        Ok(None) => not_found(),
        Err(e) => {
            tracing::warn!("Resource query task failed: {}", e);
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

    query_dict_entry(&state, dict_id, files, candidates).await
}

/// GET /dict/{id}/res/{*path}
pub(crate) async fn handle_dict_res(
    State(state): State<AppState>,
    Path((dict_id, path)): Path<(String, String)>,
) -> Response {
    query_dict_resource(state, dict_id, path).await
}

/// GET /dict/{id}/audio/{*path}
pub(crate) async fn handle_dict_audio(
    State(state): State<AppState>,
    Path((dict_id, path)): Path<(String, String)>,
) -> Response {
    query_dict_resource(state, dict_id, path).await
}

/// Legacy route compatibility: GET /resource/{id}/{*path}
pub(crate) async fn handle_dict_resource(
    State(state): State<AppState>,
    Path((dict_id, path)): Path<(String, String)>,
) -> Response {
    query_dict_resource(state, dict_id, path).await
}

async fn query_dict_resource(state: AppState, dict_id: String, path: String) -> Response {
    if path.contains("..") {
        tracing::warn!("Rejected dict resource traversal attempt: {}", path);
        return not_found();
    }

    let files = state.get_dict_resource_files_by_id(&dict_id);
    if files.is_empty() {
        return not_found();
    }

    let candidates = build_resource_candidates(&path);
    if candidates.is_empty() {
        return match read_static_file_response(state.static_dir(), &path, false).await {
            Some(resp) => resp,
            None => not_found(),
        };
    }

    let static_dir = state.static_dir().to_path_buf();
    let state = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        for file in files {
            for candidate in &candidates {
                if let Ok(Some((data, content_type))) =
                    query_specific_resource(&state, &file, candidate)
                {
                    return Some(ok_response(data, &content_type));
                }
            }
        }
        None
    })
    .await;

    match result {
        Ok(Some(resp)) => resp,
        Ok(None) => match read_static_file_response(&static_dir, &path, false).await {
            Some(resp) => resp,
            None => not_found(),
        },
        Err(e) => {
            tracing::warn!("dict resource query task failed: {}", e);
            match read_static_file_response(&static_dir, &path, false).await {
                Some(resp) => resp,
                None => not_found(),
            }
        }
    }
}

async fn query_dict_entry(
    state: &AppState,
    dict_id: String,
    files: Vec<PathBuf>,
    candidates: Vec<String>,
) -> Response {
    let state = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        for file in files {
            for candidate in &candidates {
                if let Ok(Some((data, content_type))) =
                    query_specific_entry(&state, &file, candidate, &dict_id)
                {
                    return Some(ok_response(data, &content_type));
                }
            }
        }
        None
    })
    .await;

    match result {
        Ok(Some(resp)) => resp,
        Ok(None) => not_found(),
        Err(e) => {
            tracing::warn!("dict entry query task failed: {}", e);
            not_found()
        }
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

async fn read_static_file_response(
    base_static_dir: &FsPath,
    relative_path: &str,
    index_for_directory: bool,
) -> Option<Response> {
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
            let mime_type = mime_guess::from_path(&static_file).first_or_octet_stream();
            Some(ok_response(bytes, mime_type.as_ref()))
        }
        Err(e) => {
            tracing::warn!("Failed to read static file {:?}: {}", static_file, e);
            None
        }
    }
}

// ============ Dictionary Config API ============

#[derive(Deserialize, Debug)]
pub struct DictQuery {
    /// Dictionary ID (file path) or name
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
