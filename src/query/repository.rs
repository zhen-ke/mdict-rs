use std::path::Path;

use axum::body::Bytes;
use rusqlite::{Connection, named_params};
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

    debug!("query params={}, dict={:?}", word, file);
    let Some(loc) = fetch_record_location(&conn, file, word)? else {
        return Ok(None);
    };

    let record_offset = loc.record_offset;
    let record_length = loc.record_length;
    if let Some(max) = max_record_length {
        if record_length > max {
            debug!(
                "skip oversized record {} bytes > {} for key '{}' in {:?}",
                record_length, max, word, file
            );
            return Ok(None);
        }
    }
    let block_offset = loc.block_offset;
    let block_csize = loc.block_csize;
    let block_dsize = loc.block_dsize;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RecordLocation {
    record_offset: usize,
    record_length: usize,
    block_offset: usize,
    block_csize: usize,
    block_dsize: usize,
}

fn fetch_record_location(
    conn: &Connection,
    file: &Path,
    word: &str,
) -> Result<Option<RecordLocation>, QueryError> {
    // Goldendict-ng style behavior: exact match first, then case-insensitive fallback.
    const EXACT_SQL: &str = "select record_offset, record_length, block_offset, block_size, block_dsize \
                             from MDX_INDEX where text = :word order by rowid asc limit 1;";
    const NOCASE_SQL: &str = "select record_offset, record_length, block_offset, block_size, block_dsize \
                              from MDX_INDEX where text = :word collate nocase order by rowid asc limit 1;";

    let exact = query_record_location(conn, EXACT_SQL, file, word)?;
    if exact.is_some() {
        return Ok(exact);
    }

    query_record_location(conn, NOCASE_SQL, file, word)
}

fn query_record_location(
    conn: &Connection,
    sql: &str,
    file: &Path,
    word: &str,
) -> Result<Option<RecordLocation>, QueryError> {
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| QueryError::Internal(format!("prepare query failed for {:?}: {}", file, e)))?;
    let mut rows = stmt
        .query(named_params! { ":word": word })
        .map_err(|e| QueryError::Internal(format!("query failed for {:?}: {}", file, e)))?;
    let Some(row) = rows
        .next()
        .map_err(|e| QueryError::Internal(format!("fetch row failed for {:?}: {}", file, e)))?
    else {
        return Ok(None);
    };

    let record_offset = row
        .get(0)
        .map_err(|e| QueryError::Internal(format!("decode record_offset failed: {}", e)))?;
    let record_length = row
        .get(1)
        .map_err(|e| QueryError::Internal(format!("decode record_length failed: {}", e)))?;
    let block_offset = row
        .get(2)
        .map_err(|e| QueryError::Internal(format!("decode block_offset failed: {}", e)))?;
    let block_csize = row
        .get(3)
        .map_err(|e| QueryError::Internal(format!("decode block_size failed: {}", e)))?;
    let block_dsize = row
        .get(4)
        .map_err(|e| QueryError::Internal(format!("decode block_dsize failed: {}", e)))?;

    Ok(Some(RecordLocation {
        record_offset,
        record_length,
        block_offset,
        block_csize,
        block_dsize,
    }))
}

#[cfg(test)]
mod tests {
    use super::fetch_record_location;
    use rusqlite::Connection;
    use std::path::Path;

    fn setup_index(conn: &Connection) {
        conn.execute_batch(
            "create table MDX_INDEX (
                id integer primary key,
                text text not null,
                record_offset integer not null,
                record_length integer not null,
                block_offset integer not null,
                block_size integer not null,
                block_dsize integer not null
             );
             create index idx_mdx_text on MDX_INDEX(text);
             create index idx_mdx_text_nocase on MDX_INDEX(text collate nocase);",
        )
        .expect("create schema");
    }

    #[test]
    fn exact_match_has_priority_over_nocase_match() {
        let conn = Connection::open_in_memory().expect("open sqlite");
        setup_index(&conn);
        conn.execute(
            "insert into MDX_INDEX(text, record_offset, record_length, block_offset, block_size, block_dsize)
             values ('hello', 1, 10, 100, 200, 300)",
            [],
        )
        .expect("insert hello");
        conn.execute(
            "insert into MDX_INDEX(text, record_offset, record_length, block_offset, block_size, block_dsize)
             values ('Hello', 2, 20, 101, 201, 301)",
            [],
        )
        .expect("insert Hello");

        let loc = fetch_record_location(&conn, Path::new("dummy.mdx"), "Hello")
            .expect("query")
            .expect("location");
        assert_eq!(loc.record_offset, 2);
        assert_eq!(loc.record_length, 20);
    }

    #[test]
    fn fallback_to_nocase_when_exact_miss() {
        let conn = Connection::open_in_memory().expect("open sqlite");
        setup_index(&conn);
        conn.execute(
            "insert into MDX_INDEX(text, record_offset, record_length, block_offset, block_size, block_dsize)
             values ('TeSt', 7, 8, 9, 10, 11)",
            [],
        )
        .expect("insert TeSt");

        let loc = fetch_record_location(&conn, Path::new("dummy.mdx"), "test")
            .expect("query")
            .expect("location");
        assert_eq!(loc.record_offset, 7);
        assert_eq!(loc.block_offset, 9);
    }
}
