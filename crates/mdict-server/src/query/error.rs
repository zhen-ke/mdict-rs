#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryError {
    NotFound,
    TooManyRedirects,
    InvalidInput(String),
    Internal(String),
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryError::NotFound => write!(f, "not found"),
            QueryError::TooManyRedirects => write!(f, "too many redirects"),
            QueryError::InvalidInput(msg) => write!(f, "invalid input: {}", msg),
            QueryError::Internal(msg) => write!(f, "internal error: {}", msg),
        }
    }
}

impl std::error::Error for QueryError {}
