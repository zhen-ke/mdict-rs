use std::path::Path;

use axum::body::Bytes;
use rusqlite::named_params;
use tracing::{debug, error};

use crate::app_state::AppState;

use super::error::QueryError;

pub(crate) const MAX_RESOURCE_RECORD_BYTES: usize = 32 * 1024 * 1024;

pub(crate) fn detect_content_type(word: &str) -> String {
    mime_guess::from_path(word)
        .first_or_octet_stream()
        .essence_str()
        .to_string()
}

pub(crate) fn extract_link_target(data: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(data).ok()?;
    let first_line = text.lines().next().unwrap_or("").trim();
    let linked = first_line.strip_prefix("@@@LINK=")?.trim();
    if linked.is_empty() {
        return None;
    }
    if !linked.chars().any(|c| c.is_control()) {
        return Some(linked.to_string());
    }

    let filtered: String = linked.chars().filter(|c| !c.is_control()).collect();
    let trimmed = filtered.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() == filtered.len() {
        Some(filtered)
    } else {
        Some(trimmed.to_string())
    }
}

pub(crate) fn lookup_record_in_file(
    state: &AppState,
    file: &Path,
    word: &str,
    max_record_length: Option<usize>,
) -> Result<Option<Bytes>, QueryError> {
    let conn = match state.get_db_connection(file) {
        Ok(c) => c,
        Err(e) => {
            debug!("skip dict {:?}: {}", file, e);
            return Ok(None);
        }
    };

    let mut stmt = conn
        .prepare(
            "select record_offset, record_length, block_offset, block_size, block_dsize from MDX_INDEX WHERE text= :word limit 1;",
        )
        .map_err(|e| QueryError::Internal(format!("prepare query failed for {:?}: {}", file, e)))?;
    debug!("query params={}, dict={:?}", word, file);

    let mut rows = stmt
        .query(named_params! { ":word": word })
        .map_err(|e| QueryError::Internal(format!("query failed for {:?}: {}", file, e)))?;

    let Some(row) = rows
        .next()
        .map_err(|e| QueryError::Internal(format!("fetch row failed for {:?}: {}", file, e)))?
    else {
        return Ok(None);
    };

    let record_offset: usize = row
        .get(0)
        .map_err(|e| QueryError::Internal(format!("decode record_offset failed: {}", e)))?;
    let record_length: usize = row
        .get(1)
        .map_err(|e| QueryError::Internal(format!("decode record_length failed: {}", e)))?;
    if let Some(max) = max_record_length {
        if record_length > max {
            debug!(
                "skip oversized record {} bytes > {} for key '{}' in {:?}",
                record_length, max, word, file
            );
            return Ok(None);
        }
    }
    let block_offset: usize = row
        .get(2)
        .map_err(|e| QueryError::Internal(format!("decode block_offset failed: {}", e)))?;
    let block_csize: usize = row
        .get(3)
        .map_err(|e| QueryError::Internal(format!("decode block_size failed: {}", e)))?;
    let block_dsize: usize = row
        .get(4)
        .map_err(|e| QueryError::Internal(format!("decode block_dsize failed: {}", e)))?;

    let reader = state.get_mdx_reader(file).map_err(|e| {
        QueryError::Internal(format!("open mdx reader failed for {:?}: {}", file, e))
    })?;
    let data = reader
        .read_record(
            block_offset,
            block_csize,
            block_dsize,
            record_offset,
            record_length,
        )
        .map_err(|e| {
            let err = format!("failed to read record {:?}: {}", file, e);
            error!("{}", err);
            QueryError::Internal(err)
        })?;

    Ok(Some(data))
}
