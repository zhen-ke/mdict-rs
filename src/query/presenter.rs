pub(crate) struct AggregateSection {
    pub dict_id: String,
    pub title: String,
    pub container_class: Option<String>,
    pub body: String,
}

pub(crate) fn render_aggregate_html(word: &str, sections: &[AggregateSection]) -> String {
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
