mod error;
mod response;

pub use error::AppError;
use response::{ok_response, css_response, js_response, not_found};

use crate::lucky;
use crate::query::{query, query_with_trace, suggest};
use crate::config::{static_path, get_all_dict_info, get_dict_config, get_dict_directory, DictInfo};
use serde_derive::Deserialize;

use axum::{extract::{Form, Query}, response::{Response, Json}, http::Uri};
use tokio::fs;

#[derive(Deserialize, Debug)]
pub struct SuggestQuery {
    q: String,
}

#[derive(Deserialize, Debug)]
pub struct QueryForm {
    word: String,
}

pub(crate) async fn handle_query(Form(params): Form<QueryForm>) -> Result<Response, AppError> {
    tracing::info!("Processing query for word: {}", params.word);
    let (data, content_type) = query(params.word)?;
    Ok(ok_response(data, &content_type))
}

pub(crate) async fn handle_lucky() -> Result<Response, AppError> {
    let word = lucky::lucky_word();
    tracing::info!("Lucky query for word: {}", word);
    let (data, content_type) = query(word)?;
    Ok(ok_response(data, &content_type))
}

pub(crate) async fn handle_suggest(Query(params): Query<SuggestQuery>) -> Json<Vec<String>> {
    match suggest(params.q, 10) {
        Ok(suggestions) => Json(suggestions),
        Err(e) => {
            tracing::warn!("Suggest failed: {}", e);
            Json(vec![])
        }
    }
}

/// Debug endpoint to show @@@LINK redirect chain
/// Usage: GET /trace?word=whams
pub(crate) async fn handle_trace(Query(params): Query<SuggestQuery>) -> Json<serde_json::Value> {
    match query_with_trace(params.q) {
        Ok((chain, final_word)) => Json(serde_json::json!({
            "chain": chain,
            "depth": chain.len() - 1,
            "final_word": final_word,
        })),
        Err(e) => Json(serde_json::json!({
            "error": e,
        })),
    }
}

pub(crate) async fn handle_resource(uri: Uri) -> Response {
    let path = uri.path();
    let key = path.replace("/", "\\"); // standard mdict key starts with \ e.g. \img\foo.png

    // Candidate keys to try
    let candidates = vec![
        key.clone(),                              // \img\foo.png
        path.to_string(),                         // /img/foo.png
        path.trim_start_matches('/').to_string(), // img/foo.png
    ];

    for candidate in &candidates {
        tracing::debug!("resource try key: {}", candidate);
        if let Ok((data, content_type)) = query(candidate.clone()) {
            return ok_response(data, &content_type);
        }
    }

    // 2. Try to find in file system (Static Resources)
    if let Ok(base_static_dir) = static_path() {
        let relative_path = path.trim_start_matches('/');

        // Security: Reject paths with directory traversal patterns
        if relative_path.contains("..") {
            tracing::warn!("Rejected path traversal attempt: {}", path);
            return not_found();
        }

        let mut static_dir = base_static_dir.clone();
        if relative_path.is_empty() || relative_path.ends_with('/') {
            static_dir.push(relative_path);
            static_dir.push("index.html");
        } else {
            static_dir.push(relative_path);
        }

        // Security: Verify the final path is within static directory
        if let Ok(canonical) = static_dir.canonicalize() {
            if let Ok(base_canonical) = base_static_dir.canonicalize() {
                if !canonical.starts_with(&base_canonical) {
                    tracing::warn!("Path escape attempt blocked: {:?}", static_dir);
                    return not_found();
                }
            }
        }

        if static_dir.exists() && static_dir.is_file() {
            match fs::read(&static_dir).await {
                Ok(bytes) => {
                    let mime_type = mime_guess::from_path(&static_dir).first_or_octet_stream();
                    return ok_response(bytes, mime_type.as_ref());
                }
                Err(e) => {
                    tracing::warn!("Failed to read static file {:?}: {}", static_dir, e);
                }
            }
        }
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
pub(crate) async fn handle_dict_list() -> Json<Vec<DictInfo>> {
    Json(get_all_dict_info())
}

/// GET /api/dict/style?id=xxx - Get custom CSS for a dictionary
pub(crate) async fn handle_dict_style(Query(params): Query<DictQuery>) -> Result<Response, AppError> {
    let id = params.id.ok_or_else(|| AppError::BadRequest("Missing 'id' parameter".to_string()))?;

    if let Some(config) = get_dict_config(&id) {
        let dict_dir = get_dict_directory();
        let css_content = config.get_css_content(&dict_dir);
        return Ok(css_response(css_content));
    }

    tracing::warn!("Dictionary config not found for id: {}", id);
    Err(AppError::NotFound)
}

/// GET /api/dict/script?id=xxx - Get custom JavaScript for a dictionary
pub(crate) async fn handle_dict_script(Query(params): Query<DictQuery>) -> Result<Response, AppError> {
    let id = params.id.ok_or_else(|| AppError::BadRequest("Missing 'id' parameter".to_string()))?;

    if let Some(config) = get_dict_config(&id) {
        let dict_dir = get_dict_directory();
        let js_content = config.get_js_content(&dict_dir);
        return Ok(js_response(js_content));
    }

    tracing::warn!("Dictionary config not found for id: {}", id);
    Err(AppError::NotFound)
}
