use std::collections::HashMap;

use axum::body::Bytes;
use rusqlite::{Connection, named_params};
use tracing::{debug, info, warn};

use crate::app_state::AppState;

use super::entry_query_candidates;
use super::error::QueryError;
use super::presenter::{AggregateSection, render_aggregate_html};
use super::repository::{
    MAX_RESOURCE_RECORD_BYTES, detect_content_type, extract_link_target, lookup_record_in_file,
};
use super::rewrite::rewrite_html;
use super::specific::query_specific_entry;

const MAX_REDIRECT_DEPTH: u8 = 5;
const TRACE_REDIRECT_DEPTH: u8 = 10;

pub fn query(state: &AppState, word: String) -> Result<(Bytes, String), QueryError> {
    query_internal(state, word, 0)
}

/// Aggregate query results from all enabled text dictionaries.
/// Used by `/query` and `/lucky` so the frontend can show multiple dictionary entries together.
pub fn query_aggregate(state: &AppState, word: String) -> Result<(Bytes, String), QueryError> {
    if is_resource_key(&word) {
        return query(state, word);
    }
    query_aggregate_entries(state, &word)
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

/// 返回以指定前缀开头的词条列表（用于搜索建议）
/// 使用 FTS5 + bm25 排序
pub fn suggest(state: &AppState, prefix: String, limit: usize) -> Result<Vec<String>, QueryError> {
    let trimmed = prefix.trim();
    if trimmed.len() < 2 || limit == 0 {
        return Ok(vec![]);
    }

    let Some(fts_query) = build_fts_query(trimmed) else {
        return Ok(vec![]);
    };

    let prefix_lower = trimmed.to_lowercase();
    let query_limit = limit * 20;
    let mut scores: HashMap<String, i32> = HashMap::new();

    for file in state.dict_text_files() {
        let conn = match state.get_db_connection(file) {
            Ok(c) => c,
            Err(e) => {
                debug!("skip dict {:?}: {}", file, e);
                continue;
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
                merge_prefix_fallback(&mut scores, &conn, &prefix_lower, query_limit);
                continue;
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
                merge_prefix_fallback(&mut scores, &conn, &prefix_lower, query_limit);
                continue;
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
            merge_score(&mut scores, word, score);
            fts_rows += 1;
        }

        if fts_rows == 0 {
            merge_prefix_fallback(&mut scores, &conn, &prefix_lower, query_limit);
        }
    }

    let mut candidates: Vec<(String, i32)> = scores.into_iter().collect();
    candidates.sort_by(|a, b| match b.1.cmp(&a.1) {
        std::cmp::Ordering::Equal => match a.0.len().cmp(&b.0.len()) {
            std::cmp::Ordering::Equal => a.0.to_lowercase().cmp(&b.0.to_lowercase()),
            other => other,
        },
        other => other,
    });

    Ok(candidates
        .into_iter()
        .take(limit)
        .map(|(word, _)| word)
        .collect())
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
        for candidate in &candidates {
            let max_record_length = if is_resource_key(candidate) {
                Some(MAX_RESOURCE_RECORD_BYTES)
            } else {
                None
            };
            let Some(data) = lookup_record_in_file(state, file, candidate, max_record_length)?
            else {
                continue;
            };

            if !is_resource_key(candidate) {
                if let Some(linked_word) = extract_link_target(&data) {
                    info!(
                        "following @@@LINK redirect: {} (candidate {}) -> {}",
                        w, candidate, linked_word
                    );
                    return query_internal(state, linked_word, depth + 1);
                }
            }

            let mut final_data = data;
            let content_type = if is_resource_key(candidate) {
                detect_content_type(candidate)
            } else {
                if let Some(dict_id) = state.get_dict_id(file) {
                    let rewritten = match std::str::from_utf8(&final_data) {
                        Ok(text) => rewrite_html(text, &dict_id),
                        Err(_) => {
                            let text = String::from_utf8_lossy(&final_data);
                            rewrite_html(text.as_ref(), &dict_id)
                        }
                    };
                    final_data = Bytes::from(rewritten);
                }
                "text/html".to_string()
            };

            return Ok((final_data, content_type));
        }
    }
    Err(QueryError::NotFound)
}

fn get_link_target(state: &AppState, word: &str) -> Option<String> {
    let candidates = entry_query_candidates(word);
    for file in state.dict_text_files() {
        for candidate in &candidates {
            let Ok(Some(data)) = lookup_record_in_file(state, file, candidate, None) else {
                continue;
            };
            if let Some(linked_word) = extract_link_target(&data) {
                return Some(linked_word);
            }
        }
    }
    None
}

fn query_aggregate_entries(state: &AppState, word: &str) -> Result<(Bytes, String), QueryError> {
    let mut sections = Vec::new();

    for file in state.dict_text_files() {
        let Some(dict_id) = state.get_dict_id(file) else {
            continue;
        };

        match query_specific_entry(state, file, word, &dict_id) {
            Ok(Some((data, _))) => {
                let body = match std::str::from_utf8(&data) {
                    Ok(text) => text.to_owned(),
                    Err(_) => String::from_utf8_lossy(&data).into_owned(),
                };
                let title = state.get_dict_display_name(file);
                let container_class = state.get_dict_container_class(file);
                sections.push(AggregateSection {
                    dict_id,
                    title,
                    container_class,
                    body,
                });
            }
            Ok(None) => {}
            Err(e) => {
                warn!(
                    "dict entry query failed for {:?}, word '{}': {}",
                    file, word, e
                );
            }
        }
    }

    if sections.is_empty() {
        return Err(QueryError::NotFound);
    }

    let html = render_aggregate_html(word, &sections);
    Ok((Bytes::from(html), "text/html".to_string()))
}

fn merge_prefix_fallback(
    scores: &mut HashMap<String, i32>,
    conn: &Connection,
    prefix_lower: &str,
    query_limit: usize,
) {
    let fallback = prefix_search(conn, prefix_lower, query_limit);
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

fn prefix_search(conn: &Connection, prefix_lower: &str, limit: usize) -> Vec<String> {
    let pattern = format!("{}%", prefix_lower);
    let mut stmt = match conn.prepare(
        "SELECT text FROM MDX_INDEX
         WHERE text LIKE :pattern COLLATE NOCASE
         AND text NOT LIKE '\\%'
         AND text NOT LIKE '@%'
         AND text NOT LIKE '%@%'
         AND text NOT LIKE '0%'
         AND text NOT LIKE '1%'
         AND text NOT LIKE '2%'
         AND text NOT LIKE '3%'
         AND text NOT LIKE '4%'
         AND text NOT LIKE '5%'
         AND text NOT LIKE '6%'
         AND text NOT LIKE '7%'
         AND text NOT LIKE '8%'
         AND text NOT LIKE '9%'
         AND LENGTH(text) < 50
         ORDER BY LENGTH(text), text
         LIMIT :limit;",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let rows = match stmt.query_map(
        named_params! { ":pattern": pattern, ":limit": limit as i64 },
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
    if word.starts_with('-') || word.starts_with('.') {
        return false;
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
    if word.starts_with(&prefix.chars().next().unwrap().to_uppercase().to_string()) {
        score += 20;
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
