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
        let body = match std::str::from_utf8(&section.body) {
            Ok(s) => sanitize_dict_html(s),
            Err(_) => sanitize_dict_html(&String::from_utf8_lossy(&section.body)),
        };
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
    use super::sanitize_dict_html;

    fn stripped(input: &str) -> String {
        sanitize_dict_html(input)
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
}
