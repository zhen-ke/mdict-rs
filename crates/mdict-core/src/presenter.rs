use std::collections::HashSet;
use std::sync::OnceLock;

use bytes::Bytes;
use regex::Regex;

pub struct AggregateSection {
    pub dict_id: String,
    pub title: String,
    pub container_class: Option<String>,
    /// 原始词条 HTML（来自 mmap/缓存的 `Bytes`，Arc 共享、零拷贝）。
    /// 渲染时才一次性 sanitize；避免在 `query_aggregate_entries` 里先 `to_owned`
    /// 成 `String` 再 sanitize，又拷贝一道。（查词典越多收益越明显。）
    pub body: Bytes,
}

pub fn render_aggregate_html(word: &str, sections: &[AggregateSection]) -> String {
    // Per-(dict_id, style-fingerprint) dedup state, shared across all sections
    // so the same dict's repeated `<style>`/`<link rel=stylesheet>` is emitted
    // only once even when multiple entries / sections carry identical CSS.
    // Keyed by dict_id (not global) because cross-dict CSS lives in different
    // scopes and legitimately repeats once per dict.
    let mut seen_styles: HashSet<(String, String)> = HashSet::new();
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
        // 从 `Bytes` 零拷贝取 `&str`，再 sanitize（命中 UTF-8 路径只产生一次
        // 分配＝sanitize 的输出）；非 UTF-8 的退化场景退回 lossy + sanitize。
        // 随后按 (dict_id, 指纹) 去重内联 `<style>` / `<link rel=stylesheet>`，
        // 剠掉同本词典内重复一字不差的样式块——省响应体字节，且不影响级联
        // （重复块内容完全一致，剥掉后到不会改变层叠结果）。
        let body = match std::str::from_utf8(&section.body) {
            Ok(s) => sanitize_dict_html(s),
            Err(_) => sanitize_dict_html(&String::from_utf8_lossy(&section.body)),
        };
        let body = dedup_styles(&body, &section.dict_id, &mut seen_styles);
        html.push_str(&body);
        html.push_str("</div></section>");
    }

    html.push_str("</div>");
    html
}

/// Strip dangerous HTML constructs from dictionary content to prevent XSS.
///
/// Removes（在**一次**正则扫描里完成，而不是原来的三次 `replace_all` +
/// 三次全量字符串分配）：
/// - `<script>...</script>` blocks (including attributes)
/// - Inline event-handler attributes (`onclick`, `onerror`, `onload`, etc.)
/// - `javascript:` URLs in src/href attributes
///
/// 三类模式互不覆盖：script 块被首条 `(?is)<script[^>]*>.*?</script>` 整包吞掉
/// （块内的事件属性随之消失），拼接该条会一次性走到底；其余位置的事件属性/
/// js:-URL 由后两条 match。合并成单条 alternation 后 `replace_all("", ...)` 只产生
/// 一次 `Cow::Owned` 分配，语义与原三次串行一致。
fn sanitize_dict_html(html: &str) -> String {
    static SANITIZE_RE: OnceLock<Regex> = OnceLock::new();
    let re = SANITIZE_RE.get_or_init(|| {
        // `x`=verbose' 让模式里的字面空白/换行被忽略，便于可读地拆行书写；
        // `i`=不区分大小写、's'=DOTALL' 让 `.` 跨行匹配 script 块体。
        Regex::new(
            r#"(?isx)
              <script[^>]*>.*?</script>
            | \s+on\w+\s*=\s*["'][^"']*["']
            | (src|href)\s*=\s*["']\s*javascript:[^"']*["']
            "#,
        )
        .expect("valid sanitize regex")
    });
    re.replace_all(html, "").into_owned()
}

/// Per-(dict_id, fingerprint) dedup of inline `<style>` blocks and `<link
/// rel="stylesheet">` references in a sanitized entry body.
///
/// MDX dictionaries commonly embed the same `<style>` block / `<link>` in
/// *every* entry; when several entries (or several sections of the same dict)
/// are aggregated into one response, those byte-identical tags balloon the
/// payload for no styling effect (the browser de-duplicates `<link>` by URL
/// anyway, but inline `<style>` is real wasted bytes, and each copy was already
/// separately scoped by [`scope_css`] when served from `/res/`). We keep only
/// the first occurrence per `(dict_id, fingerprint)`.
///
/// Fingerprints:
/// - `<style>`: the trimmed block body (attrs ignored — a `<style media="all">`
///   and a bare `<style>` with the same body are treated as duplicates only if
///   the whole tag matches; we key on the verbatim block text to be safe).
/// - `<link rel="stylesheet">`: the `href` value (the URL uniquely identifies
///   the stylesheet; different `media=` on the same URL would still resolve to
///   the same scoped CSS, so we dedup on href alone).
///
/// Non-stylesheet `<link>` (icon / preload / prefetch / canonical / …) is left
/// untouched. The fingerprint set is shared across all sections of the aggregate
/// render so cross-section duplicates of the same dict are also collapsed.
fn dedup_styles(html: &str, dict_id: &str, seen: &mut HashSet<(String, String)>) -> String {
    static STYLE_BLOCK_RE: OnceLock<Regex> = OnceLock::new();
    static LINK_RE: OnceLock<Regex> = OnceLock::new();
    let style_re = STYLE_BLOCK_RE.get_or_init(|| {
        // `(?is)` — case-insensitive + DOTALL so `.` spans newlines in the body.
        Regex::new(r"(?is)<style\b[^>]*>.*?</style>").expect("valid style block regex")
    });
    let link_re = LINK_RE.get_or_init(|| {
        // Capture the full `<link ...>` tag attributes for manual inspection.
        Regex::new(r"(?is)<link\b([^>]*)>").expect("valid link regex")
    });

    // Pass 1: dedup inline <style> blocks by verbatim content.
    let after_style = style_re.replace_all(html, |caps: &regex::Captures| {
        let block = &caps[0];
        let fp = block.trim().to_string();
        if seen.insert((dict_id.to_string(), format!("style:{fp}"))) {
            block.to_string() // first sighting — keep
        } else {
            String::new() // duplicate — drop
        }
    });

    // Pass 2: dedup <link rel="stylesheet" href="..."> by href. Non-stylesheet
    // links are always kept (their fingerprint is never recorded).
    let after_style = after_style.into_owned();
    let after_link = link_re.replace_all(&after_style, |caps: &regex::Captures| {
        let tag = &caps[0];
        let attrs = caps[1].as_bytes();
        let Some(href) = extract_attr(attrs, b"href") else {
            return tag.to_string(); // no href — not a stylesheet candidate
        };
        if !has_attr(attrs, b"rel") {
            return tag.to_string();
        }
        let rel = extract_attr(attrs, b"rel").unwrap_or_default();
        if !rel
            .split_whitespace()
            .any(|tok| tok.eq_ignore_ascii_case("stylesheet"))
        {
            return tag.to_string();
        }
        if seen.insert((dict_id.to_string(), format!("link:{href}"))) {
            tag.to_string()
        } else {
            String::new()
        }
    });

    after_link.into_owned()
}

/// Extract a double- or single-quoted attribute value from `attrs` (the raw
/// bytes inside a tag, after the tag name). Returns the trimmed value without
/// quotes, or `None` if the attribute is absent / has an unquoted value (we
/// don't bother parsing unquoted href values — they're rare and the dedup
/// simply falls through to "keep" in that case).
fn extract_attr(attrs: &[u8], name: &[u8]) -> Option<String> {
    let mut i = 0;
    while i < attrs.len() {
        // Skip whitespace.
        while i < attrs.len() && attrs[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= attrs.len() {
            break;
        }
        // Read the attribute name.
        let name_start = i;
        while i < attrs.len()
            && !attrs[i].is_ascii_whitespace()
            && attrs[i] != b'='
            && attrs[i] != b'/'
            && attrs[i] != b'>'
        {
            i += 1;
        }
        let attr_name = &attrs[name_start..i];
        // Skip whitespace before `=`.
        while i < attrs.len() && attrs[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= attrs.len() || attrs[i] != b'=' {
            // Boolean attribute — no value. Not the one we want (unless name
            // matches and caller asked for a boolean, which we never do here).
            continue;
        }
        i += 1; // consume `=`
        while i < attrs.len() && attrs[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= attrs.len() {
            break;
        }
        let quote = attrs[i];
        if quote == b'"' || quote == b'\'' {
            i += 1;
            let val_start = i;
            while i < attrs.len() && attrs[i] != quote {
                i += 1;
            }
            let val = std::str::from_utf8(&attrs[val_start..i]).ok()?;
            i = (i + 1).min(attrs.len()); // skip closing quote
            if attr_name.eq_ignore_ascii_case(name) {
                return Some(val.to_string());
            }
        } else {
            // Unquoted value — read until whitespace or end.
            let val_start = i;
            while i < attrs.len()
                && !attrs[i].is_ascii_whitespace()
                && attrs[i] != b'/'
                && attrs[i] != b'>'
            {
                i += 1;
            }
            let val = std::str::from_utf8(&attrs[val_start..i]).ok()?;
            if attr_name.eq_ignore_ascii_case(name) {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// True iff `attrs` carries an attribute named `name` (value-agnostic). Used to
/// short-circuit `<link>` tags that have an `href` but no `rel` — those aren't
/// stylesheets and must be preserved (e.g. `<link href="..." itemprop="...">`).
fn has_attr(attrs: &[u8], name: &[u8]) -> bool {
    let mut i = 0;
    while i < attrs.len() {
        while i < attrs.len() && attrs[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= attrs.len() {
            break;
        }
        let name_start = i;
        while i < attrs.len()
            && !attrs[i].is_ascii_whitespace()
            && attrs[i] != b'='
            && attrs[i] != b'/'
            && attrs[i] != b'>'
        {
            i += 1;
        }
        let attr_name = &attrs[name_start..i];
        if attr_name.eq_ignore_ascii_case(name) {
            return true;
        }
        // Skip to the next attribute: swallow `=value` if present.
        while i < attrs.len() && attrs[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < attrs.len() && attrs[i] == b'=' {
            i += 1;
            while i < attrs.len() && attrs[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < attrs.len() && (attrs[i] == b'"' || attrs[i] == b'\'') {
                let q = attrs[i];
                i += 1;
                while i < attrs.len() && attrs[i] != q {
                    i += 1;
                }
                i = (i + 1).min(attrs.len());
            } else {
                while i < attrs.len()
                    && !attrs[i].is_ascii_whitespace()
                    && attrs[i] != b'/'
                    && attrs[i] != b'>'
                {
                    i += 1;
                }
            }
        }
    }
    false
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{dedup_styles, render_aggregate_html, sanitize_dict_html, AggregateSection};

    fn stripped(input: &str) -> String {
        sanitize_dict_html(input)
    }

    fn dedup_fresh() -> HashSet<(String, String)> {
        HashSet::new()
    }

    #[test]
    fn sanitize_removes_script_blocks_any_case_across_newlines() {
        let html = "before<SCRIPT type=\"text/javascript\">alert(1)\n  x = 2</script>after";
        assert_eq!(stripped(html), "beforeafter");
        // 必頗整包吞掉：script 体内的事件属性也一并消失。
        let html2 = "a<script onclick=\"evil()\">body</script>b";
        assert_eq!(stripped(html2), "ab");
        // 自闭合样并需正常闭合标签才被整块移除。
        let html3 = "keep<script>no close here";
        assert_eq!(stripped(html3), "keep<script>no close here");
    }

    #[test]
    fn sanitize_strips_inline_event_handler_attrs() {
        let html = r#"<img src="x" onclick="evil()" onerror="boom()">"#;
        let out = stripped(html);
        assert!(!out.contains("onclick"));
        assert!(!out.contains("onerror"));
        // 原行不为空的属性保留。
        assert!(out.contains("src=\"x\""));
    }

    #[test]
    fn sanitize_neutralizes_javascript_urls_in_src_href() {
        let html = r#"<a href="javascript:evil()">x</a><img src="javascript:evil()">"#;
        let out = stripped(html);
        assert!(!out.contains("javascript:"));
        // 普通的 href/src 不受影响。
        let html2 = r#"<a href="/page">ok</a><img src="/i.png">"#;
        let out2 = stripped(html2);
        assert!(out2.contains("/page"));
        assert!(out2.contains("/i.png"));
    }

    #[test]
    fn sanitize_preserves_safe_content() {
        let html = "<div><p>Hello &amp; <b>world</b></p></div>";
        assert_eq!(stripped(html), html);
        assert_eq!(stripped(""), "");
        assert_eq!(stripped("plain text no tags"), "plain text no tags");
    }

    #[test]
    fn sanitize_single_pass_does_not_double_consume_across_alternatives() {
        // script 块中同时含 js:URL + 事件属性；三类在一次扫描里以 script 块
        // 尭先 match 消化掉，后续不应再留下任何危险构造。
        let html = "x<script href=\"javascript:nope\" onload=\"nope()\">y</script>z";
        assert_eq!(stripped(html), "xz");
    }

    // ---------- dedup_styles ----------

    #[test]
    fn dedup_drops_duplicate_inline_style_blocks_same_dict() {
        let block = "<style>.x{color:red}</style>";
        let html = format!("{block}body{block}tail");
        let mut seen = dedup_fresh();
        let out = dedup_styles(&html, "d1", &mut seen);
        assert_eq!(out, format!("{block}bodytail"));
        // The first block was recorded for d1.
        assert!(seen.contains(&(
            "d1".to_string(),
            "style:<style>.x{color:red}</style>".to_string()
        )));
    }

    #[test]
    fn dedup_keeps_different_style_blocks_same_dict() {
        let html = "<style>.a{color:red}</style><style>.b{color:blue}</style>";
        let mut seen = dedup_fresh();
        let out = dedup_styles(html, "d1", &mut seen);
        assert_eq!(out, html);
    }

    #[test]
    fn dedup_drops_duplicate_stylesheet_links_same_dict() {
        let link = r#"<link rel="stylesheet" href="/dict/d1/res/lm6.css">"#;
        let html = format!("{link}body{link}tail");
        let mut seen = dedup_fresh();
        let out = dedup_styles(&html, "d1", &mut seen);
        assert_eq!(out, format!("{link}bodytail"));
        assert!(seen.contains(&("d1".to_string(), "link:/dict/d1/res/lm6.css".to_string())));
    }

    #[test]
    fn dedup_keeps_same_link_for_different_dicts() {
        // Cross-dict: the CSS is scoped per dict, so the same href under two
        // different dict_ids is NOT a duplicate (each dict keeps its own first
        // sighting). We verify by running dedup on two *independent* two-link
        // inputs — one collapsed copy survives per dict.
        let link = r#"<link rel="stylesheet" href="/dict/d1/res/lm6.css">"#;
        let html = format!("{link}{link}");
        let mut seen = dedup_fresh();
        let out1 = dedup_styles(&html, "d1", &mut seen);
        let out2 = dedup_styles(&html, "d2", &mut seen);
        assert_eq!(out1, link);
        assert_eq!(out2, link);
    }

    #[test]
    fn dedup_preserves_non_stylesheet_links() {
        // icon / preload / canonical must NOT be stripped even if repeated.
        let icon = r#"<link rel="icon" href="/favicon.ico">"#;
        let html = format!("{icon}{icon}");
        let mut seen = dedup_fresh();
        let out = dedup_styles(&html, "d1", &mut seen);
        assert_eq!(out, html);
    }

    #[test]
    fn dedup_preserves_link_with_href_but_no_rel() {
        let html = r#"<link href="/some/thing">"#;
        let mut seen = dedup_fresh();
        let out = dedup_styles(html, "d1", &mut seen);
        assert_eq!(out, html);
    }

    #[test]
    fn dedup_handles_single_quoted_attrs_and_whitespace() {
        let link = r#"<link  rel = 'stylesheet'  href = '/dict/d1/res/x.css' >"#;
        let html = format!("{link}{link}");
        let mut seen = dedup_fresh();
        let out = dedup_styles(&html, "d1", &mut seen);
        assert_eq!(out, link);
    }

    #[test]
    fn dedup_treats_rel_alternate_stylesheet_as_stylesheet() {
        // `rel="alternate stylesheet"` is a valid stylesheet variant (主题切换) —
        // dedup on href should still collapse duplicates.
        let link = r#"<link rel="alternate stylesheet" href="/dict/d1/res/theme.css">"#;
        let html = format!("{link}{link}");
        let mut seen = dedup_fresh();
        let out = dedup_styles(&html, "d1", &mut seen);
        assert_eq!(out, link);
    }

    // ---------- render_aggregate_html dedup integration ----------

    fn section(id: &str, body: &str) -> AggregateSection {
        AggregateSection {
            dict_id: id.to_string(),
            title: id.to_string(),
            container_class: None,
            body: bytes::Bytes::copy_from_slice(body.as_bytes()),
        }
    }

    #[test]
    fn render_dedupes_style_blocks_across_sections_of_same_dict() {
        let block = "<style>.x{color:red}</style>";
        let body_a = format!("{block}<p>A</p>");
        let body_b = format!("{block}<p>B</p>");
        let html = render_aggregate_html("w", &[section("d1", &body_a), section("d1", &body_b)]);
        // The second section's identical <style> block must be stripped.
        let style_count = html.matches("<style>.x{color:red}</style>").count();
        assert_eq!(style_count, 1);
        // Both entry bodies are preserved.
        assert!(html.contains("<p>A</p>"));
        assert!(html.contains("<p>B</p>"));
    }

    #[test]
    fn render_keeps_per_dict_style_links_in_cross_dict_aggregate() {
        let link_a = r#"<link rel="stylesheet" href="/dict/d1/res/lm6.css">"#;
        let link_b = r#"<link rel="stylesheet" href="/dict/d2/res/lm6.css">"#;
        let html = render_aggregate_html(
            "w",
            &[
                section("d1", &format!("{link_a}<p>A</p>")),
                section("d2", &format!("{link_b}<p>B</p>")),
            ],
        );
        assert_eq!(html.matches(link_a).count(), 1);
        assert_eq!(html.matches(link_b).count(), 1);
    }
}
