use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::Context;
use memmap2::{Advice, MmapOptions};
use rusqlite::{params, params_from_iter, Connection, ToSql, Transaction};

use crate::mdict::mdx::Mdx;
use tracing::{info, warn};

const INDEX_SCHEMA_VERSION: i64 = 4;
const META_TABLE: &str = "MDX_META";
const META_SCHEMA_VERSION: &str = "schema_version";
const META_SOURCE_SIZE: &str = "source_size";
const META_SOURCE_MTIME: &str = "source_mtime";

#[derive(Debug, Clone, Copy)]
pub struct IndexStatus {
    pub db_exists: bool,
    pub up_to_date: bool,
    pub has_fts: bool,
}

/// 一个待索引的词典文件及其索引选项。
///
/// FTS 是否启用由调用方（如 server 侧的词典配置）决定并以参数注入，
/// 核心 crate 不依赖任何配置体系。
#[derive(Debug, Clone)]
pub struct IndexJob {
    pub path: PathBuf,
    /// 是否为该词典构建 FTS5 全文索引（通常对 .mdd 资源词典关闭）
    pub fts_enabled: bool,
}

impl IndexJob {
    pub fn new(path: PathBuf, fts_enabled: bool) -> Self {
        Self { path, fts_enabled }
    }
}

pub fn db_path(dict_file: &Path) -> PathBuf {
    PathBuf::from(format!("{}.db", dict_file.to_string_lossy()))
}

/// indexing all mdx files into db
pub fn indexing(jobs: &[IndexJob], reindex: bool) -> anyhow::Result<()> {
    use rayon::prelude::*;

    let failures: Vec<(PathBuf, anyhow::Error)> = jobs
        .par_iter()
        .filter_map(|job| match ensure_index(job, reindex) {
            Ok(()) => None,
            Err(e) => Some((job.path.clone(), e)),
        })
        .collect();

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

pub fn ensure_index(job: &IndexJob, reindex: bool) -> anyhow::Result<()> {
    let file = &job.path;
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
            let start = std::time::Instant::now();
            mdx_to_sqlite(file, job.fts_enabled)?;
            tracing::info!(
                "Index built for {:?} in {:.2}s",
                file,
                start.elapsed().as_secs_f64()
            );
        }
        return Ok(());
    }

    let start = std::time::Instant::now();
    mdx_to_sqlite(file, job.fts_enabled)?;
    tracing::info!(
        "Index built for {:?} in {:.2}s",
        file,
        start.elapsed().as_secs_f64()
    );
    Ok(())
}

pub fn index_status(file: &Path) -> anyhow::Result<IndexStatus> {
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
///
/// `fts_enabled`: 是否为此文件构建 FTS5 全文索引（仅对文本词典有意义；
/// .mdd 资源词典内部会被强制关闭）。
pub fn mdx_to_sqlite(file: &Path, fts_enabled: bool) -> anyhow::Result<()> {
    let db_file = db_path(file);
    let mut conn = Connection::open(&db_file)?;
    // Tune SQLite for a bulk index build.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "busy_timeout", "5000")?;
    // Larger page cache + temp-in-memory speeds up the sort/build phase.
    conn.pragma_update(None, "cache_size", "-200000")?; // ~200 MiB
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    let mmap = unsafe { MmapOptions::new().map(&fs::File::open(file)?)? };
    // 索引构建是顺序扫整个 MDX；告知内核",可激进预读、阅后即弃，
    // 避免把 page cache 填满并非查询热区页。
    let _ = mmap.advise(Advice::Sequential);
    let mdx = Mdx::new(&mmap)?;

    // Schema: create the *data* tables first (MDX_INDEX, MDX_META, MDX_FTS),
    // but deliberately defer the B-tree indexes (idx_mdx_text,
    // idx_mdx_text_nocase) until AFTER all rows are inserted. Building an
    // index in one pass over pre-sorted rowids is far cheaper than updating
    // the index B-tree on every single INSERT.
    conn.execute(
        "create table if not exists MDX_INDEX (
                id integer primary key,
                text text not null,
                normalized text,
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
    let fts_allowed = is_text_dict && fts_enabled;
    let mut fts_enabled = false;
    if fts_allowed {
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
        info!("FTS disabled by caller for {:?}", file);
    }

    let tx = conn
        .transaction()
        .with_context(|| "get transaction from connection failed")?;

    // 分块多行批量插入：每块以单条多 VALUES 语句写入，减少 SQLite 调用与解析
    // 轮次（相对逐行 prepared execute 可量级下降）。块大小受 SQLite 变量数
    // 上限制约：现代 bundled SQLite 的 `SQLITE_MAX_VARIABLE_NUMBER=32766`，
    // 故取 4000×7=28000 < 32766，把单条多 VALUES 的仸载吃到 ~85%，
    // 相比族的 100×7=700（仅 2%），建索引轮次/解析次数降一个量级。即便
    // 运行在 `MAX_VARIABLE_NUMBER=999` 的旧版上超限，`flush_index_chunk` 会以
    // 原生 SQLite 错误指出，可опат减回退。
    const INSERT_CHUNK: usize = 4000;
    let mut chunk: Vec<(String, Option<String>, i64, i64, i64, i64, i64)> =
        Vec::with_capacity(INSERT_CHUNK);
    let mut fts_chunk: Vec<String> = Vec::with_capacity(INSERT_CHUNK);

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
        // 仅对文本词典计算 normalized；资源词典（.mdd）的 text 是路径，
        // 归一化会破坏路径匹配，故存 NULL。
        let normalized = if is_text_dict {
            Some(crate::normalize::canonical_normalize(&r.text))
        } else {
            None
        };
        chunk.push((
            r.text.clone(),
            normalized,
            r.record_start_in_de_block as i64,
            record_length as i64,
            r.block_offset_in_buf as i64,
            r.block_csize as i64,
            r.block_dsize as i64,
        ));
        if fts_enabled && should_index_in_fts(&r.text) {
            fts_chunk.push(r.text.clone());
        }
        if chunk.len() >= INSERT_CHUNK {
            flush_index_chunk(&tx, &mut chunk)?;
        }
        if fts_chunk.len() >= INSERT_CHUNK {
            flush_fts_chunk(&tx, &mut fts_chunk)?;
        }
    }
    flush_index_chunk(&tx, &mut chunk)?;
    flush_fts_chunk(&tx, &mut fts_chunk)?;
    tx.commit().with_context(|| "transaction commit error")?;

    // Now that all rows are in MDX_INDEX, build the lookup indexes in one pass.
    // This is dramatically faster than maintaining them during INSERT (no
    // per-row B-tree page splits / random I/O).
    conn.execute(
        "create index if not exists idx_mdx_text on MDX_INDEX(text)",
        params![],
    )
    .with_context(|| "create idx_mdx_text failed")?;
    conn.execute(
        "create index if not exists idx_mdx_text_nocase on MDX_INDEX(text COLLATE NOCASE)",
        params![],
    )
    .with_context(|| "create idx_mdx_text_nocase failed")?;
    // normalized 覆盖索引：一次 `WHERE normalized = ?` 即返回全部定位列，
    // 避免回表；也支撑前缀区间扫描 suggest。
    conn.execute(
        "create index if not exists idx_mdx_normalized on MDX_INDEX(normalized, record_offset, record_length, block_offset, block_size, block_dsize)",
        params![],
    )
    .with_context(|| "create idx_mdx_normalized failed")?;
    // FTS5 incremental merge can leave a large pending segment; force it to
    // flush + optimize now so the first query doesn't pay the merge cost.
    if fts_enabled {
        let _ = conn.execute("insert into MDX_FTS(MDX_FTS) values('optimize')", params![]);
    }
    write_index_meta(&conn, file)?;
    Ok(())
}

/// 把一chunk（最多 100 行）以单条多 VALUES 语句写入 MDX_INDEX，
/// 减少 SQLite 调用/解析轮次。块为空时直接返回。
fn flush_index_chunk(
    tx: &Transaction,
    chunk: &mut Vec<(String, Option<String>, i64, i64, i64, i64, i64)>,
) -> anyhow::Result<()> {
    if chunk.is_empty() {
        return Ok(());
    }
    let placeholders = (0..chunk.len())
        .map(|_| "(?,?,?,?,?,?,?)")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "insert into MDX_INDEX(text, normalized, record_offset, record_length, block_offset, block_size, block_dsize) values {placeholders}"
    );
    let mut p: Vec<&dyn ToSql> = Vec::with_capacity(chunk.len() * 7);
    for (text, normalized, ro, rl, bo, bc, bd) in chunk.iter() {
        p.push(text);
        p.push(normalized);
        p.push(ro);
        p.push(rl);
        p.push(bo);
        p.push(bc);
        p.push(bd);
    }
    tx.execute(&sql, params_from_iter(p))
        .with_context(|| "insert MDX_INDEX batch error")?;
    chunk.clear();
    Ok(())
}

/// 把一chunk FTS 文本以单条多 VALUES 语句写入 MDX_FTS。块为空时直接返回。
fn flush_fts_chunk(tx: &Transaction, chunk: &mut Vec<String>) -> anyhow::Result<()> {
    if chunk.is_empty() {
        return Ok(());
    }
    let placeholders = (0..chunk.len())
        .map(|_| "(?)")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("insert into MDX_FTS(text) values {placeholders}");
    let p: Vec<&dyn ToSql> = chunk.iter().map(|s| s as &dyn ToSql).collect();
    tx.execute(&sql, params_from_iter(p))
        .with_context(|| "insert MDX_FTS batch error")?;
    chunk.clear();
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

#[cfg(test)]
mod tests {
    use super::{flush_index_chunk, should_index_in_fts};
    use rusqlite::Connection;

    fn open_index_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open sqlite");
        conn.execute(
            "create table MDX_INDEX (
                id integer primary key,
                text text not null,
                normalized text,
                record_offset integer not null,
                record_length integer not null,
                block_offset integer not null,
                block_size integer not null,
                block_dsize integer not null
             )",
            [],
        )
        .expect("create MDX_INDEX");
        conn
    }

    #[test]
    fn batch_insert_writes_all_rows_with_correct_normalized() {
        let mut conn = open_index_db();
        let tx = conn.transaction().expect("tx");
        let mut chunk = vec![
            ("a".to_string(), Some("a".to_string()), 1, 2, 3, 4, 5),
            ("B".to_string(), Some("b".to_string()), 6, 7, 8, 9, 10),
            (
                "café".to_string(),
                Some("cafe".to_string()),
                11,
                12,
                13,
                14,
                15,
            ),
        ];
        flush_index_chunk(&tx, &mut chunk).expect("flush");
        tx.commit().expect("commit");

        let count: i64 = conn
            .query_row("select count(*) from MDX_INDEX", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 3);

        let norm: String = conn
            .query_row(
                "select normalized from MDX_INDEX where text='café'",
                [],
                |r| r.get(0),
            )
            .expect("query café");
        assert_eq!(norm, "cafe");
        // chunk was drained
        assert!(chunk.is_empty());
    }

    #[test]
    fn flush_index_chunk_noop_on_empty() {
        let mut conn = open_index_db();
        let tx = conn.transaction().expect("tx");
        let mut chunk: Vec<(String, Option<String>, i64, i64, i64, i64, i64)> = vec![];
        flush_index_chunk(&tx, &mut chunk).expect("flush empty");
        tx.commit().expect("commit");
        let count: i64 = conn
            .query_row("select count(*) from MDX_INDEX", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 0);
    }

    #[test]
    fn fts_filter_rejects_paths_and_digits() {
        assert!(should_index_in_fts("hello"));
        assert!(!should_index_in_fts("\\path\\to"));
        assert!(!should_index_in_fts("12monkeys"));
        assert!(!should_index_in_fts("a@b"));
    }
}
