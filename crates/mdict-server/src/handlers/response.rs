use axum::{
    body::{Body, Bytes},
    http::{
        HeaderMap, StatusCode,
        header::{
            ACCEPT_RANGES, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, ETAG, IF_NONE_MATCH, RANGE,
        },
    },
    response::Response,
};
use std::path::Path;
use tokio::fs::File;
use tokio_util::io::ReaderStream;

/// Build a successful response with content type
pub fn ok_response(data: impl Into<Bytes>, content_type: &str) -> Response {
    let data: Bytes = data.into();
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

/// Build a 503 Service Unavailable response for temporary overload.
pub fn service_unavailable() -> Response {
    axum::http::Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("Content-Type", "text/plain; charset=utf-8")
        .header("Retry-After", "1")
        .body("Service temporarily overloaded, please retry.".into())
        .unwrap()
}

/// Build a cacheable binary response with optional ETag revalidation and byte-range support.
pub fn cacheable_binary_response(
    data: impl Into<Bytes>,
    content_type: &str,
    cache_control: &str,
    request_headers: Option<&HeaderMap>,
) -> Response {
    let data: Bytes = data.into();
    let etag = build_etag(&data);
    let total_len = data.len();

    let if_none_match = request_headers
        .and_then(|h| h.get(IF_NONE_MATCH))
        .and_then(|v| v.to_str().ok());
    if let Some(if_none_match) = if_none_match
        && etag_matches(if_none_match, &etag)
    {
        return axum::http::Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(ETAG, etag)
            .header(CACHE_CONTROL, cache_control)
            .header(ACCEPT_RANGES, "bytes")
            .body("".into())
            .unwrap();
    }

    let range_header = request_headers
        .and_then(|h| h.get(RANGE))
        .and_then(|v| v.to_str().ok());
    if let Some(range_header) = range_header
        && range_header.trim().starts_with("bytes=")
    {
        return match parse_single_range(range_header, total_len) {
            Some((start, end)) => {
                let chunk = data.slice(start..(end + 1));
                let chunk_len = chunk.len();
                axum::http::Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header("Content-Type", content_type)
                    .header(CONTENT_LENGTH, chunk_len.to_string())
                    .header(
                        CONTENT_RANGE,
                        format!("bytes {}-{}/{}", start, end, total_len),
                    )
                    .header(ACCEPT_RANGES, "bytes")
                    .header(ETAG, etag)
                    .header(CACHE_CONTROL, cache_control)
                    .body(chunk.into())
                    .unwrap()
            }
            None => axum::http::Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(CONTENT_RANGE, format!("bytes */{}", total_len))
                .header(ACCEPT_RANGES, "bytes")
                .header(ETAG, etag)
                .header(CACHE_CONTROL, cache_control)
                .body("".into())
                .unwrap(),
        };
    }

    axum::http::Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", content_type)
        .header(CONTENT_LENGTH, total_len.to_string())
        .header(ACCEPT_RANGES, "bytes")
        .header(ETAG, etag)
        .header(CACHE_CONTROL, cache_control)
        .body(data.into())
        .unwrap()
}

/// Build a streaming response from a filesystem file.
pub async fn stream_file_response(
    path: &Path,
    content_type: &str,
    cache_control: &str,
) -> Option<Response> {
    let file = File::open(path).await.ok()?;
    let total_len = file.metadata().await.ok()?.len();
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    Some(
        axum::http::Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", content_type)
            .header(CONTENT_LENGTH, total_len.to_string())
            .header(CACHE_CONTROL, cache_control)
            .body(body)
            .unwrap(),
    )
}

fn build_etag(data: &[u8]) -> String {
    let checksum = adler32::adler32(data).unwrap_or(0);
    format!(r#"W/"{:x}-{:x}""#, data.len(), checksum)
}

fn etag_matches(if_none_match: &str, etag: &str) -> bool {
    let expected = strip_weak_prefix(etag.trim());
    if_none_match.split(',').any(|candidate| {
        let candidate = candidate.trim();
        candidate == "*" || strip_weak_prefix(candidate) == expected
    })
}

fn strip_weak_prefix(tag: &str) -> &str {
    tag.strip_prefix("W/").unwrap_or(tag).trim()
}

fn parse_single_range(range_header: &str, total_len: usize) -> Option<(usize, usize)> {
    if total_len == 0 {
        return None;
    }

    let range = range_header.trim().strip_prefix("bytes=")?;
    if range.contains(',') {
        // Only a single range is supported.
        return None;
    }

    let (start_s, end_s) = range.split_once('-')?;
    if start_s.is_empty() {
        // suffix-byte-range-spec: bytes=-500
        let suffix_len = end_s.parse::<usize>().ok()?;
        if suffix_len == 0 {
            return None;
        }
        let start = total_len.saturating_sub(suffix_len);
        let end = total_len - 1;
        return Some((start, end));
    }

    let start = start_s.parse::<usize>().ok()?;
    if start >= total_len {
        return None;
    }

    let end = if end_s.is_empty() {
        total_len - 1
    } else {
        end_s.parse::<usize>().ok()?.min(total_len - 1)
    };

    if end < start {
        return None;
    }
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::parse_single_range;

    #[test]
    fn parses_standard_range() {
        assert_eq!(parse_single_range("bytes=10-19", 100), Some((10, 19)));
    }

    #[test]
    fn parses_open_ended_range() {
        assert_eq!(parse_single_range("bytes=10-", 100), Some((10, 99)));
    }

    #[test]
    fn parses_suffix_range() {
        assert_eq!(parse_single_range("bytes=-20", 100), Some((80, 99)));
    }

    #[test]
    fn rejects_invalid_range() {
        assert_eq!(parse_single_range("bytes=100-200", 100), None);
    }
}
