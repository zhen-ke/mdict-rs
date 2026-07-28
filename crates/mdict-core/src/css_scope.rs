//! Per-dictionary CSS scoping.
//!
//! Dictionary bundles ship their own CSS (`lm6.css`, OALD stylesheets, …) which
//! uses bare selectors like `.entry`, `a:hover`, `*`. Served verbatim those leak
//! across dictionary sections *and* into the app shell. To isolate them while
//! keeping content in the light DOM (so the vendored `lm6.js` post-processing —
//! which queries the global `document` — keeps working), we prefix every
//! top-level style-rule selector with a scope selector `[data-dict-id="X"]
//! .mdict-dict-body`.
//!
//! At-rules are handled structurally:
//! - `@media` / `@supports` / `@container`: recurse into the block and scope the
//!   inner style rules (the condition prelude is preserved verbatim).
//! - `@font-face` / `@keyframes` / `@-webkit-keyframes` / `@page` / `@namespace`
//!   / `@import` / `@charset`: passed through untouched — they are *global* by
//!   design (fonts, animation names) and must not be scoped.
//!
//! The parser is brace / paren / string / comment aware so it never splits
//! inside `:not(...)`, attribute selectors, or quoted values.

/// Scope every top-level style-rule selector in `css` under `scope` (e.g.
/// `[data-dict-id="abc"] .mdict-dict-body`), preserving at-rules.
pub fn scope_css(css: &str, scope: &str) -> String {
    let mut out = String::with_capacity(css.len() + scope.len() * 8);
    scope_into(css, scope, &mut out);
    out
}

fn scope_into(css: &str, scope: &str, out: &mut String) {
    let bytes = css.as_bytes();
    let mut i = 0;
    let n = bytes.len();
    while i < n {
        // Copy through any whitespace and comments until the next rule begins.
        let gap_end = i + skip_ws_comments(&css[i..]);
        if gap_end > i {
            out.push_str(&css[i..gap_end]);
            i = gap_end;
            if i >= n {
                break;
            }
        }
        if bytes[i] == b'@' {
            i = emit_at_rule(css, bytes, i, scope, out);
        } else {
            i = emit_style_rule(css, bytes, i, scope, out);
        }
    }
}

/// Return how many bytes of leading whitespace and CSS comments `rest` contains.
fn skip_ws_comments(rest: &str) -> usize {
    let bytes = rest.as_bytes();
    let mut i = 0;
    let n = bytes.len();
    while i < n {
        if rest[i..].starts_with("/*") {
            i += 2;
            while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        break;
    }
    i
}

/// Emit a single top-level at-rule starting at `i` (which points at `@`).
/// Returns the index just past the rule.
fn emit_at_rule(css: &str, bytes: &[u8], i: usize, scope: &str, out: &mut String) -> usize {
    // Read the at-rule keyword and prelude up to `{` or `;`.
    let start = i;
    let n = bytes.len();
    let mut depth: i32 = 0;
    let mut keyword_end = i + 1;
    while keyword_end < n
        && (bytes[keyword_end].is_ascii_alphanumeric() || bytes[keyword_end] == b'-')
    {
        keyword_end += 1;
    }
    let keyword = &css[start + 1..keyword_end];
    let kw_lower = keyword.to_ascii_lowercase();

    // Scan prelude until the terminating `{` (block at-rule) or `;` (statement).
    let mut j = keyword_end;
    while j < n {
        if css[j..].starts_with("/*") {
            j += 2;
            while j + 1 < n && !(bytes[j] == b'*' && bytes[j + 1] == b'/') {
                j += 1;
            }
            j += 2;
            continue;
        }
        match bytes[j] {
            b'"' | b'\'' => j += skip_string(css, j),
            b'(' | b'[' => {
                depth += 1;
                j += 1;
            }
            b')' | b']' => {
                depth -= 1;
                j += 1;
            }
            b';' if depth <= 0 => {
                // Statement at-rule (e.g. @import, @namespace, @charset) — emit as-is.
                out.push_str(&css[start..=j]);
                return j + 1;
            }
            b'{' if depth <= 0 => break,
            _ => j += 1,
        }
    }
    if j >= n {
        // Unterminated — emit the remainder verbatim.
        out.push_str(&css[start..]);
        return n;
    }
    let prelude_end = j; // points at `{`
    let prelude = &css[start..prelude_end];

    // Find the matching closing brace, accounting for nesting.
    let block_start = prelude_end;
    let mut k = block_start + 1;
    let mut brace: i32 = 1;
    while k < n && brace > 0 {
        if css[k..].starts_with("/*") {
            k += 2;
            while k + 1 < n && !(bytes[k] == b'*' && bytes[k + 1] == b'/') {
                k += 1;
            }
            k += 2;
            continue;
        }
        match bytes[k] {
            b'"' | b'\'' => k += skip_string(css, k),
            b'{' => {
                brace += 1;
                k += 1;
            }
            b'}' => {
                brace -= 1;
                k += 1;
            }
            _ => k += 1,
        }
    }
    let block_end = k; // just past the closing `}`

    let inner = &css[block_start + 1..block_end - 1.min(n)];

    let recurses = matches!(
        kw_lower.as_str(),
        "media" | "supports" | "container" | "layer"
    );
    if recurses {
        // Emit the prelude (e.g. `@media (min-width: 0)`), then scope the block's
        // inner rules, then the closing brace.
        out.push_str(prelude);
        out.push('{');
        scope_into(inner, scope, out);
        out.push('}');
    } else {
        // Global at-rules (@font-face, @keyframes, @page, …) — emit verbatim.
        out.push_str(&css[start..block_end]);
    }
    block_end
}

/// Emit a single top-level style rule starting at `i`. The selector list runs
/// from `start..open` (open = `{`), declarations `open..close`. We scope each
/// selector in the list. Returns the index just past the rule's `}`.
fn emit_style_rule(css: &str, bytes: &[u8], i: usize, scope: &str, out: &mut String) -> usize {
    let n = bytes.len();
    // Find the opening brace (already known to be at top level). Handles
    // comments / strings / parens so we don't mistake them for selectors.
    let mut j = i;
    let mut depth: i32 = 0;
    while j < n {
        if css[j..].starts_with("/*") {
            j += 2;
            while j + 1 < n && !(bytes[j] == b'*' && bytes[j + 1] == b'/') {
                j += 1;
            }
            j += 2;
            continue;
        }
        match bytes[j] {
            b'"' | b'\'' => j += skip_string(css, j),
            b'(' | b'[' => {
                depth += 1;
                j += 1;
            }
            b')' | b']' => {
                depth -= 1;
                j += 1;
            }
            b'{' if depth <= 0 => break,
            _ => j += 1,
        }
    }
    if j >= n {
        out.push_str(&css[i..]);
        return n;
    }
    let selector_list = &css[i..j];
    let scoped = scope_selector_list(selector_list, scope);
    out.push_str(&scoped);
    out.push(' ');

    // Copy the declaration block verbatim, including the braces, up to the
    // matching close (declarations may contain `{` only inside strings/san …
    // not in CSS, so a balanced brace walk is unnecessary; but be safe).
    let mut k = j;
    let mut brace: i32 = 0;
    while k < n {
        if css[k..].starts_with("/*") {
            out.push_str("/*");
            k += 2;
            while k + 1 < n && !(bytes[k] == b'*' && bytes[k + 1] == b'/') {
                out.push(bytes[k] as char);
                k += 1;
            }
            if k + 1 < n {
                out.push_str("*/");
                k += 2;
            }
            continue;
        }
        match bytes[k] {
            b'"' | b'\'' => {
                let s = skip_string(css, k);
                out.push_str(&css[k..k + s]);
                k += s;
            }
            b'{' => {
                brace += 1;
                out.push('{');
                k += 1;
            }
            b'}' => {
                brace -= 1;
                out.push('}');
                k += 1;
                if brace <= 0 {
                    break;
                }
            }
            _ => {
                out.push(bytes[k] as char);
                k += 1;
            }
        }
    }
    k
}

/// Split a comma-separated selector list (paren / string / comment aware) and
/// prefix each complex selector with `scope ` — except `:root` / `html` / `body`
/// which are rewritten to bare `scope` so global base rules still apply to the
/// scope element.
fn scope_selector_list(list: &str, scope: &str) -> String {
    let trimmed = list.trim();
    if trimmed.is_empty() {
        return list.to_string();
    }
    let mut out = String::with_capacity(list.len() + scope.len() * 4);
    for (idx, sel) in split_selectors(trimmed).iter().enumerate() {
        let sel = sel.trim();
        if idx > 0 {
            out.push_str(", ");
        }
        if sel.is_empty() {
            continue;
        }
        match sel {
            ":root" | "html" | "body" => out.push_str(scope),
            _ => {
                out.push_str(scope);
                out.push(' ');
                out.push_str(sel);
            }
        }
    }
    out
}

/// Split a selector list on top-level commas (ignoring commas inside parens,
/// attribute selectors, strings, and comments).
fn split_selectors(list: &str) -> Vec<&str> {
    let bytes = list.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut i = 0;
    let mut depth: i32 = 0;
    while i < bytes.len() {
        if list[i..].starts_with("/*") {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        match bytes[i] {
            b'"' | b'\'' => i += skip_string(list, i),
            b'(' | b'[' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' => {
                depth -= 1;
                i += 1;
            }
            b',' if depth <= 0 => {
                parts.push(&list[start..i]);
                start = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    parts.push(&list[start..]);
    parts
}

/// Length (in bytes) of a string literal starting at `i` in `s`, including the
/// opening and closing quotes and any backslash escapes.
fn skip_string(s: &str, i: usize) -> usize {
    let bytes = s.as_bytes();
    let quote = bytes[i];
    let mut j = i + 1;
    while j < bytes.len() {
        if bytes[j] == b'\\' && j + 1 < bytes.len() {
            j += 2;
            continue;
        }
        if bytes[j] == quote {
            return j + 1 - i;
        }
        j += 1;
    }
    bytes.len() - i
}

#[cfg(test)]
mod tests {
    use super::scope_css;

    const SCOPE: &str = r#"[data-dict-id="d1"] .mdict-dict-body"#;

    #[test]
    fn scopes_simple_selectors() {
        let css = ".entry { color: red }\na:hover { text-decoration: underline }";
        let out = scope_css(css, SCOPE);
        assert!(
            out.contains(r#"[data-dict-id="d1"] .mdict-dict-body .entry { color: red }"#),
            "{out}"
        );
        assert!(out.contains(&format!("{SCOPE} a:hover")));
    }

    #[test]
    fn maps_root_html_body_to_scope() {
        let css = ":root { --x: 1 } body { margin: 0 } html { font: 16px }";
        let out = scope_css(css, SCOPE);
        let root_rule = format!("{} {{ --x: 1 }}", SCOPE);
        assert!(out.contains(&root_rule), "{out}");
        assert!(out.contains(&format!("{} {{ margin: 0 }}", SCOPE)), "{out}");
        assert!(
            out.contains(&format!("{} {{ font: 16px }}", SCOPE)),
            "{out}"
        );
    }

    #[test]
    fn does_not_split_inside_pseudo_functions() {
        let css = ".a:not(.b, .c), .d { color: blue }";
        let out = scope_css(css, SCOPE);
        // :not(.b, .c) stays intact; only the top-level comma splits selectors.
        assert!(out.contains(":not(.b, .c)"), "{out}");
        assert!(out.contains(&format!("{SCOPE} .d")), "{out}");
    }

    #[test]
    fn preserves_font_face_and_keyframes_verbatim() {
        let css = "@font-face { font-family: 'LDOCE'; src: url(f.woff2) }\
                   @keyframes spin { from { transform: none } to { transform: rotate(1turn) } }\
                   .x { color: green }";
        let out = scope_css(css, SCOPE);
        assert!(
            out.contains("@font-face { font-family: 'LDOCE'; src: url(f.woff2) }"),
            "{out}"
        );
        assert!(
            out.contains(
                "@keyframes spin { from { transform: none } to { transform: rotate(1turn) } }"
            ),
            "{out}"
        );
        assert!(out.contains(&format!("{SCOPE} .x")), "{out}");
    }

    #[test]
    fn recurses_into_media_queries() {
        let css = "@media (min-width: 0) { .entry { font-size: 12px } .b, .c { x: 1 } }";
        let out = scope_css(css, SCOPE);
        assert!(out.contains("@media (min-width: 0) {"), "{out}");
        assert!(
            out.contains(&format!("{SCOPE} .entry {{ font-size: 12px }}")),
            "{out}"
        );
        assert!(
            out.contains(&format!("{SCOPE} .b, {SCOPE} .c {{ x: 1 }}")),
            "{out}"
        );
    }

    #[test]
    fn skips_import_and_charset_as_statements() {
        let css = "@charset \"utf-8\"; @import url(x.css); .y { color: red }";
        let out = scope_css(css, SCOPE);
        assert!(out.contains("@charset \"utf-8\";"), "{out}");
        assert!(out.contains("@import url(x.css);"), "{out}");
        assert!(out.contains(&format!("{SCOPE} .y")), "{out}");
    }

    #[test]
    fn handles_comments_and_strings() {
        let css = "/* a, b */ .x /* c */ { color: /* d */ red } .y { content: \"a,b\" }";
        let out = scope_css(css, SCOPE);
        assert!(out.contains("/* a, b */"), "{out}");
        assert!(out.contains(&format!("{SCOPE} .x /* c */")), "{out}");
        assert!(out.contains("\"a,b\""), "{out}");
    }

    #[test]
    fn idempotent_on_real_flat_stylesheet() {
        // lm6.css is a flat list of simple selectors — scoping must touch every
        // top-level rule and never leave a bare leading selector.
        let css = "* { word-wrap: break-word }\n.entry { line-height: 150% }\na { color: #ff5050 }";
        let out = scope_css(css, SCOPE);
        assert!(out.contains(&format!("{SCOPE} *")), "{out}");
        assert!(out.contains(&format!("{SCOPE} .entry")), "{out}");
        assert!(out.contains(&format!("{SCOPE} a")), "{out}");
        // No bare top-level rule leaked through unscoped.
        assert!(!out.starts_with(" ."), "{out}");
    }
}
