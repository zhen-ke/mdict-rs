use crate::query::QueryError;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

/// Application-level errors that can be converted to HTTP responses
#[derive(Debug)]
pub enum AppError {
    /// Entry not found in any dictionary
    NotFound,
    /// Missing required parameter
    BadRequest(String),
    /// Service is temporarily overloaded
    Overloaded,
    /// Internal server error
    Internal(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::NotFound => write!(f, "Not found"),
            AppError::BadRequest(msg) => write!(f, "Bad request: {}", msg),
            AppError::Overloaded => write!(f, "Service temporarily overloaded"),
            AppError::Internal(msg) => write!(f, "Internal error: {}", msg),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "Not found".to_string()),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::Overloaded => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Service temporarily overloaded, please retry.".to_string(),
            ),
            AppError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
        };

        tracing::warn!("Request error: {}", self);

        axum::http::Response::builder()
            .status(status)
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(message.into())
            .unwrap()
    }
}

/// Convert query module errors to AppError
impl From<QueryError> for AppError {
    fn from(err: QueryError) -> Self {
        match err {
            QueryError::NotFound => AppError::NotFound,
            QueryError::InvalidInput(msg) => AppError::BadRequest(msg),
            QueryError::TooManyRedirects => AppError::Internal("Too many redirects".to_string()),
            QueryError::Internal(msg) => AppError::Internal(msg),
        }
    }
}

/// Convert std::io::Error to AppError
impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::Internal(err.to_string())
    }
}
