use crate::app_state::AppState;
use rusqlite::{Connection, named_params};
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, error, info, warn};

mod rewrite;
pub(crate) use rewrite::rewrite_html;

mod specific;
pub use specific::{query_specific_entry, query_specific_resource};

pub fn query(state: &AppState, word: String) -> Result<(Vec<u8>, String), String> {
    query_internal(state, word, 0)
}

/// Aggregate query results from all enabled text dictionaries.
/// Used by `/query` and `/lucky` so the frontend can show multiple dictionary entries together.
pub fn query_aggregate(state: &AppState, word: String) -> Result<(Vec<u8>, String), String> {
    if is_resource_key(&word) {
        return query(state, word);
    }
    query_aggregate_entries(state, &word)
}

/// Query with trace - returns the redirect chain and final word
/// Used for debugging @@@LINK depth
pub fn query_with_trace(state: &AppState, word: String) -> Result<(Vec<String>, String), String> {
    let mut chain = vec![word.clone()];
    let mut current = word;

    for _ in 0..10 {
        // Max 10 redirects
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

/// Check if a word redirects via @@@LINK= and return the target
fn get_link_target(state: &AppState, word: &str) -> Option<String> {
    for file in state.dict_text_files() {
        let Ok(Some(data)) = lookup_record_in_file(state, file, word) else {
            continue;
        };
        if let Some(linked_word) = extract_link_target(&data) {
            return Some(linked_word);
        }
    }
    None
}

pub(crate) fn is_resource_key(word: &str) -> bool {
    word.starts_with('\\') || word.starts_with('/')
}

/// Internal query with redirect depth limit to prevent infinite loops
fn query_internal(state: &AppState, word: String, depth: u8) -> Result<(Vec<u8>, String), String> {
    // Prevent infinite redirect loops
    if depth > 5 {
        return Err("too many redirects".to_string());
    }

    let w = word.clone();
    let files = if is_resource_key(&w) {
        state.dict_resource_files()
    } else {
        state.dict_text_files()
    };

    for file in files.iter() {
        let Some(data) = lookup_record_in_file(state, file, &w)? else {
            continue;
        };

        // Check for @@@LINK= redirect
        if !is_resource_key(&w) {
            if let Some(linked_word) = extract_link_target(&data) {
                info!("following @@@LINK redirect: {} -> {}", w, linked_word);
                return query_internal(state, linked_word, depth + 1);
            }
        }

        let mut final_data = data;
        let content_type = if is_resource_key(&w) {
            detect_content_type(&w)
        } else {
            if let Some(dict_id) = state.get_dict_id(file) {
                let text = String::from_utf8_lossy(&final_data).to_string();
                let rewritten = rewrite_html(&text, &dict_id);
                final_data = rewritten.into_bytes();
            }
            "text/html".to_string()
        };

        return Ok((final_data, content_type));
    }
    Err("not found".to_string())
}

fn query_aggregate_entries(state: &AppState, word: &str) -> Result<(Vec<u8>, String), String> {
    let mut sections = Vec::new();

    for file in state.dict_text_files() {
        let Some(dict_id) = state.get_dict_id(file) else {
            continue;
        };

        match query_specific_entry(state, file, word, &dict_id) {
            Ok(Some((data, _))) => {
                let body = String::from_utf8_lossy(&data).to_string();
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
        return Err("not found".to_string());
    }

    let html = render_aggregate_html(word, &sections);
    Ok((html.into_bytes(), "text/html".to_string()))
}

pub(crate) fn detect_content_type(word: &str) -> String {
    mime_guess::from_path(word)
        .first_or_octet_stream()
        .essence_str()
        .to_string()
}

pub(crate) fn extract_link_target(data: &[u8]) -> Option<String> {
    let text = String::from_utf8(data.to_vec()).ok()?;
    let first_line = text.lines().next().unwrap_or("").trim();
    if !first_line.starts_with("@@@LINK=") {
        return None;
    }

    let linked_word = first_line
        .trim_start_matches("@@@LINK=")
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string();
    if linked_word.is_empty() {
        None
    } else {
        Some(linked_word)
    }
}

pub(crate) fn lookup_record_in_file(
    state: &AppState,
    file: &Path,
    word: &str,
) -> Result<Option<Vec<u8>>, String> {
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
        .map_err(|e| e.to_string())?;
    debug!("query params={}, dict={:?}", word, file);

    let mut rows = stmt
        .query(named_params! { ":word": word })
        .map_err(|e| e.to_string())?;

    let Some(row) = rows.next().map_err(|e| e.to_string())? else {
        return Ok(None);
    };

    let record_offset: usize = row.get(0).unwrap_or(0);
    let record_length: usize = row.get(1).unwrap_or(0);
    let block_offset: usize = row.get(2).unwrap_or(0);
    let block_csize: usize = row.get(3).unwrap_or(0);
    let block_dsize: usize = row.get(4).unwrap_or(0);

    let reader = state.get_mdx_reader(file).map_err(|e| e.to_string())?;
    let data = reader
        .read_record(
            block_offset,
            block_csize,
            block_dsize,
            record_offset,
            record_length,
        )
        .map_err(|e| {
            let err = format!("failed to read record: {}", e);
            error!("{}", err);
            err
        })?;

    Ok(Some(data))
}

struct AggregateSection {
    dict_id: String,
    title: String,
    container_class: Option<String>,
    body: String,
}

fn render_aggregate_html(word: &str, sections: &[AggregateSection]) -> String {
    let mut html = String::with_capacity(sections.len() * 4096);
    html.push_str(r#"<div class="mdict-aggregate">"#);
    html.push_str(&format!(
        r#"<div class="mdict-aggregate-meta"><span class="mdict-agg-hit">命中 {} 本词典</span><span class="mdict-agg-dot">·</span><span class="mdict-agg-label">查询词</span><strong class="mdict-query-word">{}</strong></div>"#,
        sections.len(),
        escape_html(word)
    ));

    for (idx, section) in sections.iter().enumerate() {
        let class_attr = section
            .container_class
            .as_ref()
            .map(|cls| format!(" {}", escape_html_attr(cls)))
            .unwrap_or_default();

        html.push_str(&format!(
            r#"<section class="mdict-dict-section{}" data-dict-id="{}">"#,
            class_attr,
            escape_html_attr(&section.dict_id)
        ));
        html.push_str(&format!(
            r#"<header class="mdict-dict-head"><div class="mdict-dict-title"><span class="mdict-dict-index">{}</span><span class="mdict-dict-name">{}</span></div><span class="mdict-dict-id">{}</span></header>"#,
            idx + 1,
            escape_html(&section.title),
            escape_html(&section.dict_id)
        ));
        html.push_str(r#"<div class="mdict-dict-body">"#);
        html.push_str(&section.body);
        html.push_str("</div></section>");
    }

    html.push_str("</div>");
    html
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn escape_html_attr(s: &str) -> String {
    escape_html(s)
}

/// 返回以指定前缀开头的词条列表（用于搜索建议）
/// 使用 FTS5 + bm25 排序
pub fn suggest(state: &AppState, prefix: String, limit: usize) -> Result<Vec<String>, String> {
    let trimmed = prefix.trim();
    if trimmed.len() < 2 {
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
                // Fall back to prefix search
                let fallback = prefix_search(&conn, &prefix_lower, query_limit);
                for (rank, word) in fallback.into_iter().enumerate() {
                    if !is_suggest_candidate(&word) {
                        continue;
                    }
                    let word_lower = word.to_lowercase();
                    let score = calculate_fts_score(&prefix_lower, &word_lower, &word, rank as f64);
                    match scores.get(&word) {
                        Some(existing) if *existing >= score => {}
                        _ => {
                            scores.insert(word, score);
                        }
                    }
                }
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
                // Fall back to prefix search
                let fallback = prefix_search(&conn, &prefix_lower, query_limit);
                for (rank, word) in fallback.into_iter().enumerate() {
                    if !is_suggest_candidate(&word) {
                        continue;
                    }
                    let word_lower = word.to_lowercase();
                    let score = calculate_fts_score(&prefix_lower, &word_lower, &word, rank as f64);
                    match scores.get(&word) {
                        Some(existing) if *existing >= score => {}
                        _ => {
                            scores.insert(word, score);
                        }
                    }
                }
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
            match scores.get(&word) {
                Some(existing) if *existing >= score => {}
                _ => {
                    scores.insert(word, score);
                }
            }
            fts_rows += 1;
        }

        if fts_rows == 0 {
            // Some DBs might not have MDX_FTS populated; fall back to prefix search.
            let fallback = prefix_search(&conn, &prefix_lower, query_limit);
            for (rank, word) in fallback.into_iter().enumerate() {
                if !is_suggest_candidate(&word) {
                    continue;
                }
                let word_lower = word.to_lowercase();
                let score = calculate_fts_score(&prefix_lower, &word_lower, &word, rank as f64);
                match scores.get(&word) {
                    Some(existing) if *existing >= score => {}
                    _ => {
                        scores.insert(word, score);
                    }
                }
            }
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
        if ch.is_alphanumeric() {
            cleaned.push(ch);
        } else if ch.is_whitespace() {
            cleaned.push(' ');
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

    // Exact match gets highest score
    if word_lower == prefix {
        score += 1000;
    }

    // Exact prefix match (case-sensitive) gets bonus
    if word_lower.starts_with(prefix) {
        score += 100;
    }

    // Word starts exactly with prefix (case-sensitive original)
    if word.starts_with(&prefix.chars().next().unwrap().to_uppercase().to_string()) {
        score += 20;
    }

    // Shorter words are generally more relevant
    score += 50 - word.len().min(50) as i32;

    // Single words (no spaces) are preferred
    if !word.contains(' ') {
        score += 30;
    }

    // Penalize entries with numbers
    if word.chars().any(|c| c.is_numeric()) {
        score -= 20;
    }

    // Penalize entries with special punctuation
    if word.contains(',') || word.contains(';') || word.contains(':') {
        score -= 10;
    }

    // Prefer better bm25 score (lower is better)
    let bm25_bonus = (-bm25_score * 1000.0).round() as i32;
    score + bm25_bonus
}
