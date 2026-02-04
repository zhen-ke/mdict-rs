mod error;
mod response;

pub use error::AppError;
use response::{css_response, js_response, not_found, ok_response};

use crate::app_state::AppState;
use crate::config::DictInfo;
use crate::lucky;
use crate::query::{query, query_with_trace, suggest};
use serde_derive::Deserialize;

use axum::{
    extract::{Form, Query, State},
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
    let key = path.replace("/", "\\"); // standard mdict key starts with \ e.g. \img\foo.png

    // 1. Try to find in file system (Static Resources)
    let base_static_dir = state.static_dir().to_path_buf();
    let relative_path = path.trim_start_matches('/');

    // Security: Reject paths with directory traversal patterns
    if relative_path.contains("..") {
        tracing::warn!("Rejected path traversal attempt: {}", path);
        return not_found();
    }

    let mut static_file = base_static_dir.clone();
    if relative_path.is_empty() || relative_path.ends_with('/') {
        static_file.push(relative_path);
        static_file.push("index.html");
    } else {
        static_file.push(relative_path);
    }

    // Security: Verify the final path is within static directory
    if let Ok(canonical) = static_file.canonicalize() {
        if let Ok(base_canonical) = base_static_dir.canonicalize() {
            if !canonical.starts_with(&base_canonical) {
                tracing::warn!("Path escape attempt blocked: {:?}", static_file);
                return not_found();
            }
        }
    }

    if static_file.exists() && static_file.is_file() {
        match fs::read(&static_file).await {
            Ok(bytes) => {
                let mime_type = mime_guess::from_path(&static_file).first_or_octet_stream();
                return ok_response(bytes, mime_type.as_ref());
            }
            Err(e) => {
                tracing::warn!("Failed to read static file {:?}: {}", static_file, e);
            }
        }
    }

    // Candidate keys to try
    let candidates = vec![
        key.clone(),                              // \img\foo.png
        path.to_string(),                         // /img/foo.png
        path.trim_start_matches('/').to_string(), // img/foo.png
    ];

    let state = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        for candidate in candidates {
            tracing::debug!("resource try key: {}", candidate);
            if let Ok((data, content_type)) = query(&state, candidate) {
                return Some(ok_response(data, &content_type));
            }
        }
        None
    })
    .await;

    match result {
        Ok(Some(response)) => return response,
        Ok(None) => {}
        Err(e) => tracing::warn!("Resource query task failed: {}", e),
    }

    not_found()
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
