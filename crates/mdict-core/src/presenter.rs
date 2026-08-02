use std::collections::HashSet;
use std::sync::OnceLock;

use bytes::Bytes;
use regex::Regex;

pub struct AggregateSection {
    pub dict_id: String,
    pub title: String,
    pub container_class: Option<String>,
    /// 词典配置文件（`<dict>.toml`）里声明的自定义 CSS（注入 iframe `<head>`，
    /// 位于词典自带样式之后、全局微调层之前）。
    pub extra_css: Option<String>,
    /// 词典配置文件里声明的自定义 JS（注入 iframe 文档 body 末尾，随词典
    /// 脚本一起在沙箱内执行）。
    pub extra_js: Option<String>,
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
        // 每本词典一个 iframe 沙箱：条目（含自带 <script>/<style>）在独立文档
        // 内渲染执行，浏览器原生隔离，杜绝跨词典样式/脚本污染。词典自带的
        // `<style>`/`<link>` 放进 iframe 的 `<head>`（已由 rewrite_html 把
        // url() 重写到本词典 /dict/{id}/res/...）；`.mdict-dict-body` 保留为
        // 内容容器，也是词典 CSS 作用域锚点。
        html.push_str(r#"<div class="mdict-dict-frame">"#);
        html.push_str(&render_dict_iframe_srcdoc(section, &mut seen_styles));
        html.push_str("</div></section>");
    }

    html.push_str("</div>");
    html
}

/// 组装一本词典的 iframe `srcdoc`（完整的独立 HTML 文档）。
///
/// 结构：
/// ```html
/// <html><head>
///   <style>…词典自带样式（去重后）…</style>
///   <link rel="stylesheet" …>…（去重后）…
///   <link rel="stylesheet" href="/article-style.css">（全局微调层，末尾、权重最高）
/// </head><body>
///   <div class="mdict-dict-body">…词典条目（脚本保留执行）…</div>
/// </body></html>
/// ```
///
/// `article-style.css` 由服务端静态目录提供，内部选择器已被 `scope_css`
/// 作用域到 `.mdict-dict-body`，因此只在词典条目容器内生效、不污染 iframe
/// 外的主页面。
fn render_dict_iframe_srcdoc(
    section: &AggregateSection,
    seen_styles: &mut HashSet<(String, String)>,
) -> String {
    // 先从原始条目里把 `<style>`/`<link rel=stylesheet>` 提取出来（去重后放进
    // <head>），条目正文里再去除这些标签，避免重复加载。
    let raw = match std::str::from_utf8(&section.body) {
        Ok(s) => s.to_string(),
        Err(_) => String::from_utf8_lossy(&section.body).into_owned(),
    };
    // sanitize 保留 <script>，只净除危险事件属性/URL。
    let sanitized = sanitize_dict_html(&raw);

    // 提取 <style> 块与 <link rel="stylesheet">（词典自带样式）
    let mut head_css = String::new();
    let mut body = sanitized.clone();
    extract_dict_styles(&sanitized, section.dict_id.as_str(), &mut body, &mut head_css, seen_styles);

    let mut doc = String::with_capacity(raw.len() + 1024);
    doc.push_str("<!DOCTYPE html><html><head><meta charset=\"utf-8\">");
    doc.push_str(
        "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">",
    );
    // 词典自带样式（去重后）放进 <head>
    doc.push_str(&head_css);
    // 词典配置文件里声明的自定义 CSS（位于自带样式之后，可覆盖自带样式）
    if let Some(css) = &section.extra_css {
        if !css.trim().is_empty() {
            doc.push_str("<style>");
            doc.push_str(css);
            doc.push_str("</style>");
        }
    }
    // 全局微调层：末尾加载、权重最高，统一各词典视觉
    doc.push_str(
        r#"<link rel="stylesheet" type="text/css" href="/article-style.css">"#,
    );
    doc.push_str("</head>");
    // 作用域锚点：词典自带 CSS 被服务端作用域化为 `[data-dict-id="X"]
    // .mdict-dict-body`，iframe 内部必须复现这个祖先/后代结构才能命中
    // （否则词典自带样式在 iframe 内全部失效）。
    doc.push_str(&format!(
        r#"<body><div class="mdict-dict-scope" data-dict-id="{}"><div class="mdict-dict-body">"#,
        escape_html_attr(&section.dict_id)
    ));
    doc.push_str(&body);
    doc.push_str("</div></div>");
    // 词典配置文件里声明的自定义 JS（随词典条目一起在沙箱内执行）
    if let Some(js) = &section.extra_js {
        if !js.trim().is_empty() {
            doc.push_str("<script>");
            doc.push_str(js);
            doc.push_str("</script>");
        }
    }
    // 高度自适应桥 + 内部链接转发：iframe 为 opaque-origin 沙箱，父页无法
    // 读取内部 DOM 或捕获内部点击；由内部脚本上报高度，并把内部路由链接
    // （sound://、entry://、/dict/{id}/...）点击转发给父页处理。
    let bridge_js = format!(
        r#"<script>
(function () {{
  function report() {{
    var h = document.documentElement.scrollHeight;
    try {{ parent.postMessage({{ mdictFrame: true, dictId: "{}", height: h }}, "*"); }} catch (e) {{}}
  }}
  if (window.ResizeObserver) {{
    new ResizeObserver(report).observe(document.documentElement);
  }}
  window.addEventListener('load', report);
  setTimeout(report, 50);
  setTimeout(report, 300);
  document.addEventListener('click', function (e) {{
    var a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
    if (!a) return;
    var href = a.getAttribute('href') || '';
    if (href.indexOf('sound://') === 0 || href.indexOf('entry://') === 0 ||
        href.indexOf('/dict/') === 0 || href.indexOf('/resource/') === 0) {{
      e.preventDefault();
      try {{ parent.postMessage({{ mdictNav: true, dictId: "{}", href: href }}, "*"); }} catch (err) {{}}
    }}
  }});
}})();
</script>"#,
        escape_js_string(&section.dict_id),
        escape_js_string(&section.dict_id)
    );
    doc.push_str(&bridge_js);
    doc.push_str("</body></html>");

    // srcdoc 属性转义：`&`→`&amp;`、`"`→`&quot;`、`<`→`&lt;` 等。iframe srcdoc
    // 的实体在解析时被还原成原文，再作为 HTML 文档解析，因此这是安全且正确的。
    //
    // sandbox 刻意**不加** `allow-same-origin`：srcdoc iframe 会继承父页面 origin，
    // 若同时开 allow-scripts + allow-same-origin，沙箱脚本即可访问父页面 DOM
    // （window.parent），沙箱形同虚设。保持 opaque origin 时 `window.parent`
    // 访问被浏览器跨源策略阻断，词典脚本只能在 iframe 内自娱自乐；相对/绝对
    // 路径资源（/dict/{id}/res/...、/article-style.css）不依赖 origin，照常加载。
    //
    // `allow-scripts` 允许 iframe 内脚本（词典自带脚本 + 上面高度桥）运行；
    // postMessage 是跨源安全的通信通道，父页据此自适应 iframe 高度。
    let escaped = escape_html(&doc);
    format!(
        r#"<iframe class="mdict-dict-iframe" data-dict-id="{}" srcdoc="{}" loading="lazy" sandbox="allow-scripts allow-popups allow-forms"></iframe>"#,
        escape_html_attr(&section.dict_id),
        escaped
    )
}

/// 从 sanitize 后的条目里提取 `<style>` 块与 `<link rel="stylesheet">`（词典
/// 自带样式），放入 `head_css`；从 `body` 中移除这些样式标签，避免正文重复
/// 加载。先经 [`dedup_styles`] 去重（同一词典重复样式只保留一次），再提取。
fn extract_dict_styles(
    html: &str,
    dict_id: &str,
    body: &mut String,
    head_css: &mut String,
    seen: &mut HashSet<(String, String)>,
) {
    // 先去重（复用 dedup_styles 的语义与指纹规则），再去掉残留的样式标签。
    *body = dedup_styles(html, dict_id, seen);

    static STYLE_BLOCK_RE: OnceLock<Regex> = OnceLock::new();
    static LINK_RE: OnceLock<Regex> = OnceLock::new();
    let style_re = STYLE_BLOCK_RE.get_or_init(|| {
        Regex::new(r"(?is)(<style\b[^>]*>)(.*?)(</style>)").expect("valid style block regex")
    });
    let link_re = LINK_RE.get_or_init(|| {
        Regex::new(r"(?is)<link\b([^>]*)>").expect("valid link regex")
    });

    // 提取 <style> 块（首见，已去重）到 <head>，从正文移除。
    let mut extracted: Vec<String> = Vec::new();
    *body = style_re
        .replace_all(body, |caps: &regex::Captures| {
            let full = &caps[0];
            extracted.push(full.to_string());
            String::new()
        })
        .into_owned();

    // 提取 <link rel="stylesheet"> 到 <head>，从正文移除。非样式表 link 保留。
    let mut extracted_links: Vec<String> = Vec::new();
    *body = link_re
        .replace_all(body, |caps: &regex::Captures| {
            let tag = &caps[0];
            let attrs = caps[1].as_bytes();
            let Some(_href) = extract_attr(attrs, b"href") else {
                return tag.to_string();
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
            extracted_links.push(tag.to_string());
            String::new()
        })
        .into_owned();

    for tag in extracted {
        head_css.push_str(&tag);
    }
    for tag in extracted_links {
        head_css.push_str(&tag);
    }
}

/// Strip dangerous HTML constructs from dictionary content to prevent XSS.
///
/// 词典自带的 `<script>` **保留**（不再删除）——脚本会随词典条目一起被放进
/// 独立 iframe 沙箱执行，浏览器原生隔离，因此无需在服务端剥掉。此处只净除
/// 仍然危险的构造（一次正则扫描完成）：
/// - Inline event-handler attributes (`onclick`, `onerror`, `onload`, etc.)
/// - `javascript:` URLs in src/href attributes
///
/// 剩余风险（如 `<iframe>` 引用危险 URL、`data:text/html`）由 iframe 沙箱的
/// 浏览器级隔离兜底，不在此处做字符串级防御。
fn sanitize_dict_html(html: &str) -> String {
    static SANITIZE_RE: OnceLock<Regex> = OnceLock::new();
    let re = SANITIZE_RE.get_or_init(|| {
        // `x`=verbose 让模式里的字面空白/换行被忽略，便于可读地拆行书写；
        // `i`=不区分大小写。
        Regex::new(
            r#"(?ix)
              \s+on\w+\s*=\s*["'][^"']*["']
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

/// 转义一段文本以安全嵌入 JS 字符串字面量（双引号包裹）。dict_id 等外部
/// 输入嵌入桥接脚本时必须经过此函数，避免闭合字符串注入额外 JS。
fn escape_js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
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

    use super::{AggregateSection, dedup_styles, render_aggregate_html, sanitize_dict_html};

    fn stripped(input: &str) -> String {
        sanitize_dict_html(input)
    }

    fn dedup_fresh() -> HashSet<(String, String)> {
        HashSet::new()
    }

    #[test]
    fn sanitize_preserves_script_blocks() {
        // 词典自带脚本应原样保留——由 iframe 沙箱隔离执行，服务端不再删除。
        let html = "before<SCRIPT type=\"text/javascript\">alert(1)\n  x = 2</script>after";
        assert_eq!(stripped(html), html);
        // 事件属性/危险 URL 仍然被净除。
        let html2 = "a<script onclick=\"evil()\">body</script>b";
        let out2 = stripped(html2);
        assert!(out2.contains("<script"));
        assert!(out2.contains(">body</script>"));
        assert!(!out2.contains("onclick"));
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
        // script 块保留；块内的事件属性 + js:URL 仍被净除。
        let html = "x<script href=\"javascript:nope\" onload=\"nope()\">y</script>z";
        let out = stripped(html);
        assert!(out.contains("<script"));
        assert!(out.contains(">y</script>"));
        assert!(!out.contains("onload"));
        assert!(!out.contains("javascript:nope"));
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
            extra_css: None,
            extra_js: None,
            body: bytes::Bytes::copy_from_slice(body.as_bytes()),
        }
    }

    #[test]
    fn render_wraps_each_dict_in_own_iframe_sandbox() {
        let html = render_aggregate_html("w", &[section("d1", "<p>A</p>"), section("d2", "<p>B</p>")]);
        // 每本词典一个 iframe（data-dict-id 区分），结构上互相独立。
        let frame_count = html.matches("mdict-dict-iframe").count();
        assert_eq!(frame_count, 2);
        assert!(html.contains(r#"<section class="mdict-dict-section" data-dict-id="d1">"#));
        assert!(html.contains(r#"<section class="mdict-dict-section" data-dict-id="d2">"#));
        // 条目正文在 srcdoc 里（被实体转义，但解析后会还原）。
        assert!(html.contains("&lt;p&gt;A&lt;/p&gt;"));
        assert!(html.contains("&lt;p&gt;B&lt;/p&gt;"));
    }

    #[test]
    fn render_moves_dict_styles_into_iframe_head_and_dedupes() {
        let block = "<style>.x{color:red}</style>";
        let html = render_aggregate_html("w", &[section("d1", &format!("{block}<p>A</p>{block}<p>B</p>"))]);
        // 只有一个 iframe。
        assert_eq!(html.matches("mdict-dict-iframe").count(), 1);
        // srcdoc 里的 <style> 被实体转义；同一词典重复样式只保留一次。
        let style_in_srcdoc = html.matches("&lt;style&gt;.x{color:red}&lt;/style&gt;").count();
        assert_eq!(style_in_srcdoc, 1);
        // 正文里不再残留 <style>（已移入 <head>），条目内容保留。
        assert!(html.contains("&lt;p&gt;A&lt;/p&gt;"));
        assert!(html.contains("&lt;p&gt;B&lt;/p&gt;"));
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
        // 两本词典各一个 iframe，各自的样式表链接只在各自 srcdoc 的 <head> 出现一次。
        assert_eq!(html.matches("mdict-dict-iframe").count(), 2);
        assert_eq!(
            html.matches("&lt;link rel=&quot;stylesheet&quot; href=&quot;/dict/d1/res/lm6.css&quot;&gt;")
                .count(),
            1
        );
        assert_eq!(
            html.matches("&lt;link rel=&quot;stylesheet&quot; href=&quot;/dict/d2/res/lm6.css&quot;&gt;")
                .count(),
            1
        );
    }

    #[test]
    fn render_keeps_dict_scripts_in_srcdoc() {
        // 词典自带脚本必须保留（随 iframe 沙箱执行），服务端不删除。
        let html = render_aggregate_html("w", &[section("d1", "<script>alert(1)</script><p>A</p>")]);
        assert_eq!(html.matches("mdict-dict-iframe").count(), 1);
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        // 事件属性/危险 URL 仍被净除。
        let html2 = render_aggregate_html("w", &[section("d1", r#"<img src="x" onclick="evil()"><p>A</p>"#)]);
        assert!(!html2.contains("onclick"));
    }
}
