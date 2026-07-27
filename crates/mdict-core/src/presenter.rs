use std::sync::OnceLock;

use regex::Regex;

pub struct AggregateSection {
    pub dict_id: String,
    pub title: String,
    pub container_class: Option<String>,
    pub body: String,
}

pub fn render_aggregate_html(word: &str, sections: &[AggregateSection]) -> String {
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
        html.push_str(&sanitize_dict_html(&section.body));
        html.push_str("</div></section>");
    }

    html.push_str("</div>");
    html
}

/// Strip dangerous HTML constructs from dictionary content to prevent XSS.
///
/// Removes:
/// - `<script>...</script>` blocks (including attributes)
/// - Inline event-handler attributes (`onclick`, `onerror`, `onload`, etc.)
/// - `javascript:` URLs in src/href attributes
fn sanitize_dict_html(html: &str) -> String {
    static SCRIPT_RE: OnceLock<Regex> = OnceLock::new();
    static EVENT_RE: OnceLock<Regex> = OnceLock::new();
    static JS_URL_RE: OnceLock<Regex> = OnceLock::new();

    let script_re = SCRIPT_RE
        .get_or_init(|| Regex::new(r"(?is)<script[^>]*>.*?</script>").expect("valid script regex"));
    let event_re = EVENT_RE.get_or_init(|| {
        Regex::new(r#"(?i)\s+on\w+\s*=\s*["'][^"']*["']"#).expect("valid event regex")
    });
    let js_url_re = JS_URL_RE.get_or_init(|| {
        Regex::new(r#"(?i)(src|href)\s*=\s*["']\s*javascript:[^"']*["']"#)
            .expect("valid js url regex")
    });

    let result = script_re.replace_all(html, "");
    let result = event_re.replace_all(&result, "");
    let result = js_url_re.replace_all(&result, "");
    result.into_owned()
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn escape_html_attr(s: &str) -> String {
    escape_html(s)
}
