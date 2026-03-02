use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::Context;
use memmap2::MmapOptions;
use rusqlite::{Connection, params};

use crate::config::DictConfig;
use crate::mdict::mdx::Mdx;
use tracing::{info, warn};

const INDEX_SCHEMA_VERSION: i64 = 3;
const META_TABLE: &str = "MDX_META";
const META_SCHEMA_VERSION: &str = "schema_version";
const META_SOURCE_SIZE: &str = "source_size";
const META_SOURCE_MTIME: &str = "source_mtime";

#[derive(Debug, Clone, Copy)]
pub(crate) struct IndexStatus {
    pub db_exists: bool,
    pub up_to_date: bool,
    pub has_fts: bool,
}

pub(crate) fn db_path(dict_file: &Path) -> PathBuf {
    PathBuf::from(format!("{}.db", dict_file.to_string_lossy()))
}

/// indexing all mdx files into db
pub(crate) fn indexing(files: &[PathBuf], reindex: bool) -> anyhow::Result<()> {
    let mut failures: Vec<(PathBuf, anyhow::Error)> = Vec::new();

    for file in files {
        if let Err(e) = ensure_index(file, reindex) {
            failures.push((file.clone(), e));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        for (file, err) in &failures {
            warn!("Indexing failed for {:?}: {}", file, err);
        }
        Err(anyhow::anyhow!(
            "Indexing failed for {} dictionaries",
            failures.len()
        ))
    }
}

pub(crate) fn ensure_index(file: &Path, reindex: bool) -> anyhow::Result<()> {
    let db_path = db_path(file);
    if db_path.exists() {
        let needs_reindex = if reindex {
            true
        } else {
            match index_up_to_date(file, &db_path) {
                Ok(up_to_date) => !up_to_date,
                Err(e) => {
                    warn!(
                        "Failed to validate existing index {:?}, will rebuild: {}",
                        db_path, e
                    );
                    true
                }
            }
        };

        if needs_reindex {
            fs::remove_file(&db_path)?;
            info!(
                "Rebuilding index for {:?}, old db removed: {:?}",
                file, db_path
            );
            mdx_to_sqlite(file)?;
        }
        return Ok(());
    }

    mdx_to_sqlite(file)
}

pub(crate) fn index_status(file: &Path) -> anyhow::Result<IndexStatus> {
    let db_file = db_path(file);
    if !db_file.exists() {
        return Ok(IndexStatus {
            db_exists: false,
            up_to_date: false,
            has_fts: false,
        });
    }

    let up_to_date = index_up_to_date(file, &db_file).unwrap_or(false);
    let conn = Connection::open(&db_file)?;
    let has_fts = has_table(&conn, "MDX_FTS")?;
    Ok(IndexStatus {
        db_exists: true,
        up_to_date,
        has_fts,
    })
}

/// mdx entries and definition to sqlite table
pub(crate) fn mdx_to_sqlite(file: &Path) -> anyhow::Result<()> {
    let db_file = db_path(file);
    let mut conn = Connection::open(&db_file)?;
    // Enable WAL mode during index build.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "busy_timeout", "5000")?;
    let mmap = unsafe { MmapOptions::new().map(&fs::File::open(file)?)? };
    let mdx = Mdx::new(&mmap)?;

    conn.execute(
        "create table if not exists MDX_INDEX (
                id integer primary key,
                text text not null,
                record_offset integer not null,
                record_length integer not null,
                block_offset integer not null,
                block_size integer not null,
                block_dsize integer not null
         )",
        params![],
    )
    .with_context(|| "create table failed")?;
    conn.execute(
        "create index if not exists idx_mdx_text on MDX_INDEX(text)",
        params![],
    )
    .with_context(|| "create index failed")?;
    conn.execute(
        "create index if not exists idx_mdx_text_nocase on MDX_INDEX(text COLLATE NOCASE)",
        params![],
    )
    .with_context(|| "create index failed")?;
    conn.execute(
        "create table if not exists MDX_META (
            key text primary key not null,
            value text not null
         )",
        params![],
    )
    .with_context(|| "create metadata table failed")?;

    let is_text_dict = !file
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("mdd"));
    let mut fts_enabled = false;
    let fts_allowed = if is_text_dict {
        DictConfig::load(file)
            .map(|cfg| cfg.is_fts_enabled())
            .unwrap_or(true)
    } else {
        false
    };
    if is_text_dict && fts_allowed {
        match conn.execute(
            "create virtual table if not exists MDX_FTS using fts5(text, tokenize='unicode61 remove_diacritics 2')",
            params![],
        ) {
            Ok(_) => {
                fts_enabled = true;
            }
            Err(e) => {
                warn!(
                    "FTS5 not available, skipping MDX_FTS for {:?}: {}",
                    file, e
                );
            }
        }
    } else if is_text_dict {
        info!("FTS disabled by config for {:?}", file);
    }

    let tx = conn
        .transaction()
        .with_context(|| "get transaction from connection failed")?;

    {
        let mut stmt = tx
            .prepare_cached(
                "insert into MDX_INDEX(text, record_offset, record_length, block_offset, block_size, block_dsize) values (?,?,?,?,?,?)",
            )
            .with_context(|| "prepare insert statement failed")?;
        let mut fts_stmt = if fts_enabled {
            Some(
                tx.prepare_cached("insert into MDX_FTS(text) values (?)")
                    .with_context(|| "prepare FTS insert statement failed")?,
            )
        } else {
            None
        };

        for r in mdx.entries() {
            let record_length = r
                .record_end_in_de_block
                .checked_sub(r.record_start_in_de_block)
                .with_context(|| {
                    format!(
                        "invalid record range for '{}': {}..{}",
                        r.text, r.record_start_in_de_block, r.record_end_in_de_block
                    )
                })?;
            stmt.execute(params![
                r.text,
                r.record_start_in_de_block,
                record_length,
                r.block_offset_in_buf,
                r.block_csize,
                r.block_dsize
            ])
            .with_context(|| "insert MDX_INDEX table error")?;

            if let Some(ref mut fts_stmt) = fts_stmt {
                if should_index_in_fts(&r.text) {
                    fts_stmt
                        .execute(params![r.text])
                        .with_context(|| "insert MDX_FTS table error")?;
                }
            }
        }
    }
    tx.commit().with_context(|| "transaction commit error")?;
    write_index_meta(&conn, file)?;
    Ok(())
}

fn index_up_to_date(file: &Path, db_file: &Path) -> anyhow::Result<bool> {
    let conn = Connection::open(db_file)?;

    if !has_table(&conn, "MDX_INDEX")? {
        return Ok(false);
    }

    if !has_table(&conn, META_TABLE)? {
        return Ok(false);
    }

    let schema_version = read_meta_value(&conn, META_SCHEMA_VERSION)?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or_default();
    if schema_version != INDEX_SCHEMA_VERSION {
        return Ok(false);
    }

    let source_size = read_meta_value(&conn, META_SOURCE_SIZE)?
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or_default();
    let source_mtime = read_meta_value(&conn, META_SOURCE_MTIME)?
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or_default();
    let (current_size, current_mtime) = source_signature(file)?;

    Ok(source_size == current_size && source_mtime == current_mtime)
}

fn write_index_meta(conn: &Connection, file: &Path) -> anyhow::Result<()> {
    let (size, mtime) = source_signature(file)?;
    conn.execute(
        "insert or replace into MDX_META(key, value) values (?1, ?2)",
        params![META_SCHEMA_VERSION, INDEX_SCHEMA_VERSION.to_string()],
    )
    .with_context(|| "write meta schema_version failed")?;
    conn.execute(
        "insert or replace into MDX_META(key, value) values (?1, ?2)",
        params![META_SOURCE_SIZE, size.to_string()],
    )
    .with_context(|| "write meta source_size failed")?;
    conn.execute(
        "insert or replace into MDX_META(key, value) values (?1, ?2)",
        params![META_SOURCE_MTIME, mtime.to_string()],
    )
    .with_context(|| "write meta source_mtime failed")?;
    Ok(())
}

fn read_meta_value(conn: &Connection, key: &str) -> anyhow::Result<Option<String>> {
    let mut stmt = conn.prepare("select value from MDX_META where key = ?1 limit 1")?;
    let mut rows = stmt.query(params![key])?;
    if let Some(row) = rows.next()? {
        let value: String = row.get(0)?;
        Ok(Some(value))
    } else {
        Ok(None)
    }
}

fn has_table(conn: &Connection, table: &str) -> anyhow::Result<bool> {
    let count: i64 = conn.query_row(
        "select count(*) from sqlite_master where type='table' and name=?1",
        params![table],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn source_signature(file: &Path) -> anyhow::Result<(u64, u64)> {
    let meta = fs::metadata(file)?;
    let size = meta.len();
    let modified = meta
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Ok((size, modified))
}

fn should_index_in_fts(text: &str) -> bool {
    if text.is_empty() || text.len() > 50 {
        return false;
    }
    if text.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    let mut chars = text.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return false,
    };
    if first == '\\' || first == '/' || first == '@' || first == '-' || first == '.' {
        return false;
    }
    if first.is_ascii_digit() {
        return false;
    }
    if text.contains('@') || text.contains('<') || text.contains('>') {
        return false;
    }
    if text.contains('/') || text.contains('\\') {
        return false;
    }
    true
}
