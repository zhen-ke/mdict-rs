use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use axum::body::Bytes;
use rayon::prelude::*;
use rusqlite::{Connection, named_params};
use tracing::{debug, info, warn};

use crate::app_state::AppState;

use mdict_core::fuzzy::{FuzzySuggestion, fuzzy_suggest as fuzzy_suggest_one};
use mdict_core::presenter::{
    AggregateSection, EntryMeta, extract_section_meta, render_aggregate_html,
};
use serde::Serialize;

use super::canonical_normalize;
use super::entry_query_candidates;
use super::error::QueryError;
use super::repository::{
    EntryCandidateLookup, MAX_RESOURCE_RECORD_BYTES, detect_content_type, lookup_entry_candidate,
    lookup_entry_candidate_normalized, lookup_record_in_file, rewrite_entry_html_record,
};
use super::specific::query_specific_entry_with_final;

/// Optional dict-id filter.  `None` = query all dicts (backward compatible).
pub type DictFilter = Option<HashSet<String>>;

const MAX_REDIRECT_DEPTH: u8 = 5;
const TRACE_REDIRECT_DEPTH: u8 = 10;

pub fn query(state: &AppState, word: String) -> Result<(Bytes, String), QueryError> {
    query_internal(state, word, 0)
}

/// Aggregate query results from all enabled text dictionaries.
/// Used by `/query` and `/lucky` so the frontend can show multiple dictionary entries together.
///
/// When `filter` is `Some`, only dictionaries whose id is in the set are queried.
pub fn query_aggregate(
    state: &AppState,
    word: String,
    filter: &DictFilter,
) -> Result<(Bytes, String), QueryError> {
    if is_resource_key(&word) {
        return query(state, word);
    }
    query_aggregate_entries(state, &word, filter)
}

/// Query with trace - returns the redirect chain and final word
/// Used for debugging @@@LINK depth
pub fn query_with_trace(
    state: &AppState,
    word: String,
) -> Result<(Vec<String>, String), QueryError> {
    if word.trim().is_empty() {
        return Err(QueryError::InvalidInput(
            "trace query word must not be empty".to_string(),
        ));
    }

    let mut chain = vec![word.clone()];
    let mut current = word;

    for _ in 0..TRACE_REDIRECT_DEPTH {
        match get_link_target(state, &current) {
            Some(target) => {
                chain.push(target.clone());
                current = target;
            }
            None => break,
        }
    }

    Ok((chain, current))
}

pub(crate) fn is_resource_key(word: &str) -> bool {
    word.starts_with('\\') || word.starts_with('/')
}

/// Apply an optional dict-id filter to the file list.
///
/// Returns the full list unchanged when filter is `None`, or only the files
/// whose dict_id is in the allowed set.
fn filter_dict_files<'a>(
    files: &'a [PathBuf],
    state: &AppState,
    filter: &DictFilter,
) -> Vec<&'a PathBuf> {
    match filter {
        None => files.iter().collect(),
        Some(allowed) => files
            .iter()
            .filter(|f| state.get_dict_id(f).is_some_and(|id| allowed.contains(&id)))
            .collect(),
    }
}

const FUZZY_MAX_DIST: usize = 2;

/// 返回以指定前缀开头的词条列表（用于搜索建议）
/// 使用 FTS5 + bm25 排序
///
/// When `filter` is `Some`, only dictionaries whose id is in the set are searched.
pub fn suggest(
    state: &AppState,
    prefix: String,
    limit: usize,
    filter: &DictFilter,
) -> Result<Vec<String>, QueryError> {
    let trimmed = prefix.trim();
    if trimmed.len() < 2 || limit == 0 {
        return Ok(vec![]);
    }

    let Some(fts_query) = build_fts_query(trimmed) else {
        return Ok(vec![]);
    };

    let prefix_lower = trimmed.to_lowercase();
    let canonical = canonical_normalize(trimmed);
    let query_limit = limit * 20;

    // Parallelize suggest queries across dictionaries using Rayon.
    let files = state.dict_text_files();
    let dict_files = filter_dict_files(&files, state, filter);
    let per_dict_scores: Vec<HashMap<String, i32>> = dict_files
        .par_iter()
        .map(|file| {
            let mut local_scores: HashMap<String, i32> = HashMap::new();
            let conn = match state.get_db_connection(file) {
                Ok(c) => c,
                Err(e) => {
                    debug!("skip dict {:?}: {}", file, e);
                    return local_scores;
                }
            };

            let mut stmt = match conn.prepare(
                "SELECT text, bm25(MDX_FTS) as score FROM MDX_FTS
                 WHERE MDX_FTS MATCH :query
                 ORDER BY score
                 LIMIT :limit;",
            ) {
                Ok(s) => s,
                Err(e) => {
                    debug!("FTS not available for {:?}: {}", file, e);
                    merge_prefix_fallback(
                        &mut local_scores,
                        &conn,
                        &canonical,
                        &prefix_lower,
                        query_limit,
                    );
                    return local_scores;
                }
            };

            let rows = match stmt.query_map(
                named_params! { ":query": fts_query.as_str(), ":limit": query_limit as i64 },
                |row| {
                    let text: String = row.get(0)?;
                    let score: f64 = row.get(1)?;
                    Ok((text, score))
                },
            ) {
                Ok(r) => r,
                Err(e) => {
                    warn!("FTS query failed for {:?}: {}", file, e);
                    merge_prefix_fallback(
                        &mut local_scores,
                        &conn,
                        &canonical,
                        &prefix_lower,
                        query_limit,
                    );
                    return local_scores;
                }
            };

            let mut fts_rows = 0usize;
            for row in rows {
                let Ok((word, bm25_score)) = row else {
                    continue;
                };
                if !is_suggest_candidate(&word) {
                    continue;
                }

                let word_lower = word.to_lowercase();
                let score = calculate_fts_score(&prefix_lower, &word_lower, &word, bm25_score);
                merge_score(&mut local_scores, word, score);
                fts_rows += 1;
            }

            if fts_rows == 0 {
                merge_prefix_fallback(
                    &mut local_scores,
                    &conn,
                    &canonical,
                    &prefix_lower,
                    query_limit,
                );
            }

            local_scores
        })
        .collect();

    // Merge per-dict score maps into global scores.
    let mut scores: HashMap<String, i32> = HashMap::new();
    for local in per_dict_scores {
        for (word, score) in local {
            merge_score(&mut scores, word, score);
        }
    }

    let mut candidates: Vec<(String, i32)> = scores.into_iter().collect();
    // 精确命中（含大小写）硬置顶；其次规范化等价（café/Cafe、变音、标点折叠）
    // 置顶——专业词典输入前缀时，完全一致/词形折叠一致的词条必须排最前，
    // 不能被 bm25 分数噪声覆盖（bm25_bonus 可高达数千，盖过 1000 的精确分）。
    let prefix_canon = canonical_normalize(trimmed);
    candidates.sort_by(|a, b| {
        let a_exact = a.0.to_lowercase() == prefix_lower;
        let b_exact = b.0.to_lowercase() == prefix_lower;
        match (a_exact, b_exact) {
            (true, false) => return std::cmp::Ordering::Less,
            (false, true) => return std::cmp::Ordering::Greater,
            (true, true) => {
                return a
                    .0
                    .len()
                    .cmp(&b.0.len())
                    .then(a.0.to_lowercase().cmp(&b.0.to_lowercase()));
            }
            (false, false) => {}
        }
        if !prefix_canon.is_empty() {
            let a_canon = canonical_normalize(&a.0) == prefix_canon;
            let b_canon = canonical_normalize(&b.0) == prefix_canon;
            match (a_canon, b_canon) {
                (true, false) => return std::cmp::Ordering::Less,
                (false, true) => return std::cmp::Ordering::Greater,
                _ => {}
            }
        }
        match b.1.cmp(&a.1) {
            std::cmp::Ordering::Equal => match a.0.len().cmp(&b.0.len()) {
                std::cmp::Ordering::Equal => a.0.to_lowercase().cmp(&b.0.to_lowercase()),
                other => other,
            },
            other => other,
        }
    });

    Ok(candidates
        .into_iter()
        .take(limit)
        .map(|(word, _)| word)
        .collect())
}

/// 编辑距离近邻建议（did-you-mean）：当 `/query` 未命中时调用，
/// 返回与查询词编辑距离 ≤ 2 的词条。跨词典并行执行，以每个词的“最小距离”
/// 合并，再按 (距离升序, 词长升序, 字典序) 排序、去重、截取前 `limit` 条。
///
/// 仅复用 `mdict_core::fuzzy::fuzzy_suggest`（背后是既有 `idx_mdx_normalized`
/// 覆盖索引的首字符区间 + 长度窗预筛 + 早停 Levenshtein + `CANDIDATE_CAP`），
/// 不改动任何查询/索引 schema。
///
/// When `filter` is `Some`, only dictionaries whose id is in the set are searched.
pub fn fuzzy_suggest(
    state: &AppState,
    word: String,
    limit: usize,
    filter: &DictFilter,
) -> Result<Vec<String>, QueryError> {
    let trimmed = word.trim();
    if trimmed.is_empty() || limit == 0 {
        return Ok(vec![]);
    }
    // 过短的词 fuzzy 匹配噪声太大且无意义（单字所有词均距离 ≤ 1）。
    if trimmed.chars().count() < 2 {
        return Ok(vec![]);
    }

    let files = state.dict_text_files();
    // 每个词典独立返回 (词, 最小距离)。
    let per_dict: Vec<Vec<(String, usize)>> = filter_dict_files(&files, state, filter)
        .par_iter()
        .map(|file| {
            let conn = match state.get_db_connection(file) {
                Ok(c) => c,
                Err(e) => {
                    debug!("fuzzy skip dict {:?}: {}", file, e);
                    return vec![];
                }
            };
            match fuzzy_suggest_one(&conn, trimmed, FUZZY_MAX_DIST, limit * 3) {
                Ok(hits) => hits
                    .into_iter()
                    .map(|FuzzySuggestion { distance, word }| (word, distance))
                    .collect(),
                Err(e) => {
                    warn!("fuzzy query failed for {:?}: {}", file, e);
                    vec![]
                }
            }
        })
        .collect();

    // 跨词典合并：同一词取最小距离。
    let mut best: HashMap<String, usize> = HashMap::new();
    for hits in per_dict {
        for (word, dist) in hits {
            best.entry(word)
                .and_modify(|d| *d = (*d).min(dist))
                .or_insert(dist);
        }
    }

    let mut scored: Vec<(usize, String)> = best.into_iter().map(|(w, d)| (d, w)).collect();
    scored.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.len().cmp(&b.1.len()))
            .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
    });

    Ok(scored.into_iter().take(limit).map(|(_, w)| w).collect())
}

fn query_internal(
    state: &AppState,
    word: String,
    depth: u8,
) -> Result<(Bytes, String), QueryError> {
    if depth > MAX_REDIRECT_DEPTH {
        return Err(QueryError::TooManyRedirects);
    }

    let w = word.clone();
    let files = if is_resource_key(&w) {
        state.dict_resource_files()
    } else {
        state.dict_text_files()
    };
    let candidates = if is_resource_key(&w) {
        vec![w.clone()]
    } else {
        entry_query_candidates(&w)
    };

    for file in files {
        if !is_resource_key(&w) {
            // 规范化键一次性精确匹配：命中大小写/变音/标点/空白变体即短路候选展开。
            let canonical = canonical_normalize(&w);
            if !canonical.is_empty() {
                match lookup_entry_candidate_normalized(state, &file, &canonical)? {
                    EntryCandidateLookup::Miss => {}
                    EntryCandidateLookup::Redirect(linked_word) => {
                        info!(
                            "following normalized @@@LINK redirect: {} -> {}",
                            w, linked_word
                        );
                        return query_internal(state, linked_word, depth + 1);
                    }
                    EntryCandidateLookup::Hit(data) => {
                        let html = if let Some(dict_id) = state.get_dict_id(&file) {
                            rewrite_entry_html_record(data, &dict_id)
                        } else {
                            data
                        };
                        return Ok((html, "text/html".to_string()));
                    }
                }
            }
        }
        for candidate in &candidates {
            if is_resource_key(candidate) {
                let Some(data) =
                    lookup_record_in_file(state, &file, candidate, Some(MAX_RESOURCE_RECORD_BYTES))?
                else {
                    continue;
                };
                return Ok((data, detect_content_type(candidate)));
            }

            match lookup_entry_candidate(state, &file, candidate)? {
                EntryCandidateLookup::Miss => continue,
                EntryCandidateLookup::Redirect(linked_word) => {
                    info!(
                        "following @@@LINK redirect: {} (candidate {}) -> {}",
                        w, candidate, linked_word
                    );
                    return query_internal(state, linked_word, depth + 1);
                }
                EntryCandidateLookup::Hit(data) => {
                    let html = if let Some(dict_id) = state.get_dict_id(&file) {
                        rewrite_entry_html_record(data, &dict_id)
                    } else {
                        data
                    };
                    return Ok((html, "text/html".to_string()));
                }
            }
        }
    }
    Err(QueryError::NotFound)
}

fn get_link_target(state: &AppState, word: &str) -> Option<String> {
    let candidates = entry_query_candidates(word);
    for file in state.dict_text_files() {
        for candidate in &candidates {
            let Ok(lookup) = lookup_entry_candidate(state, &file, candidate) else {
                continue;
            };
            if let EntryCandidateLookup::Redirect(linked_word) = lookup {
                return Some(linked_word);
            }
        }
    }
    None
}

fn query_aggregate_entries(
    state: &AppState,
    word: &str,
    filter: &DictFilter,
) -> Result<(Bytes, String), QueryError> {
    let results = collect_aggregate_sections(state, word, filter)?;
    if results.is_empty() {
        return Err(QueryError::NotFound);
    }
    let sections: Vec<AggregateSection> = results
        .into_iter()
        .map(|r| r.section)
        .collect();
    let html = render_aggregate_html(word, &sections);
    Ok((Bytes::from(html), "text/html".to_string()))
}

/// 聚合查询的 JSON 载荷：完整聚合 HTML（含 iframe 沙箱）+ 每本词典的结构化
/// 元数据（词头/音标/发音/entries/义项数），供前端词头条、义项导航等使用。
#[derive(Debug, Serialize)]
pub struct QueryJsonPayload {
    /// 用户查询词（未归一）。
    pub word: String,
    /// 词形归一/重定向后的最终命中词；与 `word` 不同时前端展示"词形变化"提示。
    pub matched: String,
    /// 命中词典数。
    pub hit_count: usize,
    /// 完整聚合 HTML（与 `/query` 的 HTML 响应同构，前端直接插入）。
    pub html: String,
    /// 每本词典的结构化元数据（顺序与 html 内 section 一致）。
    pub sections: Vec<QueryJsonSection>,
}

/// 单本词典的结构化元数据。
#[derive(Debug, Serialize)]
pub struct QueryJsonSection {
    pub dict_id: String,
    pub title: String,
    pub headword: String,
    pub phonetics: Vec<String>,
    pub audio: Option<String>,
    pub entries: Vec<EntryMeta>,
    pub sense_count: usize,
}

/// 聚合查询（JSON 版）：供 `/query?format=json` 使用。
pub fn query_aggregate_json(
    state: &AppState,
    word: String,
    filter: &DictFilter,
) -> Result<QueryJsonPayload, QueryError> {
    if is_resource_key(&word) {
        return Err(QueryError::InvalidInput(
            "resource keys are not supported by the JSON endpoint".to_string(),
        ));
    }
    let results = collect_aggregate_sections(state, &word, filter)?;
    if results.is_empty() {
        return Err(QueryError::NotFound);
    }

    let matched = results[0].matched.clone();
    let hit_count = results.len();
    let sections: Vec<AggregateSection> = results
        .into_iter()
        .map(|r| r.section)
        .collect();
    let html = render_aggregate_html(&word, &sections);

    let meta_sections: Vec<QueryJsonSection> = sections
        .into_iter()
        .map(|section| {
            let raw = match std::str::from_utf8(&section.body) {
                Ok(s) => s.to_string(),
                Err(_) => String::from_utf8_lossy(&section.body).into_owned(),
            };
            let meta = extract_section_meta(&raw, &section.dict_id);
            QueryJsonSection {
                dict_id: section.dict_id,
                title: section.title,
                headword: meta.headword,
                phonetics: meta.phonetics,
                audio: meta.audio,
                entries: meta.entries,
                sense_count: meta.sense_count,
            }
        })
        .collect();

    Ok(QueryJsonPayload {
        word,
        matched,
        hit_count,
        html,
        sections: meta_sections,
    })
}

/// 聚合查询的条目收集（含最终命中词），供 HTML/JSON 两个端点共用。
fn collect_aggregate_sections(
    state: &AppState,
    word: &str,
    filter: &DictFilter,
) -> Result<Vec<AggregateEntryResult>, QueryError> {
    // Collect (file, dict_id) pairs up front so we can fan out the per-dict
    // lookups in parallel. Each lookup is independent: it takes its own pooled
    // SQLite connection and mmap reader, follows @@@LINK= redirects within
    // the same file, and produces a single HTML body. rayon preserves input
    // order, so the rendered sections stay in scan order.
    let files = state.dict_text_files();
    let tasks: Vec<(&PathBuf, String)> = filter_dict_files(&files, state, filter)
        .into_iter()
        .filter_map(|file| state.get_dict_id(file).map(|id| (file, id)))
        .collect();

    if tasks.is_empty() {
        return Err(QueryError::NotFound);
    }

    let sections: Vec<AggregateEntryResult> = tasks
        .par_iter()
        .filter_map(
            |&(file, ref dict_id)| {
                match query_specific_entry_with_final(state, file, word, dict_id) {
                    Ok(Some((data, _, matched))) => {
                        // 直接存原始 `Bytes`（Arc 共享、零拷贝）， sanitize 推迟到
                        // `render_aggregate_html` 里一次性完成，避免这里先 `to_owned`
                        // 成 `String` 再 sanitize 的二道拷贝。
                        let title = state.get_dict_display_name(file);
                        let container_class = state.get_dict_container_class(file);
                        // 词典级自定义 CSS/JS（<dict>.toml 的 css/js 字段，支持内联或
                        // @file 引用）——注入该词典的 iframe 沙箱文档。内容在
                        // AppState 建 catalog 时已一次性解析（@file 读盘不落在
                        // 查询热路径上，见 `get_extra_css/js`）。
                        let extra_css = state.get_extra_css(dict_id);
                        let extra_js = state.get_extra_js(dict_id);
                        Some(AggregateEntryResult {
                            matched,
                            section: AggregateSection {
                                dict_id: dict_id.clone(),
                                title,
                                container_class,
                                extra_css,
                                extra_js,
                                body: data,
                            },
                        })
                    }
                    Ok(None) => None,
                    Err(e) => {
                        warn!(
                            "dict entry query failed for {:?}, word '{}': {}",
                            file, word, e
                        );
                        None
                    }
                }
            },
        )
        .collect();

    if sections.is_empty() {
        return Err(QueryError::NotFound);
    }
    Ok(sections)
}

/// 聚合条目收集结果：词条 + 该词典内的最终命中词。
struct AggregateEntryResult {
    section: AggregateSection,
    matched: String,
}

fn merge_prefix_fallback(
    scores: &mut HashMap<String, i32>,
    conn: &Connection,
    canonical: &str,
    prefix_lower: &str,
    query_limit: usize,
) {
    let fallback = prefix_search_normalized(conn, canonical, query_limit);
    for (rank, word) in fallback.into_iter().enumerate() {
        if !is_suggest_candidate(&word) {
            continue;
        }
        let word_lower = word.to_lowercase();
        let score = calculate_fts_score(prefix_lower, &word_lower, &word, rank as f64);
        merge_score(scores, word, score);
    }
}

fn merge_score(scores: &mut HashMap<String, i32>, word: String, score: i32) {
    match scores.get(&word) {
        Some(existing) if *existing >= score => {}
        _ => {
            scores.insert(word, score);
        }
    }
}

/// 在 `normalized` 覆盖索引上做字典序区间扫描（`>= lo AND < hi`），
/// 取代旧的 `LIKE 'prefix%'` 允底。规范化列已折叠大小写/变音/标点，
/// 因此一次区间扫描即可覆盖变音变体（如输 "cafe" 命中 "café"），
/// 且有严格延迟上界（非全索引扫描）。
fn prefix_search_normalized(conn: &Connection, canonical: &str, limit: usize) -> Vec<String> {
    let Some(hi) = mdict_core::normalize::prefix_upper(canonical) else {
        // 末位码点溢出（退化场景）：退化为只取 >= lo。
        let mut stmt = match conn.prepare(
            "SELECT text FROM MDX_INDEX WHERE normalized >= :lo \
             AND length(text) < 50 ORDER BY length(text), text LIMIT :limit;",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = match stmt.query_map(
            named_params! { ":lo": canonical, ":limit": limit as i64 },
            |row| row.get::<_, String>(0),
        ) {
            Ok(r) => r,
            Err(_) => return vec![],
        };
        return rows.filter_map(|r| r.ok()).collect();
    };
    let mut stmt = match conn.prepare(
        "SELECT text FROM MDX_INDEX WHERE normalized >= :lo AND normalized < :hi \
         AND length(text) < 50 ORDER BY length(text), text LIMIT :limit;",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let rows = match stmt.query_map(
        named_params! { ":lo": canonical, ":hi": hi, ":limit": limit as i64 },
        |row| row.get::<_, String>(0),
    ) {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    rows.filter_map(|r| r.ok()).collect()
}

fn build_fts_query(input: &str) -> Option<String> {
    let mut cleaned = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_alphanumeric() || ch.is_whitespace() {
            cleaned.push(ch);
        } else {
            cleaned.push(' ');
        }
    }

    let parts: Vec<String> = cleaned
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| format!("{}*", t))
        .collect();

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn is_suggest_candidate(word: &str) -> bool {
    if word.len() > 40 {
        return false;
    }
    if word.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    if word.contains('/') || word.contains('\\') || word.contains('<') || word.contains('>') {
        return false;
    }
    if let Some(first) = word.chars().next() {
        if first == '-' || first == '.' || first.is_ascii_digit() {
            return false;
        }
    }
    true
}

/// Calculate relevance score for a suggestion (FTS + heuristics)
fn calculate_fts_score(prefix: &str, word_lower: &str, word: &str, bm25_score: f64) -> i32 {
    let mut score = 0;

    if word_lower == prefix {
        score += 1000;
    }
    if word_lower.starts_with(prefix) {
        score += 100;
    }
    if let Some(ch) = prefix.chars().next() {
        if word.starts_with(&ch.to_uppercase().to_string()) {
            score += 20;
        }
    }

    score += 50 - word.len().min(50) as i32;

    if !word.contains(' ') {
        score += 30;
    }
    if word.chars().any(|c| c.is_numeric()) {
        score -= 20;
    }
    if word.contains(',') || word.contains(';') || word.contains(':') {
        score -= 10;
    }

    let bm25_bonus = (-bm25_score * 1000.0).round() as i32;
    score + bm25_bonus
}
