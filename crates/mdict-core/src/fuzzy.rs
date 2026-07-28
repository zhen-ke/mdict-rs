//! 编辑距离近邻建议（did-you-mean）。
//!
//! 当一次精确/前缀查询未命中时，用它与词典里“首字符相同、长度相近”的词条
//! 做带早停的 Levenshtein 比对，挑出编辑距离 ≤ `max_dist` 的若干候选，作为
//! “你是不是想查…”返回给前端。
//!
//! 之所以能落在亚毫秒~毫秒级，靠的是两层剪枝：
//!   1. 首字符 + 长度窗口预筛：在既有 `idx_mdx_normalized` 覆盖索引上做
//!      `normalized >= first AND normalized < upper(first)` 的字典序区间扫描，
//!      再叠加 `length(normalized) BETWEEN lo AND hi`，把百万词条缩到几百候选；
//!   2. `CANDIDATE_CAP` 硬上限：即便遇到极端稠密前缀（全词同首字母），
//!      SQL `LIMIT` 也保证最多扫描 `CANDIDATE_CAP` 行，DoS 面天然受控；
//!   3. 早停 Levenshtein：对这几百候选跑带 `max` cutoff 的 DP，距离一旦超过
//!      `max` 立即返回 `max+1`，不计算完整矩阵。
//!
//! 这一套即 onedict 的 `Index::suggest` 做法，照搬到 mdict 的 `MDX_INDEX`
//! schema 上。不改索引 schema —— 只复用现有 `idx_mdx_normalized`。

use rusqlite::{params, Connection};

use crate::normalize::{canonical_normalize, prefix_upper};

/// 单次 fuzzy 查询从索引中扫描的候选行上限。
///
/// 预筛后的候选数远小于此（首字符 + 长度窗通常把 1M 词条压到几百）；
/// 即便命中极端稠密前缀，`LIMIT` 也把扫描量钉死在这个量级，使 fuzzy 查询
/// 有严格延迟上界（DoS 受控）。玩 onedict 同款常量。
pub const CANDIDATE_CAP: i64 = 4000;

/// fuzzy 查询返回的单条命中：编辑距离 + 词形。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzySuggestion {
    /// 与查询词的 Levenshtein 距离（≤ `max_dist`）。
    pub distance: usize,
    /// 命中的词条 `text`（原始词形，未归一化）。
    pub word: String,
}

/// 对一个未命中的查询词返回“编辑距离 ≤ `max_dist`”的近似建议。
///
/// `conn` 必须是已建好 `MDX_INDEX` + `idx_mdx_normalized` 的索引库（即
/// `mdx_to_sqlite` 产物）。返回结果按 (distance 升序, 词长升序, 字典序) 排序、
/// 去重、截断到 `limit` 条。
///
/// 算法见模块级注释：首字符区间 + 长度窗 + `CANDIDATE_CAP` 上限 + 早停 levenshtein。
pub fn fuzzy_suggest(
    conn: &Connection,
    word: &str,
    max_dist: usize,
    limit: usize,
) -> anyhow::Result<Vec<FuzzySuggestion>> {
    if limit == 0 {
        return Ok(vec![]);
    }
    let norm = canonical_normalize(word);
    if norm.is_empty() {
        return Ok(vec![]);
    }

    // 长度窗口：编辑距离不会超过两串长度差，故 |len - candidate_len| > max_dist
    // 的候选必不命中，下界 lo_len / 上界 hi_len 直接在 SQL 里过滤掉。
    let norm_len = norm.chars().count();
    let lo_len = norm_len.saturating_sub(max_dist) as i64;
    let hi_len = norm_len.saturating_add(max_dist) as i64;

    // 首字符子串：在 normalized 列（已小写、折叠变音）上做字典序区间扫描。
    // canonical_normalize 保证首字符已是小写，且候选的 normalized 也折叠过，
    // 故两侧首字符可比、同前缀的所有词在 B-tree 上连续。
    let first: String = norm.chars().take(1).collect();

    let candidates: Vec<(String, String)> = match prefix_upper(&first) {
        Some(hi) => {
            let mut stmt = conn.prepare_cached(
                "SELECT text, normalized FROM MDX_INDEX
                 WHERE normalized IS NOT NULL
                   AND normalized >= ?1 AND normalized < ?2
                   AND length(normalized) BETWEEN ?3 AND ?4
                 LIMIT ?5",
            )?;
            let rows = stmt.query_map(params![first, hi, lo_len, hi_len, CANDIDATE_CAP], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        }
        None => {
            // 首字符已是最大码点（极端退化）：退化为 >= lo 的半开扫描。
            let mut stmt = conn.prepare_cached(
                "SELECT text, normalized FROM MDX_INDEX
                 WHERE normalized IS NOT NULL
                   AND normalized >= ?1
                   AND length(normalized) BETWEEN ?2 AND ?3
                 LIMIT ?4",
            )?;
            let rows = stmt.query_map(params![first, lo_len, hi_len, CANDIDATE_CAP], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        }
    };

    let mut scored: Vec<(usize, String)> = Vec::with_capacity(candidates.len());
    for (headword, normalized) in candidates {
        if !is_fuzzy_candidate(&headword) {
            continue;
        }
        let d = levenshtein(&norm, &normalized, max_dist);
        if d <= max_dist {
            scored.push((d, headword));
        }
    }

    scored.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.len().cmp(&b.1.len()))
            .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
    });
    // 去重：同词形的最近距离已排在前面；保留首个出现（case-insensitive 去重，
    // 与服务层 merge 行为一致，避免 "Apple"/"apple" 双占名额）。
    scored.dedup_by(|a, b| a.1.eq_ignore_ascii_case(&b.1));

    Ok(scored
        .into_iter()
        .take(limit)
        .map(|(distance, word)| FuzzySuggestion { distance, word })
        .collect())
}

/// fuzzy 候选过滤：与 suggest 的 `is_suggest_candidate` 对齐，排除资源路径、
/// 巨型词条、带空格的多词短语、以特殊字符/数字起头的条目。这一步在 Rust 侧
/// 做（候选量已被 `CANDIDATE_CAP` 钉住，Rust 过滤零压力），保持 SQL 简单。
fn is_fuzzy_candidate(word: &str) -> bool {
    if word.len() > 64 {
        return false;
    }
    if word.is_empty() {
        return false;
    }
    if word.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    if word.contains('/') || word.contains('\\') || word.contains('<') || word.contains('>') {
        return false;
    }
    let starts_with_symbol_or_digit = matches!(
        word.chars().next(),
        Some(c) if c == '-' || c == '.' || c == '@' || c.is_ascii_digit()
    );
    if starts_with_symbol_or_digit {
        return false;
    }
    true
}

/// Levenshtein 编辑距离 + `max` 早停：超过 `max` 时立即返回 `max + 1`，
/// 不计算完整矩阵。长度差超 `max` 直接短路。
pub fn levenshtein(a: &str, b: &str, max: usize) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > max {
        return max + 1;
    }
    // b 是较短串以缩小内层列宽 → matrix 压成两行滚动数组。
    let (a, b) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        let mut row_min = curr[0];
        for (j, &cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
            row_min = row_min.min(curr[j + 1]);
        }
        // 当前行最小值已超 max → 最终距离必超 max，早停。
        if row_min > max {
            return max + 1;
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_basic_and_early_stop() {
        assert_eq!(levenshtein("kitten", "sitting", 3), 3);
        assert_eq!(levenshtein("serene", "serena", 2), 1);
        // 3 > max(2) → 早停返回 max+1
        assert_eq!(levenshtein("abc", "xyz", 2), 3);
        // 长度差超 max 直接短路
        assert_eq!(levenshtein("a", "abcd", 1), 2);
        assert_eq!(levenshtein("", "", 2), 0);
        assert_eq!(levenshtein("", "ab", 2), 2);
    }

    fn open_db_with_rows(rows: &[(&str, &str)]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
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
        .unwrap();
        for (i, (text, norm)) in rows.iter().enumerate() {
            conn.execute(
                "insert into MDX_INDEX(id, text, normalized, record_offset, record_length, block_offset, block_size, block_dsize)
                 values (?,?,?,?,?,?,?,?)",
                params![i as i64, text, norm, 0i64, 0i64, 0i64, 0i64, 0i64],
            )
            .unwrap();
        }
        conn.execute(
            "create index idx_mdx_normalized on MDX_INDEX(normalized, record_offset, record_length, block_offset, block_size, block_dsize)",
            [],
        )
        .unwrap();
        conn
    }

    #[test]
    fn fuzzy_finds_near_miss_within_distance() {
        // 注意：fuzzy 仅扫描与查询词“首字符相同”的候选（首字符预筛）——
        // 这正是把百万词条压到几百候选的关键剪枝。故 typo 只能测“不改动首字符的”距离。
        let conn = open_db_with_rows(&[
            ("cat", "cat"),
            ("bat", "bat"),
            ("cart", "cart"),
            ("dog", "dog"),
            ("caterpillar", "caterpillar"),
            ("Café", "cafe"),
        ]);
        // "ct" → 漏一个 'a'，与 cat 距离 1，且首字符仍为 'c'。
        let hits = fuzzy_suggest(&conn, "ct", 2, 10).unwrap();
        let words: Vec<String> = hits.iter().map(|h| h.word.clone()).collect();
        assert!(
            words.contains(&"cat".to_string()),
            "cat should match ct (dist 1): {words:?}"
        );
        assert!(
            words.contains(&"cart".to_string()),
            "cart should match ct (dist 2)"
        );
        // “bat” 首字符 'b' 与查询 'c' 不同，不在候选窗内；"dog" 同理。
        assert!(!words.contains(&"bat".to_string()));
        assert!(!words.contains(&"dog".to_string()));
        assert!(hits.iter().all(|h| h.distance <= 2));
    }

    #[test]
    fn fuzzy_first_char_typo_excluded_by_prefilter() {
        // 首字符拼错（kat 而非 cat）的 typo 不被捕获——这是首字符预筛的既定取舍。
        let conn = open_db_with_rows(&[("cat", "cat"), ("bat", "bat")]);
        let hits = fuzzy_suggest(&conn, "kat", 2, 10).unwrap();
        assert!(
            hits.is_empty(),
            "first-char typo is out of the prefilter window"
        );
    }

    #[test]
    fn fuzzy_normalizes_query_word() {
        let conn = open_db_with_rows(&[("cafe", "cafe"), ("cake", "cake")]);
        // 输入带变音/大小写差异，经 canonical_normalize 后 → "cafe"
        let hits = fuzzy_suggest(&conn, "Café", 1, 10).unwrap();
        let words: Vec<String> = hits.iter().map(|h| h.word.clone()).collect();
        assert!(
            words.contains(&"cafe".to_string()),
            "should match via normalized form"
        );
    }

    #[test]
    fn fuzzy_empty_and_zero_limit() {
        let conn = open_db_with_rows(&[("cat", "cat")]);
        assert!(fuzzy_suggest(&conn, "", 2, 10).unwrap().is_empty());
        assert!(fuzzy_suggest(&conn, "cat", 2, 0).unwrap().is_empty());
    }

    #[test]
    fn fuzzy_dedups_case_variants() {
        let conn = open_db_with_rows(&[("Apple", "apple"), ("apple", "apple")]);
        let hits = fuzzy_suggest(&conn, "aple", 1, 10).unwrap();
        assert_eq!(
            hits.len(),
            1,
            "Apple/apple share normalized form, should dedup"
        );
    }

    #[test]
    fn fuzzy_sorted_by_distance_then_length() {
        let conn = open_db_with_rows(&[("cat", "cat"), ("cart", "cart"), ("bat", "bat")]);
        let hits = fuzzy_suggest(&conn, "cat", 2, 10).unwrap();
        // 精确命中 cat (dist 0) 排首位
        assert_eq!(hits[0].word, "cat");
        assert_eq!(hits[0].distance, 0);
        // 其余按距离再按长度
        assert!(hits.iter().zip(hits.iter().skip(1)).all(|(a, b)| {
            (a.distance, a.word.len()).cmp(&(b.distance, b.word.len()))
                != std::cmp::Ordering::Greater
        }));
    }
}
