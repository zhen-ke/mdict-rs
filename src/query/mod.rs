use rusqlite::named_params;
use tracing::{info, error};
use crate::config::{get_db_connection, MDX_FILES, get_mdx_reader};
use std::path::Path;

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
    for file in MDX_FILES {
        if file.ends_with(".mdd") {
            continue;
        }

        let conn = get_db_connection(file).ok()?;
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

/// Internal query with redirect depth limit to prevent infinite loops
fn query_internal(word: String, depth: u8) -> Result<(Vec<u8>, String), String> {
    // Prevent infinite redirect loops
    if depth > 5 {
        return Err("too many redirects".to_string());
    }

    let w = word.clone();
    for file in MDX_FILES {
        let conn = get_db_connection(file).map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("select record_offset, record_length, block_offset, block_size, block_dsize from MDX_INDEX WHERE text= :word limit 1;")
            .map_err(|e| e.to_string())?;
        info!("query params={}, dict={}", &w, file);

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
            if let Ok(text) = String::from_utf8(data.clone()) {
                // Get first non-empty line and check for @@@LINK=
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
            }

            // Determine Content-Type
            let content_type = if w.starts_with("\\") || w.starts_with("/") {
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

/// 返回以指定前缀开头的词条列表（用于搜索建议）
pub fn suggest(prefix: String, limit: usize) -> Result<Vec<String>, String> {
    if prefix.is_empty() || prefix.len() < 2 {
        return Ok(vec![]);
    }

    let prefix_lower = prefix.to_lowercase();
    let mut suggestions = std::collections::HashSet::new();
    let mut candidates: Vec<(String, i32)> = Vec::new(); // (word, score)

    // Collect candidates from all dictionaries
    for file in MDX_FILES.iter() {
        if file.ends_with(".mdd") {
            continue;
        }

        if let Ok(conn) = get_db_connection(file) {
            // Use GLOB for case-sensitive prefix match first, then LIKE as fallback
            // Query more candidates for better ranking
            let query_limit = limit * 10;

            // Try case-insensitive prefix match using LOWER()
            let pattern = format!("{}%", prefix_lower);

            let mut stmt = match conn.prepare(
                "SELECT text FROM MDX_INDEX
                 WHERE LOWER(text) LIKE :pattern
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
                Err(_) => continue,
            };

            let rows = match stmt.query_map(
                named_params! { ":pattern": pattern, ":limit": query_limit as i64 },
                |row| row.get::<_, String>(0)
            ) {
                Ok(r) => r,
                Err(_) => continue,
            };

            for row in rows {
                if let Ok(word) = row {
                    // Skip entries with special characters
                    if word.contains('/')
                        || word.contains('\\')
                        || word.contains('<')
                        || word.contains('>')
                        || word.starts_with('-')
                        || word.starts_with('.')
                    {
                        continue;
                    }

                    // Skip very long entries (likely phrases or sentences)
                    if word.len() > 40 {
                        continue;
                    }

                    if suggestions.insert(word.clone()) {
                        // Calculate relevance score
                        let word_lower = word.to_lowercase();
                        let score = calculate_score(&prefix_lower, &word_lower, &word);
                        candidates.push((word, score));
                    }
                }
            }
        }
    }

    // Sort by score (higher is better), then by length, then alphabetically
    candidates.sort_by(|a, b| {
        match b.1.cmp(&a.1) {  // score descending
            std::cmp::Ordering::Equal => {
                match a.0.len().cmp(&b.0.len()) {  // length ascending
                    std::cmp::Ordering::Equal => a.0.to_lowercase().cmp(&b.0.to_lowercase()),
                    other => other,
                }
            }
            other => other,
        }
    });

    // Truncate to requested limit and extract words
    let result: Vec<String> = candidates
        .into_iter()
        .take(limit)
        .map(|(word, _)| word)
        .collect();

    Ok(result)
}

/// Calculate relevance score for a suggestion
fn calculate_score(prefix: &str, word_lower: &str, word: &str) -> i32 {
    let mut score = 0;

    // Exact match gets highest score
    if word_lower == prefix {
        score += 1000;
    }

    // Exact prefix match (case-sensitive) gets bonus
    if word.to_lowercase().starts_with(prefix) {
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

    score
}
