use std::path::Path;
use std::sync::OnceLock;

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use regex::Regex;

static ATTR_RE: OnceLock<Regex> = OnceLock::new();

pub(crate) fn rewrite_html(html: &str, dict_id: &str) -> String {
    let re = ATTR_RE.get_or_init(|| {
        Regex::new(r#"(?i)\b(src|href)=["']([^"']+)["']"#).expect("valid src/href attribute regex")
    });

    re.replace_all(html, |caps: &regex::Captures| {
        let attr = &caps[1];
        let value = &caps[2];
        let full_match = &caps[0];

        let Some(new_val) = rewrite_link(attr, value, dict_id) else {
            return full_match.to_string();
        };

        format!(r#"{}="{}""#, attr, new_val)
    })
    .into_owned()
}

fn rewrite_link(attr: &str, value: &str, dict_id: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || is_external_link(value) {
        return None;
    }

    if let Some(target) = value.strip_prefix("sound://") {
        let (base, suffix) = split_base_and_suffix(target);
        let path = normalize_resource_path(base);
        if path.is_empty() {
            return None;
        }
        return Some(format!("/dict/{}/audio/{}{}", dict_id, path, suffix));
    }

    if let Some(target) = value.strip_prefix("entry://") {
        let (base, _) = split_base_and_suffix(target);
        let entry_word = normalize_entry_word(base);
        if entry_word.is_empty() {
            return None;
        }
        return Some(format!(
            "/dict/{}/entry/{}",
            dict_id,
            encode_entry_word(&entry_word)
        ));
    }

    let (base, suffix) = split_base_and_suffix(value);
    if base.is_empty() {
        return None;
    }

    let attr_lc = attr.to_ascii_lowercase();
    if attr_lc == "src" || looks_like_resource_link(base) {
        let path = normalize_resource_path(base);
        if path.is_empty() {
            return None;
        }
        return Some(format!("/dict/{}/res/{}{}", dict_id, path, suffix));
    }

    let entry_word = normalize_entry_word(base);
    if entry_word.is_empty() {
        return None;
    }
    Some(format!(
        "/dict/{}/entry/{}",
        dict_id,
        encode_entry_word(&entry_word)
    ))
}

fn is_external_link(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("data:")
        || lower.starts_with("javascript:")
        || lower.starts_with("mailto:")
        || lower.starts_with("tel:")
        || lower.starts_with("about:")
        || lower.starts_with("//")
        || lower.starts_with('#')
}

fn split_base_and_suffix(url: &str) -> (&str, &str) {
    let query_pos = url.find('?');
    let fragment_pos = url.find('#');
    let split_pos = match (query_pos, fragment_pos) {
        (Some(q), Some(f)) => Some(q.min(f)),
        (Some(q), None) => Some(q),
        (None, Some(f)) => Some(f),
        (None, None) => None,
    };

    if let Some(idx) = split_pos {
        (&url[..idx], &url[idx..])
    } else {
        (url, "")
    }
}

fn normalize_resource_path(path: &str) -> String {
    let mut cleaned = path.trim().replace('\\', "/");
    while cleaned.starts_with("./") {
        cleaned = cleaned[2..].to_string();
    }
    while cleaned.starts_with('/') {
        cleaned = cleaned[1..].to_string();
    }

    cleaned
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

fn normalize_entry_word(word: &str) -> String {
    word.trim().trim_matches('/').trim_matches('\\').to_string()
}

fn encode_entry_word(word: &str) -> String {
    utf8_percent_encode(word, NON_ALPHANUMERIC).to_string()
}

fn looks_like_resource_link(path: &str) -> bool {
    if path.starts_with("./")
        || path.starts_with("../")
        || path.starts_with('\\')
        || path.starts_with('/')
        || path.contains('\\')
    {
        let trimmed = path.trim_start_matches('/').trim_start_matches('\\');
        if is_single_segment_plain_entry(trimmed) {
            return false;
        }
        return true;
    }

    if path.contains('/') {
        return true;
    }

    has_resource_extension(path)
}

fn is_single_segment_plain_entry(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    if path.contains('/') || path.contains('\\') {
        return false;
    }
    !has_resource_extension(path)
}

fn has_resource_extension(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    matches!(
        ext.as_deref(),
        Some("jpg")
            | Some("jpeg")
            | Some("png")
            | Some("gif")
            | Some("bmp")
            | Some("webp")
            | Some("svg")
            | Some("ico")
            | Some("css")
            | Some("js")
            | Some("wav")
            | Some("mp3")
            | Some("ogg")
            | Some("oga")
            | Some("flac")
            | Some("aac")
            | Some("m4a")
            | Some("mp4")
            | Some("webm")
            | Some("ttf")
            | Some("otf")
            | Some("woff")
            | Some("woff2")
            | Some("eot")
            | Some("pdf")
    )
}

#[cfg(test)]
mod tests {
    use super::rewrite_html;

    #[test]
    fn rewrites_sound_link_to_audio_route() {
        let html = r#"<a href="sound://uk/us.mp3">play</a>"#;
        let out = rewrite_html(html, "d1");
        assert!(out.contains(r#"href="/dict/d1/audio/uk/us.mp3""#));
    }

    #[test]
    fn rewrites_entry_scheme_to_entry_route() {
        let html = r#"<a href="entry://hello world">x</a>"#;
        let out = rewrite_html(html, "d1");
        assert!(out.contains(r#"href="/dict/d1/entry/hello%20world""#));
    }

    #[test]
    fn rewrites_plain_href_word_to_entry_route() {
        let html = r#"<a href="dictionary">x</a>"#;
        let out = rewrite_html(html, "d1");
        assert!(out.contains(r#"href="/dict/d1/entry/dictionary""#));
    }

    #[test]
    fn rewrites_src_to_resource_route() {
        let html = r#"<img src="\img\foo.png">"#;
        let out = rewrite_html(html, "d1");
        assert!(out.contains(r#"src="/dict/d1/res/img/foo.png""#));
    }

    #[test]
    fn keeps_external_links_unchanged() {
        let html = r#"<a href="https://example.com">x</a>"#;
        let out = rewrite_html(html, "d1");
        assert_eq!(html, out);
    }
}
