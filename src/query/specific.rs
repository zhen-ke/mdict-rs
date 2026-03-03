use crate::app_state::AppState;
use crate::query::{
    EntryCandidateLookup, MAX_RESOURCE_RECORD_BYTES, QueryError, detect_content_type,
    entry_query_candidates, lookup_entry_candidate, lookup_record_in_file,
    rewrite_entry_html_record,
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

    let candidates = entry_query_candidates(word);
    if candidates.is_empty() {
        return Ok(None);
    }

    for candidate in candidates {
        match lookup_entry_candidate(state, file, &candidate)? {
            EntryCandidateLookup::Miss => continue,
            EntryCandidateLookup::Redirect(linked_word) => {
                info!(
                    "following dict-specific @@@LINK redirect: {} (candidate {}) -> {}",
                    word, candidate, linked_word
                );
                return query_specific_entry_internal(
                    state,
                    file,
                    &linked_word,
                    dict_id,
                    depth + 1,
                );
            }
            EntryCandidateLookup::Hit(data) => {
                let data = rewrite_entry_html_record(data, dict_id);
                return Ok(Some((data, "text/html".to_string())));
            }
        }
    }

    Ok(None)
}
