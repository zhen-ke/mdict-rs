use axum::{http::StatusCode, response::Response};

/// Build a successful response with content type
pub fn ok_response(data: Vec<u8>, content_type: &str) -> Response {
    axum::http::Response::builder()
        .header("Content-Type", content_type)
        .body(data.into())
        .unwrap()
}

/// Build a CSS response
pub fn css_response(content: String) -> Response {
    axum::http::Response::builder()
        .header("Content-Type", "text/css; charset=utf-8")
        .body(content.into())
        .unwrap()
}

/// Build a JavaScript response
pub fn js_response(content: String) -> Response {
    axum::http::Response::builder()
        .header("Content-Type", "application/javascript; charset=utf-8")
        .body(content.into())
        .unwrap()
}

/// Build a 404 Not Found response
pub fn not_found() -> Response {
    axum::http::Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header("Content-Type", "text/plain")
        .body("Not Found".into())
        .unwrap()
}
