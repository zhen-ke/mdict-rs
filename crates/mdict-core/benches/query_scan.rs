//! Phase 3 回归门禁：1M 词条规模下的 SQL 查询延迟。
//!
//! 丈量 Phase 2 引入的两条热查询在百万词条下的延迟：
//!   - `WHERE normalized = ?` 精确命中（normalized 覆盖索引，O(log n)）
//!   - `WHERE normalized >= lo AND normalized < hi` 前缀区间扫描（suggest 路径）
//!
//! 这两条取代了旧的逐行 32 候选 + `LIKE 'prefix%'` 全扫，是
//! "keystroke→candidates < 50ms / 1M 词条"回归门禁的直接度量。
//!
//! 注：DB 在首次访问时一次性构建（批量多 VALUES 插入 + 延迟建索引），
//! 约 2~3s；同一进程内后续调用复用该连接。rusqlite `Connection` 非
//! `Sync`（内部 `RefCell` 语句缓存），故用 `Mutex` 包裹挂在 `OnceLock` 上；
//! bench 单线程测量，锁无竞争。

use std::sync::{Mutex, OnceLock};

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use rusqlite::{Connection, ToSql, Transaction, params, params_from_iter};

const ROWS: usize = 1_000_000;

static DB: OnceLock<Mutex<Connection>> = OnceLock::new();

fn build_db(rows: usize) -> Connection {
    let mut conn = Connection::open_in_memory().expect("open sqlite");
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
        params![],
    )
    .expect("create MDX_INDEX");

    let tx = conn.transaction().expect("tx");
    flush_chunks(&tx, rows);
    tx.commit().expect("commit");

    // 覆盖索引：与生产 schema (idx_mdx_normalized) 一致
    conn.execute(
        "create index idx_mdx_normalized on MDX_INDEX(normalized, record_offset, record_length, block_offset, block_size, block_dsize)",
        params![],
    )
    .expect("create idx_mdx_normalized");
    conn
}

fn flush_chunks(tx: &Transaction, rows: usize) {
    const CHUNK: usize = 100;
    let mut buf: Vec<(String, String, i64, i64, i64, i64, i64)> = Vec::with_capacity(CHUNK);
    for i in 0..rows {
        let text = format!("entry{i:06}");
        // text 已是 ascii 小写，normalized == text
        buf.push((text.clone(), text, i as i64, 10, i as i64, 100, 200));
        if buf.len() >= CHUNK {
            flush(tx, &mut buf);
        }
    }
    flush(tx, &mut buf);
}

fn flush(tx: &Transaction, chunk: &mut Vec<(String, String, i64, i64, i64, i64, i64)>) {
    if chunk.is_empty() {
        return;
    }
    let placeholders = (0..chunk.len())
        .map(|_| "(?,?,?,?,?,?,?)")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "insert into MDX_INDEX(text, normalized, record_offset, record_length, block_offset, block_size, block_dsize) values {placeholders}"
    );
    let mut p: Vec<&dyn ToSql> = Vec::with_capacity(chunk.len() * 7);
    for (t, n, ro, rl, bo, bc, bd) in chunk.iter() {
        p.push(t);
        p.push(n);
        p.push(ro);
        p.push(rl);
        p.push(bo);
        p.push(bc);
        p.push(bd);
    }
    tx.execute(&sql, params_from_iter(p)).expect("insert batch");
    chunk.clear();
}

fn db() -> &'static Mutex<Connection> {
    DB.get_or_init(|| Mutex::new(build_db(ROWS)))
}

fn bench_normalized_exact(c: &mut Criterion) {
    c.bench_function("normalized_exact_lookup_1m", |b| {
        b.iter(|| {
            let conn = db().lock().unwrap();
            let mut stmt = conn
                .prepare_cached(
                    "select record_offset, record_length, block_offset, block_size, block_dsize
                     from MDX_INDEX where normalized = ? order by rowid asc limit 1",
                )
                .expect("prepare");
            let r: Option<(i64, i64, i64, i64, i64)> = stmt
                .query_row(params![black_box("entry000500")], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                })
                .ok();
            black_box(r);
        });
    });
}

fn bench_prefix_scan(c: &mut Criterion) {
    c.bench_function("prefix_range_scan_1m", |b| {
        b.iter(|| {
            let conn = db().lock().unwrap();
            let mut stmt = conn
                .prepare_cached(
                    "select text from MDX_INDEX where normalized >= ? and normalized < ? order by rowid limit 10",
                )
                .expect("prepare");
            let rows: Vec<String> = stmt
                .query_map(params!["entry0005", "entry0006"], |row| {
                    row.get::<_, String>(0)
                })
                .expect("query")
                .filter_map(Result::ok)
                .collect();
            black_box(rows);
        });
    });
}

/// Phase 4 回归门禁：did-you-mean fuzzy 建议在 1M 词条下的端到端延迟。
///
/// 丈量 `mdict_core::fuzzy::fuzzy_suggest` 的完整热路径——首字符区间 + 长度窗
/// 预筛（在 `idx_mdx_normalized` 覆盖索引上 LIMIT CANDIDATE_CAP 扫描）→
/// 对 ≈4000 候选跑早停 Levenshtein → 排序去重。门禁基线：≤2 编辑距离 /
/// 1M 词条 < 50ms。
///
/// 输入 "entry000501" 是 "entry000500" 的距离-1 近邻，长度 12，首个 'e'；
/// bench DB 全部 1M 行同首字母 'e'、同长度 11，是“最坏稠密前缀”场景——
/// 预筛不再省力、直接撞 `CANDIDATE_CAP`，仍应远低于门禁。
fn bench_fuzzy_suggest(c: &mut Criterion) {
    c.bench_function("fuzzy_suggest_1m", |b| {
        b.iter(|| {
            let conn = db().lock().unwrap();
            let hits = mdict_core::fuzzy::fuzzy_suggest(&conn, black_box("entry000501"), 2, 10)
                .expect("fuzzy_suggest");
            black_box(hits);
        });
    });
}

criterion_group!(
    benches,
    bench_normalized_exact,
    bench_prefix_scan,
    bench_fuzzy_suggest
);
criterion_main!(benches);
