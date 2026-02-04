use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use memmap2::MmapOptions;
use rusqlite::{Connection, params};

use crate::mdict::mdx::Mdx;
use tracing::{info, warn};

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
        if reindex {
            fs::remove_file(&db_path)?;
            info!("old db file:{:?} removed", db_path);
            mdx_to_sqlite(file)?;
        }
        return Ok(());
    }

    mdx_to_sqlite(file)
}

/// mdx entries and definition to sqlite table
pub(crate) fn mdx_to_sqlite(file: &Path) -> anyhow::Result<()> {
    let db_file = db_path(file);
    let mut conn = Connection::open(&db_file)?;
    // Enable WAL mode and set timeout to prevent locking issues
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "busy_timeout", "5000")?;
    let mmap = unsafe { MmapOptions::new().map(&fs::File::open(file)?)? };
    let mdx = Mdx::new(&mmap)?;

    conn.execute(
        "create table if not exists MDX_INDEX (
                text text primary key not null,
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
        "create index if not exists idx_mdx_text_nocase on MDX_INDEX(text COLLATE NOCASE)",
        params![],
    )
    .with_context(|| "create index failed")?;

    let is_text_dict = !file
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("mdd"));
    let mut fts_enabled = false;
    if is_text_dict {
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
    }

    let tx = conn
        .transaction()
        .with_context(|| "get transaction from connection failed")?;

    {
        let mut stmt = tx
            .prepare_cached("insert or replace into MDX_INDEX values (?,?,?,?,?,?)")
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
            stmt.execute(params![
                r.text,
                r.record_start_in_de_block,
                r.record_end_in_de_block - r.record_start_in_de_block,
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
    Ok(())
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
