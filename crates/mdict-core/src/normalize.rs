const MAX_QUERY_CANDIDATES: usize = 32;

/// 规范化归一：小写 + 折叠拉丁变音符号 + 折叠标点为空格 + 合并空白。
///
/// 用于索引时计算 `normalized` 列与查询时的规范化候选，使 "Café—menu"、
/// "cafe menu"、"CAFE MENU" 等大小写/变音/标点/空白变体归一到同一键。
/// 单独一次 `WHERE normalized = ?` 即可命中这些变体，无需展开 32 个候选。
/// 注意：词形回退（running→run）不是归一化能覆盖的，仍走候选展开。
pub fn canonical_normalize(input: &str) -> String {
    let lower = input.to_lowercase();
    let folded = fold_latin_diacritics(&lower);
    fold_punctuation_to_space(&folded)
}

/// 计算字典序上界，用于 `normalized` 列的前缀区间扫描
/// (`normalized >= lo AND normalized < hi`)。
/// 将末位 codepoint +1；若溢出则返回 `None`（调用方退化为只取 `>= lo`）。
pub fn prefix_upper(prefix: &str) -> Option<String> {
    let mut chars: Vec<char> = prefix.chars().collect();
    let last = chars.last_mut()?;
    let new_val = u32::from(*last).checked_add(1)?;
    *last = char::from_u32(new_val)?;
    Some(chars.into_iter().collect())
}

pub fn entry_query_candidates(word: &str) -> Vec<String> {
    let mut candidates = Vec::with_capacity(16);

    let raw = word.trim();
    if raw.is_empty() {
        return candidates;
    }

    push_unique(&mut candidates, raw.to_string());

    let stripped = raw
        .trim_start_matches('/')
        .trim_start_matches('\\')
        .trim_end_matches('/')
        .trim_end_matches('\\');
    if !stripped.is_empty() {
        push_unique(&mut candidates, stripped.to_string());
    }

    let normalized = normalize_lookup_key(stripped);
    if !normalized.is_empty() {
        push_unique(&mut candidates, normalized.clone());
    }

    let folded = fold_latin_diacritics(&normalized);
    if !folded.is_empty() {
        push_unique(&mut candidates, folded.clone());
    }

    let punctuation_folded = fold_punctuation_to_space(&folded);
    if !punctuation_folded.is_empty() {
        push_unique(&mut candidates, punctuation_folded.clone());
    }

    let compact = punctuation_folded.replace(' ', "");
    if !compact.is_empty() {
        push_unique(&mut candidates, compact);
    }

    let seeds = candidates.clone();
    for seed in seeds {
        for lemma in english_lemma_candidates(&seed) {
            push_unique(&mut candidates, lemma);
            if candidates.len() >= MAX_QUERY_CANDIDATES {
                return candidates;
            }
        }
    }

    candidates
}

fn normalize_lookup_key(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_space = true;

    for ch in input.chars() {
        if ch.is_control() {
            continue;
        }

        if is_space_like(ch) {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
            continue;
        }

        let ch = normalize_quote_and_dash(ch);
        for lowered in ch.to_lowercase() {
            out.push(lowered);
            prev_space = false;
        }
    }

    out.trim().to_string()
}

fn fold_latin_diacritics(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => out.push('a'),
            'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => out.push('c'),
            'ď' | 'đ' => out.push('d'),
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => out.push('e'),
            'ƒ' => out.push('f'),
            'ĝ' | 'ğ' | 'ġ' | 'ģ' => out.push('g'),
            'ĥ' | 'ħ' => out.push('h'),
            'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => out.push('i'),
            'ĵ' => out.push('j'),
            'ķ' => out.push('k'),
            'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => out.push('l'),
            'ñ' | 'ń' | 'ņ' | 'ň' | 'ŉ' => out.push('n'),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => out.push('o'),
            'ŕ' | 'ŗ' | 'ř' => out.push('r'),
            'ś' | 'ŝ' | 'ş' | 'š' => out.push('s'),
            'ţ' | 'ť' | 'ŧ' => out.push('t'),
            'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => out.push('u'),
            'ŵ' => out.push('w'),
            'ý' | 'ÿ' | 'ŷ' => out.push('y'),
            'ź' | 'ż' | 'ž' => out.push('z'),
            'æ' => out.push_str("ae"),
            'œ' => out.push_str("oe"),
            'ß' => out.push_str("ss"),
            _ => out.push(ch),
        }
    }
    out
}

fn fold_punctuation_to_space(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_space = true;
    for ch in input.chars() {
        if ch.is_alphanumeric() || ch == '\'' {
            out.push(ch);
            prev_space = false;
            continue;
        }
        if !prev_space {
            out.push(' ');
            prev_space = true;
        }
    }
    out.trim().to_string()
}

fn english_lemma_candidates(input: &str) -> Vec<String> {
    let s = input.trim();
    if s.is_empty() || s.contains(' ') {
        return Vec::new();
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '\'' || c == '-')
    {
        return Vec::new();
    }

    let lower = s.to_ascii_lowercase();
    let mut out = Vec::with_capacity(10);
    push_unique(&mut out, lower.clone());

    if let Some(irr) = irregular_lemma(&lower) {
        push_unique(&mut out, irr.to_string());
    }

    if let Some(stem) = lower.strip_suffix("'s") {
        push_unique(&mut out, stem.to_string());
    }
    if let Some(stem) = lower.strip_suffix("s'") {
        push_unique(&mut out, stem.to_string());
    }

    if lower.ends_with("ies") && lower.len() > 4 {
        push_unique(&mut out, format!("{}y", &lower[..lower.len() - 3]));
    }

    if lower.ends_with("ves") && lower.len() > 4 {
        push_unique(&mut out, format!("{}f", &lower[..lower.len() - 3]));
        push_unique(&mut out, format!("{}fe", &lower[..lower.len() - 3]));
    }

    if lower.ends_with("es") && lower.len() > 3 {
        push_unique(&mut out, lower[..lower.len() - 2].to_string());
    }

    if lower.ends_with('s') && lower.len() > 3 {
        push_unique(&mut out, lower[..lower.len() - 1].to_string());
    }

    if lower.ends_with("ing") && lower.len() > 5 {
        let stem = &lower[..lower.len() - 3];
        push_unique(&mut out, stem.to_string());
        if let Some(trimmed) = trim_doubled_consonant(stem) {
            push_unique(&mut out, trimmed.to_string());
        }
        push_unique(&mut out, format!("{stem}e"));
    }

    if lower.ends_with("ed") && lower.len() > 4 {
        let stem = &lower[..lower.len() - 2];
        push_unique(&mut out, stem.to_string());
        if let Some(trimmed) = trim_doubled_consonant(stem) {
            push_unique(&mut out, trimmed.to_string());
        }
        push_unique(&mut out, format!("{stem}e"));
    }

    if lower.ends_with("er") && lower.len() > 4 {
        let stem = &lower[..lower.len() - 2];
        push_unique(&mut out, stem.to_string());
    }

    if lower.ends_with("est") && lower.len() > 5 {
        let stem = &lower[..lower.len() - 3];
        push_unique(&mut out, stem.to_string());
    }

    out
}

fn irregular_lemma(word: &str) -> Option<&'static str> {
    match word {
        "went" | "gone" => Some("go"),
        "did" | "done" => Some("do"),
        "was" | "were" | "is" | "are" | "am" => Some("be"),
        "better" | "best" => Some("good"),
        "worse" | "worst" => Some("bad"),
        "children" => Some("child"),
        "mice" => Some("mouse"),
        "teeth" => Some("tooth"),
        "feet" => Some("foot"),
        "men" => Some("man"),
        "women" => Some("woman"),
        _ => None,
    }
}

fn trim_doubled_consonant(stem: &str) -> Option<&str> {
    let mut chars = stem.chars().rev();
    let last = chars.next()?;
    let prev = chars.next()?;
    if last == prev && matches!(last, 'b'..='z') {
        return stem.get(..stem.len().saturating_sub(last.len_utf8()));
    }
    None
}

fn normalize_quote_and_dash(ch: char) -> char {
    match ch {
        '\u{2018}' | '\u{2019}' | '\u{201B}' | '\u{2032}' => '\'',
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
        _ => ch,
    }
}

fn is_space_like(ch: char) -> bool {
    ch.is_whitespace() || ch == '\u{00A0}' || ch == '\u{3000}'
}

fn push_unique(list: &mut Vec<String>, value: String) {
    if value.is_empty() || list.iter().any(|existing| existing == &value) {
        return;
    }
    if list.len() < MAX_QUERY_CANDIDATES {
        list.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::entry_query_candidates;

    #[test]
    fn builds_normalized_and_lemma_candidates() {
        let c = entry_query_candidates(" Running ");
        assert!(c.contains(&"Running".to_string()));
        assert!(c.contains(&"running".to_string()));
        assert!(c.contains(&"run".to_string()));
    }

    #[test]
    fn folds_diacritics_and_punctuation() {
        let c = entry_query_candidates("Café—menu");
        assert!(c.contains(&"café-menu".to_string()));
        assert!(c.contains(&"cafe-menu".to_string()));
        assert!(c.contains(&"cafe menu".to_string()));
    }

    #[test]
    fn has_irregular_lemma() {
        let c = entry_query_candidates("went");
        assert!(c.contains(&"go".to_string()));
    }

    #[test]
    fn canonical_normalize_folds_case_diacritics_and_punctuation() {
        use super::canonical_normalize;
        assert_eq!(canonical_normalize("Café—menu"), "cafe menu");
        assert_eq!(canonical_normalize("CAFE MENU"), "cafe menu");
        assert_eq!(canonical_normalize("naïve"), "naive");
        assert_eq!(canonical_normalize("it's"), "it's");
        assert_eq!(canonical_normalize("  Hello,World  "), "hello world");
    }

    #[test]
    fn prefix_upper_increments_last_codepoint() {
        use super::prefix_upper;
        assert_eq!(prefix_upper("cafe").as_deref(), Some("caff"));
        assert_eq!(prefix_upper("a").as_deref(), Some("b"));
        // CJK 末位码点 +1 仍是合法 char
        assert!(prefix_upper("你好").is_some());
        // 最大码点溢出 → None
        assert_eq!(prefix_upper("\u{10FFFF}"), None);
    }
}
