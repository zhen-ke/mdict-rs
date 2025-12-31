use std::fs;
use std::path::PathBuf;

use anyhow::Context;
use rusqlite::{params, Connection};
use memmap2::MmapOptions;

use crate::mdict::mdx::Mdx;
use tracing::info;

/// indexing all mdx files into db
pub(crate) fn indexing(files: &[&str], reindex: bool) -> anyhow::Result<()> {
    for file in files {
        let db_file_name = format!("{}{}", file, ".db");
        let db_path = PathBuf::from(&db_file_name);
        if db_path.exists() {
            if reindex {
                fs::remove_file(&db_file_name).expect("remove old db file error");
                info!("old db file:{} removed", &db_file_name);
                mdx_to_sqlite(file).expect("indexing failed");
            }
        } else {
            mdx_to_sqlite(file).expect("indexing failed");
        }
    }

    Ok(())
}

/// mdx entries and definition to sqlite table
pub(crate) fn mdx_to_sqlite(file: &str) -> anyhow::Result<()> {
    let db_file = format!("{}{}", file, ".db");
    let mut conn = Connection::open(&db_file)?;
    // Enable WAL mode and set timeout to prevent locking issues
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.pragma_update(None, "synchronous", "NORMAL").unwrap();
    conn.pragma_update(None, "busy_timeout", "5000").unwrap();
    let file_path = PathBuf::from(file);
    let mmap = unsafe {
        MmapOptions::new().map(&fs::File::open(&file_path)?)?
    };
    let mdx = Mdx::new(&mmap);

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

    let tx = conn
        .transaction()
        .with_context(|| "get transaction from connection failed")?;

    for r in mdx.entries() {
        tx.execute(
            "insert or replace into MDX_INDEX values (?,?,?,?,?,?)",
            params![
                r.text,
                r.record_start_in_de_block,
                r.record_end_in_de_block - r.record_start_in_de_block,
                r.block_offset_in_buf,
                r.block_csize,
                r.block_dsize
            ],
        )
            .with_context(|| "insert MDX_INDEX table error")?;
    }
    tx.commit().with_context(|| "transaction commit error")?;
    conn.close().expect("close db connection failed");
    Ok(())
}

