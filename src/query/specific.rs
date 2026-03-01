use crate::app_state::AppState;
use crate::query::{
    MAX_RESOURCE_RECORD_BYTES, QueryError, detect_content_type, extract_link_target,
    lookup_record_in_file, rewrite_html,
};
use axum::body::Bytes;
use std::path::Path;
use tracing::info;

const MAX_REDIRECT_DEPTH: u8 = 5;

/// Query a specific dictionary file for static resources (image/audio/css/js...).
pub fn query_specific_resource(
    state: &AppState,
    file: &Path,
    key: &str,
) -> Result<Option<(Bytes, String)>, QueryError> {
    let Some(data) = lookup_record_in_file(state, file, key, Some(MAX_RESOURCE_RECORD_BYTES))?
    else {
        return Ok(None);
    };
    Ok(Some((data, detect_content_type(key))))
}

/// Query an entry from a specific dictionary text file.
/// It follows @@@LINK= redirects inside the same dictionary file.
pub fn query_specific_entry(
    state: &AppState,
    file: &Path,
    word: &str,
    dict_id: &str,
) -> Result<Option<(Bytes, String)>, QueryError> {
    query_specific_entry_internal(state, file, word, dict_id, 0)
}

fn query_specific_entry_internal(
    state: &AppState,
    file: &Path,
    word: &str,
    dict_id: &str,
    depth: u8,
) -> Result<Option<(Bytes, String)>, QueryError> {
    if depth > MAX_REDIRECT_DEPTH {
        return Err(QueryError::TooManyRedirects);
    }

    let Some(mut data) = lookup_record_in_file(state, file, word, None)? else {
        return Ok(None);
    };

    if let Some(linked_word) = extract_link_target(&data) {
        info!(
            "following dict-specific @@@LINK redirect: {} -> {}",
            word, linked_word
        );
        return query_specific_entry_internal(state, file, &linked_word, dict_id, depth + 1);
    }

    let rewritten = match std::str::from_utf8(&data) {
        Ok(text) => rewrite_html(text, dict_id),
        Err(_) => {
            let text = String::from_utf8_lossy(&data);
            rewrite_html(text.as_ref(), dict_id)
        }
    };
    data = Bytes::from(rewritten);

    Ok(Some((data, "text/html".to_string())))
}
