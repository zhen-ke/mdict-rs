use std::path::Path;
use std::sync::OnceLock;

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use regex::Regex;

static ATTR_RE: OnceLock<Regex> = OnceLock::new();
static SRCSET_RE: OnceLock<Regex> = OnceLock::new();
static STYLE_BLOCK_RE: OnceLock<Regex> = OnceLock::new();
static STYLE_ATTR_RE: OnceLock<Regex> = OnceLock::new();
static URL_FN_RE: OnceLock<Regex> = OnceLock::new();

/// Rewrite every asset reference in a dictionary entry's HTML so it resolves
/// against this server's per-dictionary routes.
///
/// Handles, in one pass over the relevant tokens:
/// - `src` / `href` / `data-src` / `data-href` / `poster` / `xlink:href`
///   attributes (URL → route via [`rewrite_link`]).
/// - `srcset` attributes (comma-separated candidate list; each URL rewritten).
/// - `url(...)` references inside `<style>` blocks *and* inline `style="..."`
///   attributes (fonts / background images / sprite icons).
///
/// `sound://`, `entry://` and external links are handled as before. Dictionary
/// scripts are left untouched (the Shadow-DOM render shell re-creates and runs
/// them, scoped per section — see `presenter::render_aggregate_html`).
pub fn rewrite_html(html: &str, dict_id: &str) -> String {
    let attr_re = ATTR_RE.get_or_init(|| {
        // `xlink:href` is listed before `href` so the colon-containing name
        // is matched as a whole at the `x` position rather than letting `href`
        // match the suffix after the colon (which would drop the `xlink:`
        // prefix and corrupt SVG `<use>`/`<image>` references).
        Regex::new(
            r#"(?i)\b(src|href|xlink:href|data-src|data-href|poster)\s*=\s*["']([^"']+)["']"#,
        )
        .expect("valid attribute regex")
    });

    let out = attr_re.replace_all(html, |caps: &regex::Captures| {
        let attr = &caps[1];
        let value = &caps[2];
        let full_match = &caps[0];
        let Some(new_val) = rewrite_link(attr, value, dict_id) else {
            return full_match.to_string();
        };
        format!(r#"{}="{}""#, attr, new_val)
    });
    let mut out = out.into_owned();

    // srcset="a.png 1x, b.png 2x" / "cover.jpg 480w" — rewrite each candidate URL.
    let srcset_re = SRCSET_RE.get_or_init(|| {
        Regex::new(r#"(?i)\bsrcset\s*=\s*["']([^"']+)["']"#).expect("valid srcset regex")
    });
    let tmp = srcset_re.replace_all(&out, |caps: &regex::Captures| {
        let value = &caps[1];
        let rewritten = rewrite_srcset(value, dict_id);
        format!(r#"srcset="{}""#, rewritten)
    });
    out = tmp.into_owned();

    // url(...) inside <style> blocks (fonts, background images, sprites).
    let block_re = STYLE_BLOCK_RE.get_or_init(|| {
        Regex::new(r#"(?is)(<style\b[^>]*>)(.*?)(</style>)"#).expect("valid style block regex")
    });
    let tmp = block_re.replace_all(&out, |caps: &regex::Captures| {
        let open = &caps[1];
        let body = &caps[2];
        let close = &caps[3];
        format!("{}{}{}", open, rewrite_css_urls(body, dict_id), close)
    });
    out = tmp.into_owned();

    // url(...) inside inline style="..." attributes. The attribute value may
    // contain the *other* quote inside url() (e.g. style="..url('x.png').."),
    // so we match delimiter-aware rather than with a single `[^"']*` class.
    let style_attr_re = STYLE_ATTR_RE.get_or_init(|| {
        Regex::new(r#"(?is)\bstyle\s*=\s*(?:"([^"]*)"|'([^']*)')"#).expect("valid style attr regex")
    });
    let tmp = style_attr_re.replace_all(&out, |caps: &regex::Captures| {
        // Exactly one of the two alternatives matched; pick the captured value
        // and remember which quote delimiter was used so we emit valid HTML.
        let (value, quote) = if let Some(v) = caps.get(1) {
            (v.as_str(), '"')
        } else if let Some(v) = caps.get(2) {
            (v.as_str(), '\'')
        } else {
            return caps[0].to_string();
        };
        if !value.contains("url(") {
            return caps[0].to_string();
        }
        let rewritten = rewrite_css_urls(value, dict_id);
        format!("style={quote}{rewritten}{quote}")
    });
    out = tmp.into_owned();

    out
}

/// Rewrite each `url(...)` token inside a CSS fragment to a dict-scoped
/// `/dict/{id}/res/...` absolute path. External / data: / fragment URLs are
/// left untouched. Uses a single non-backtracking pass over `url(...)`
/// tokens.
///
/// Shared by the per-entry inline `<style>` / `style=` rewrite ([`rewrite_html`])
/// and the resource handler's CSS-file rewrite (where it runs after
/// [`mdict_core::css_scope::scope_css`] so font/background `url()` references
/// in a dictionary's own `.css` resolve against that dict's `/res/` route
/// instead of leaking to the global resource handler).
pub fn rewrite_css_urls(css: &str, dict_id: &str) -> String {
    let url_re = URL_FN_RE.get_or_init(|| {
        // `url( <url> )` — optional surrounding quotes, trim whitespace. The
        // captured URL excludes quotes, parens and whitespace.
        Regex::new(r#"(?i)url\(\s*['"]?([^'")\s]+)['"]?\s*\)"#).expect("valid url() regex")
    });
    url_re
        .replace_all(css, |caps: &regex::Captures| {
            let raw = &caps[1];
            let Some(new_val) = rewrite_resource_url(raw, dict_id) else {
                return caps[0].to_string();
            };
            format!("url({})", new_val)
        })
        .into_owned()
}

/// Rewrite a `srcset` value: `a.png 1x, b.png 2x, ...` — each candidate is a URL
/// optionally followed by a width/density descriptor. Only the URL part is
/// rewritten; the descriptor is preserved.
fn rewrite_srcset(value: &str, dict_id: &str) -> String {
    value
        .split(',')
        .map(|item| {
            let trimmed = item.trim();
            if trimmed.is_empty() {
                return String::new();
            }
            let mut parts = trimmed.split_whitespace();
            let Some(url) = parts.next() else {
                return trimmed.to_string();
            };
            let descriptor = parts.collect::<Vec<_>>().join(" ");
            match rewrite_resource_url(url, dict_id) {
                Some(new_url) => {
                    if descriptor.is_empty() {
                        new_url
                    } else {
                        format!("{new_url} {descriptor}")
                    }
                }
                None => trimmed.to_string(),
            }
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Rewrite a bare resource URL (used by `url()`, `srcset`, `poster`, …) to a
/// dict-scoped route. Returns `None` for external / data: / fragment / empty
/// values so the caller can leave the original token untouched.
fn rewrite_resource_url(value: &str, dict_id: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || is_external_link(value) {
        return None;
    }
    let path = normalize_resource_path(value);
    if path.is_empty() {
        return None;
    }
    Some(format!("/dict/{}/res/{}", dict_id, path))
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
        let (base, suffix) = split_base_and_suffix(target);
        let entry_word = normalize_entry_word(base);
        if entry_word.is_empty() {
            if !suffix.is_empty() {
                return Some(suffix.to_string());
            }
            return None;
        }
        return Some(format!(
            "/dict/{}/entry/{}{}",
            dict_id,
            encode_entry_word(&entry_word),
            suffix
        ));
    }

    let (base, suffix) = split_base_and_suffix(value);
    if base.is_empty() {
        if !suffix.is_empty() {
            return Some(suffix.to_string());
        }
        return None;
    }

    let attr_lc = attr.to_ascii_lowercase();
    if attr_lc == "src"
        || attr_lc == "data-src"
        || attr_lc == "poster"
        || attr_lc == "xlink:href"
        || looks_like_resource_link(base)
    {
        let path = normalize_resource_path(base);
        if path.is_empty() {
            return None;
        }
        return Some(format!("/dict/{}/res/{}{}", dict_id, path, suffix));
    }

    // `href` / `data-href` plain word → entry route (cross-dict link).
    let entry_word = normalize_entry_word(base);
    if entry_word.is_empty() {
        if !suffix.is_empty() {
            return Some(suffix.to_string());
        }
        return None;
    }
    Some(format!(
        "/dict/{}/entry/{}{}",
        dict_id,
        encode_entry_word(&entry_word),
        suffix
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
    let cleaned = path.trim().replace('\\', "/");
    let cleaned = cleaned.trim_start_matches("./");
    let cleaned = cleaned.trim_start_matches('/');

    cleaned
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".." && *part != ".")
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
    fn rewrites_entry_scheme_with_anchor() {
        let html = r#"<a href="entry://#LDOCE6_weather_1">Noun</a><a href="entry://weather#LDOCE6_weather_2">Verb</a>"#;
        let out = rewrite_html(html, "d1");
        assert!(out.contains(r##"href="#LDOCE6_weather_1""##));
        assert!(out.contains(r##"href="/dict/d1/entry/weather#LDOCE6_weather_2""##));
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

    #[test]
    fn rewrites_data_src_lazyload() {
        let html = r#"<img data-src="img/lazy.jpg" src="placeholder.gif">"#;
        let out = rewrite_html(html, "d1");
        assert!(out.contains(r#"data-src="/dict/d1/res/img/lazy.jpg""#));
        assert!(out.contains(r#"src="/dict/d1/res/placeholder.gif""#));
    }

    #[test]
    fn rewrites_data_href_entry_link() {
        let html = r#"<a data-href="serendipity">x</a>"#;
        let out = rewrite_html(html, "d1");
        assert!(out.contains(r#"data-href="/dict/d1/entry/serendipity""#));
    }

    #[test]
    fn rewrites_video_poster() {
        let html = r#"<video poster="cover.png"></video>"#;
        let out = rewrite_html(html, "d1");
        assert!(out.contains(r#"poster="/dict/d1/res/cover.png""#));
    }

    #[test]
    fn rewrites_svg_xlink_href_image() {
        let html = r#"<image xlink:href="pic/logo.svg"></image>"#;
        let out = rewrite_html(html, "d1");
        assert!(out.contains(r#"xlink:href="/dict/d1/res/pic/logo.svg""#));
    }

    #[test]
    fn preserves_svg_use_local_fragment_xlink_href() {
        let html = r##"<use xlink:href="#icon-play"></use>"##;
        let out = rewrite_html(html, "d1");
        // #… is treated as external/fragment → left untouched.
        assert!(out.contains(r##"xlink:href="#icon-play""##));
    }

    #[test]
    fn rewrites_srcset_candidates() {
        let html = r#"<img srcset="img/lo.png 480w, img/hi.png 1080w">"#;
        let out = rewrite_html(html, "d1");
        assert!(
            out.contains(r#"srcset="/dict/d1/res/img/lo.png 480w, /dict/d1/res/img/hi.png 1080w""#)
        );
    }

    #[test]
    fn rewrites_css_file_urls_across_at_rules_and_declarations() {
        // The resource handler serves a dictionary's own `.css` from
        // `/dict/{id}/res/...` after running it through `scope_css` + this
        // `rewrite_css_urls`. Every `url(...)` — in `@import`, `@font-face
        // src`, `background`, `content` — must resolve under that dict's res
        // route; external/data: left alone.
        let css = "\
@import url(base.css);\n\
@font-face { font-family: 'LDOCE'; src: url(fonts/f.woff2) }\n\
.x { background: url('img/bg.png') no-repeat }\n\
.y { list-style: url(https://cdn/x/a.svg) }\n\
.z { content: url(data:image/png;base64,AAA) }";
        let out = super::rewrite_css_urls(css, "d7");
        assert!(
            out.contains(r#"@import url(/dict/d7/res/base.css);"#),
            "{out}"
        );
        assert!(out.contains(r#"url(/dict/d7/res/fonts/f.woff2)"#), "{out}");
        assert!(out.contains(r#"url(/dict/d7/res/img/bg.png)"#), "{out}");
        // External absolute + data: left untouched.
        assert!(out.contains(r#"url(https://cdn/x/a.svg)"#), "{out}");
        assert!(out.contains(r#"url(data:image/png;base64,AAA)"#), "{out}");
    }

    #[test]
    fn rewrites_url_in_style_block() {
        let html =
            r#"<style>.x{background:url(img/bg.png)}@font-face{src:url(fonts/f.woff2)}</style>"#;
        let out = rewrite_html(html, "d1");
        assert!(out.contains(r#"url(/dict/d1/res/img/bg.png)"#));
        assert!(out.contains(r#"url(/dict/d1/res/fonts/f.woff2)"#));
    }

    #[test]
    fn rewrites_url_in_inline_style_attr() {
        let html = r#"<div style="background-image:url('img/bg.jpg')">x</div>"#;
        let out = rewrite_html(html, "d1");
        assert!(out.contains(r#"url(/dict/d1/res/img/bg.jpg)"#));
    }

    #[test]
    fn leaves_external_and_data_url_untouched() {
        let html = r#"<style>.a{background:url(https://x/y.png)}.b{background:url(data:image/png;base64,AAA)}</style>"#;
        let out = rewrite_html(html, "d1");
        assert!(out.contains("url(https://x/y.png)"));
        assert!(out.contains("url(data:image/png;base64,AAA)"));
    }

    #[test]
    fn does_not_touch_script_content() {
        let html = r#"<script>var s="url(nope.png)";</script>"#;
        let out = rewrite_html(html, "d1");
        // url() inside <script> must NOT be rewritten (would corrupt dict JS).
        assert!(out.contains(r#""url(nope.png)""#));
    }
}
