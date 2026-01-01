use rusqlite::named_params;
use tracing::{info, error};
use crate::config::{get_db_connection, MDX_FILES, get_mdx_reader, get_fst_index};
use std::path::Path;
use fst::automaton::Levenshtein;
use fst::{IntoStreamer, Streamer};

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
    for file in MDX_FILES.iter() {
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
    for file in MDX_FILES.iter() {
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
/// 结合前缀匹配和 FST 模糊搜索
pub fn suggest(prefix: String, limit: usize) -> Result<Vec<String>, String> {
    if prefix.is_empty() || prefix.len() < 2 {
        return Ok(vec![]);
    }

    let prefix_lower = prefix.to_lowercase();
    let mut suggestions = std::collections::HashSet::new();
    let mut candidates: Vec<(String, i32)> = Vec::new(); // (word, score)

    // 1. 前缀匹配（高优先级）
    for file in MDX_FILES.iter() {
        if file.ends_with(".mdd") {
            continue;
        }

        if let Ok(conn) = get_db_connection(file) {
            let query_limit = limit * 10;
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
                    if word.contains('/')
                        || word.contains('\\')
                        || word.contains('<')
                        || word.contains('>')
                        || word.starts_with('-')
                        || word.starts_with('.')
                    {
                        continue;
                    }

                    if word.len() > 40 {
                        continue;
                    }

                    if suggestions.insert(word.clone()) {
                        let word_lower = word.to_lowercase();
                        let score = calculate_score(&prefix_lower, &word_lower, &word);
                        candidates.push((word, score));
                    }
                }
            }
        }
    }

    // 2. FST 模糊搜索（补充结果，低优先级）
    // 只有当前缀匹配结果不足时才进行模糊搜索
    if candidates.len() < limit && prefix.len() >= 3 {
        info!("Prefix matches: {}, adding fuzzy search results", candidates.len());

        let fuzzy_results = fuzzy_search(&prefix, limit);

        for (word, edit_dist) in fuzzy_results {
            // 跳过已有的结果
            if suggestions.contains(&word) {
                continue;
            }

            // 跳过特殊字符
            if word.contains('/') || word.contains('\\') || word.contains('<') || word.contains('>') {
                continue;
            }

            if suggestions.insert(word.clone()) {
                let score = calculate_fuzzy_score(&prefix, &word, edit_dist);
                candidates.push((word, score));
            }
        }
    }

    // 排序：分数降序，长度升序，字母顺序
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

/// 根据查询长度确定模糊搜索的编辑距离
/// 短词使用更严格的匹配，长词允许更多错误
fn get_fuzzy_distance(query_len: usize) -> u32 {
    match query_len {
        0..=2 => 0,   // 太短，不做模糊匹配
        3..=4 => 1,   // 允许 1 个编辑距离
        5..=7 => 2,   // 允许 2 个编辑距离
        _ => 2,       // 最多 2 个编辑距离
    }
}

/// 使用 FST 进行模糊搜索
/// 返回匹配的词条列表（已排序）
fn fuzzy_search(query: &str, limit: usize) -> Vec<(String, u32)> {
    let query_lower = query.to_lowercase();
    let distance = get_fuzzy_distance(query_lower.len());

    // 如果距离为 0，不进行模糊搜索
    if distance == 0 {
        return vec![];
    }

    let mut results: Vec<(String, u32)> = Vec::new();

    // 遍历所有词典的 FST 索引
    for file in MDX_FILES.iter() {
        if file.ends_with(".mdd") {
            continue;
        }

        if let Some(fst_map) = get_fst_index(file) {
            // 创建 Levenshtein 自动机
            let automaton = match Levenshtein::new(&query_lower, distance) {
                Ok(a) => a,
                Err(_) => continue,
            };

            // 搜索匹配项
            let mut stream = fst_map.search(&automaton).into_stream();

            while let Some((key, _)) = stream.next() {
                if let Ok(word) = String::from_utf8(key.to_vec()) {
                    // 跳过特殊词条
                    if word.starts_with('\\') || word.starts_with('/') || word.starts_with('@') {
                        continue;
                    }
                    if word.len() > 40 {
                        continue;
                    }

                    // 计算实际编辑距离（用于排序）
                    let edit_dist = levenshtein_distance(&query_lower, &word);
                    results.push((word, edit_dist));

                    // 限制结果数量避免过多计算
                    if results.len() >= limit * 3 {
                        break;
                    }
                }
            }
        }
    }

    // 按编辑距离排序
    results.sort_by_key(|(_, dist)| *dist);
    results.truncate(limit);
    results
}

/// 简单的 Levenshtein 距离计算
fn levenshtein_distance(s1: &str, s2: &str) -> u32 {
    let len1 = s1.chars().count();
    let len2 = s2.chars().count();

    if len1 == 0 {
        return len2 as u32;
    }
    if len2 == 0 {
        return len1 as u32;
    }

    let s1_chars: Vec<char> = s1.chars().collect();
    let s2_chars: Vec<char> = s2.chars().collect();

    let mut prev_row: Vec<usize> = (0..=len2).collect();
    let mut curr_row = vec![0; len2 + 1];

    for i in 1..=len1 {
        curr_row[0] = i;
        for j in 1..=len2 {
            let cost = if s1_chars[i - 1] == s2_chars[j - 1] { 0 } else { 1 };
            curr_row[j] = (prev_row[j] + 1)
                .min(curr_row[j - 1] + 1)
                .min(prev_row[j - 1] + cost);
        }
        std::mem::swap(&mut prev_row, &mut curr_row);
    }

    prev_row[len2] as u32
}

/// 模糊搜索的分数计算
/// 目标: "helo" 搜索时 "hello" 应该排在 "halo" 前面
fn calculate_fuzzy_score(query: &str, word: &str, edit_distance: u32) -> i32 {
    let query_lower = query.to_lowercase();
    let word_lower = word.to_lowercase();
    let mut score = 100; // 基础分

    // 1. 编辑距离惩罚 (距离越大，扣分越多)
    score -= (edit_distance * 30) as i32;

    // 2. 精确匹配 - 最高优先级
    if word_lower == query_lower {
        return 2000;
    }

    // 3. 共同前缀加分 - 关键改进!
    // "helo" vs "hello": 共同前缀 "hel" = 3 字符
    // "helo" vs "halo": 共同前缀 "h" = 1 字符
    let common_prefix_len = query_lower
        .chars()
        .zip(word_lower.chars())
        .take_while(|(a, b)| a == b)
        .count();
    score += (common_prefix_len * 25) as i32;

    // 4. 查询被包含在词中 (如 "helo" 的字符大部分在 "hello" 中)
    let query_chars: std::collections::HashSet<char> = query_lower.chars().collect();
    let word_chars: std::collections::HashSet<char> = word_lower.chars().collect();
    let common_chars = query_chars.intersection(&word_chars).count();
    let coverage = (common_chars as f32 / query_chars.len().max(1) as f32 * 50.0) as i32;
    score += coverage;

    // 5. 词长与查询长度比例 - 越接近越好
    // "helo"(4) vs "hello"(5) 比例 = 0.8, 加分
    // "helo"(4) vs "Heloise"(7) 比例 = 0.57, 少加分
    let len_ratio = query.len() as f32 / word.len().max(1) as f32;
    let len_bonus = (len_ratio.min(1.0) * 30.0) as i32;
    score += len_bonus;

    // 6. 以查询开头的词加分
    if word_lower.starts_with(&query_lower) {
        score += 80;
    }

    // 7. 单词优先（无空格）
    if !word.contains(' ') {
        score += 15;
    }

    // 8. 惩罚过长的词
    if word.len() > query.len() + 3 {
        score -= ((word.len() - query.len() - 3) * 5) as i32;
    }

    score
}
