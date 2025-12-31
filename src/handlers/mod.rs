use crate::lucky;
use crate::query::{query, query_with_trace, suggest};
use serde_derive::Deserialize;

use axum::{extract::{Path, Form, Query}, response::{Response, IntoResponse, Json}, http::{StatusCode, Uri}};
use tokio::fs;
use crate::config::static_path;

#[derive(Deserialize, Debug)]
pub struct SuggestQuery {
    q: String,
}

#[derive(Deserialize, Debug)]
pub struct QueryForm {
    word: String,
}

pub(crate) async fn handle_query(Form(params): Form<QueryForm>) -> Response {
    match query(params.word) {
        Ok((data, content_type)) => axum::http::Response::builder()
            .header("Content-Type", content_type)
            .body(data.into())
            .unwrap(),
        Err(e) => {
            // 区分 "not found" 和其他错误
            let status = if e == "not found" { 404 } else { 500 };
            axum::http::Response::builder()
                .status(status)
                .body(e.into())
                .unwrap()
        }
    }
}

pub(crate) async fn handle_lucky() -> Response {
    let word = lucky::lucky_word();
    match query(word) {
        Ok((data, content_type)) => axum::http::Response::builder()
            .header("Content-Type", content_type)
            .body(data.into())
            .unwrap(),
        Err(e) => axum::http::Response::builder()
            .status(500)
            .body(e.into())
            .unwrap(),
    }
}

pub(crate) async fn handle_suggest(Query(params): Query<SuggestQuery>) -> Json<Vec<String>> {
    match suggest(params.q, 10) {
        Ok(suggestions) => Json(suggestions),
        Err(_) => Json(vec![]),
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
        key.clone(),                       // \img\foo.png
        path.to_string(),                  // /img/foo.png
        path.trim_start_matches('/').to_string(), // img/foo.png
    ];

    for candidate in candidates {
        tracing::info!("resource try key: {}", candidate);
        if let Ok((data, content_type)) = query(candidate) {
            return axum::http::Response::builder()
                .header("Content-Type", content_type)
                .body(data.into())
                .unwrap();
        }
    }

    // Attempt with different prefix if needed? e.g. key without leading slash
    // ...

    // 2. Try to find in file system (Static Resources)
    // Map URI to file path
    if let Ok(mut static_path) = static_path() {
        // Remove leading slash to append
        let mut relative_path = path.trim_start_matches('/');
        if relative_path.is_empty() || relative_path.ends_with('/') {
            static_path.push(relative_path);
            static_path.push("index.html");
        } else {
            static_path.push(relative_path);
        }

        if static_path.exists() && static_path.is_file() {
             match fs::read(&static_path).await {
                Ok(bytes) => {
                    // Guess mime type
                    let mime_type = mime_guess::from_path(&static_path).first_or_octet_stream();
                    return axum::http::Response::builder()
                        .header("Content-Type", mime_type.as_ref())
                        .body(bytes.into())
                        .unwrap();
                }
                Err(_) => {}
             }
        }
    }

    axum::http::Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body("Not Found".into())
        .unwrap()
}
