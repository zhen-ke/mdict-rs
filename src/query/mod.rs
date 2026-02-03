use rusqlite::{Connection, named_params};
use tracing::{debug, error, info, warn};
use crate::config::{get_db_connection, MDX_RESOURCE_FILES, MDX_TEXT_FILES, get_mdx_reader};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{LazyLock, Mutex};

pub fn query(word: String) -> Result<(Vec<u8>, String), String> {
    query_internal(word, 0)
}

/// Query with trace - returns the redirect chain and final word
/// Used for debugging @@@LINK depth
pub fn query_with_trace(word: String) -> Result<(Vec<String>, String), String> {
    let mut chain = vec![word.clone()];
    let mut current = word;

    for _ in 0..10 {  // Max 10 redirects
        match get_link_target(&current) {
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
fn get_link_target(word: &str) -> Option<String> {
    for file in MDX_TEXT_FILES.iter() {
        let conn = match get_db_connection(file) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let mut stmt = conn
            .prepare("select record_offset, record_length, block_offset, block_size, block_dsize from MDX_INDEX WHERE text= :word limit 1;")
            .ok()?;

        let mut rows = stmt.query(named_params! { ":word": word }).ok()?;

        if let Some(row) = rows.next().ok()? {
            let record_offset: usize = row.get(0).unwrap_or(0);
            let record_length: usize = row.get(1).unwrap_or(0);
            let block_offset: usize = row.get(2).unwrap_or(0);
            let block_csize: usize = row.get(3).unwrap_or(0);
            let block_dsize: usize = row.get(4).unwrap_or(0);

            let reader = get_mdx_reader(file).ok()?;
            let data = reader.read_record(block_offset, block_csize, block_dsize, record_offset, record_length).ok()?;

            if let Ok(text) = String::from_utf8(data) {
                let first_line = text.lines().next().unwrap_or("").trim();
                if first_line.starts_with("@@@LINK=") {
                    let linked_word: String = first_line
                        .trim_start_matches("@@@LINK=")
                        .chars()
                        .filter(|c| !c.is_control())
                        .collect::<String>()
                        .trim()
                        .to_string();
                    if !linked_word.is_empty() {
                        return Some(linked_word);
                    }
                }
            }
        }
    }
    None
}

fn is_resource_key(word: &str) -> bool {
    word.starts_with('\\') || word.starts_with('/')
}

/// Internal query with redirect depth limit to prevent infinite loops
fn query_internal(word: String, depth: u8) -> Result<(Vec<u8>, String), String> {
    // Prevent infinite redirect loops
    if depth > 5 {
        return Err("too many redirects".to_string());
    }

    let w = word.clone();
    let files = if is_resource_key(&w) {
        &MDX_RESOURCE_FILES
    } else {
        &MDX_TEXT_FILES
    };

    for file in files.iter() {
        let conn = match get_db_connection(file) {
            Ok(c) => c,
            Err(e) => {
                debug!("skip dict {}: {}", file, e);
                continue;
            }
        };
        let mut stmt = conn
            .prepare("select record_offset, record_length, block_offset, block_size, block_dsize from MDX_INDEX WHERE text= :word limit 1;")
            .map_err(|e| e.to_string())?;
        debug!("query params={}, dict={}", &w, file);

        let mut rows = stmt.query(named_params! { ":word": w }).map_err(|e| e.to_string())?;

        if let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let record_offset: usize = row.get(0).unwrap_or(0);
            let record_length: usize = row.get(1).unwrap_or(0);
            let block_offset: usize = row.get(2).unwrap_or(0);
            let block_csize: usize = row.get(3).unwrap_or(0);
            let block_dsize: usize = row.get(4).unwrap_or(0);

            // Access cached reader
            let reader = get_mdx_reader(file).map_err(|e| e.to_string())?;

            let data = reader.read_record(block_offset, block_csize, block_dsize, record_offset, record_length).map_err(|e| {
                let err = format!("failed to read record: {}", e);
                error!("{}", err);
                err
            })?;

            // Check for @@@LINK= redirect
            let text = String::from_utf8_lossy(&data);
            let first_line = text.lines().next().unwrap_or("").trim();
            if first_line.starts_with("@@@LINK=") {
                let linked_word: String = first_line
                    .trim_start_matches("@@@LINK=")
                    .chars()
                    .filter(|c| !c.is_control())  // Remove all control characters
                    .collect::<String>()
                    .trim()
                    .to_string();
                if !linked_word.is_empty() {
                    info!("following @@@LINK redirect: {} -> {}", w, linked_word);
                    return query_internal(linked_word, depth + 1);
                }
            }

            // Determine Content-Type
            let content_type = if is_resource_key(&w) {
                let path = Path::new(&w);
                match path.extension().and_then(|s| s.to_str()) {
                    Some("jpg") | Some("jpeg") => "image/jpeg",
                    Some("png") => "image/png",
                    Some("gif") => "image/gif",
                    Some("css") => "text/css",
                    Some("js") => "text/javascript",
                    Some("wav") => "audio/wav",
                    Some("mp3") => "audio/mp3",
                    _ => "application/octet-stream"
                }
            } else {
                 "text/html"
            };

            return Ok((data, content_type.to_string()));
        }
    }
    Err("not found".to_string())
}

/// FTS ready cache (per dict)
static FTS_READY: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// 返回以指定前缀开头的词条列表（用于搜索建议）
/// 使用 FTS5 + bm25 排序
pub fn suggest(prefix: String, limit: usize) -> Result<Vec<String>, String> {
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

    for file in MDX_TEXT_FILES.iter() {
        let conn = match get_db_connection(file) {
            Ok(c) => c,
            Err(e) => {
                debug!("skip dict {}: {}", file, e);
                continue;
            }
        };

        if ensure_fts(&conn, file) {
            let mut stmt = match conn.prepare(
                "SELECT text, bm25(MDX_FTS) as score FROM MDX_FTS
                 WHERE MDX_FTS MATCH :query
                 ORDER BY score
                 LIMIT :limit;"
            ) {
                Ok(s) => s,
                Err(e) => {
                    warn!("FTS prepare failed for {}: {}", file, e);
                    continue;
                }
            };

            let rows = match stmt.query_map(
                named_params! { ":query": fts_query.as_str(), ":limit": query_limit as i64 },
                |row| {
                    let text: String = row.get(0)?;
                    let score: f64 = row.get(1)?;
                    Ok((text, score))
                }
            ) {
                Ok(r) => r,
                Err(e) => {
                    warn!("FTS query failed for {}: {}", file, e);
                    continue;
                }
            };

            for row in rows {
                let Ok((word, bm25_score)) = row else { continue };
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
            }
        } else {
            warn!("FTS not available for {}, falling back to prefix search", file);
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
    candidates.sort_by(|a, b| {
        match b.1.cmp(&a.1) {
            std::cmp::Ordering::Equal => {
                match a.0.len().cmp(&b.0.len()) {
                    std::cmp::Ordering::Equal => a.0.to_lowercase().cmp(&b.0.to_lowercase()),
                    other => other,
                }
            }
            other => other,
        }
    });

    Ok(candidates
        .into_iter()
        .take(limit)
        .map(|(word, _)| word)
        .collect())
}

fn ensure_fts(conn: &Connection, dict: &str) -> bool {
    {
        let ready = FTS_READY
            .lock()
            .expect("FTS_READY mutex poisoned");
        if ready.contains(dict) {
            return true;
        }
    }

    if let Err(e) = conn.execute(
        "create virtual table if not exists MDX_FTS using fts5(text, tokenize='unicode61 remove_diacritics 2')",
        [],
    ) {
        warn!("FTS5 not available: {}", e);
        return false;
    }

    let count: i64 = match conn.query_row("select count(*) from MDX_FTS", [], |row| row.get(0)) {
        Ok(c) => c,
        Err(e) => {
            warn!("FTS count failed: {}", e);
            return false;
        }
    };

    if count == 0 {
        if let Err(e) = conn.execute(
            "insert into MDX_FTS(text)
             select text from MDX_INDEX
             where text not like '\\%'
               and text not like '/%'
               and text not like '@%'
               and text not like '%@%'
               and text not like '0%'
               and text not like '1%'
               and text not like '2%'
               and text not like '3%'
               and text not like '4%'
               and text not like '5%'
               and text not like '6%'
               and text not like '7%'
               and text not like '8%'
               and text not like '9%'
               and instr(text, ' ') = 0
               and length(text) < 50;",
            [],
        ) {
            warn!("FTS backfill failed: {}", e);
            return false;
        }
    }

    FTS_READY
        .lock()
        .expect("FTS_READY mutex poisoned")
        .insert(dict.to_string());
    true
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
         LIMIT :limit;"
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let rows = match stmt.query_map(
        named_params! { ":pattern": pattern, ":limit": limit as i64 },
        |row| row.get::<_, String>(0)
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
