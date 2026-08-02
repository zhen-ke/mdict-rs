//! Dictionary configuration module
//!
//! Each MDX dictionary can have an optional companion .toml config file
//! for custom styles, scripts, and metadata.

use std::fs;
use std::path::Path;

use serde_derive::Deserialize;
use tracing::{info, warn};

/// Dictionary configuration structure
#[derive(Debug, Clone, Deserialize, Default)]
pub struct DictConfig {
    /// Display name of the dictionary
    pub name: Option<String>,

    /// Description of the dictionary
    pub description: Option<String>,

    /// Version string
    #[allow(dead_code)]
    pub version: Option<String>,

    /// Custom CSS (inline or @file reference)
    pub css: Option<String>,

    /// Custom JavaScript (inline or @file reference)
    pub js: Option<String>,

    /// CSS container class for style scoping
    pub container_class: Option<String>,

    /// Whether FTS should be built for this dictionary (default: true)
    pub fts: Option<bool>,

    /// FTS5 分词器选择（默认 `unicode61`）。可选值：`unicode61` / `trigram`。
    /// 白名单校验在 [`mdict_core::indexing::FtsTokenizer::from_name`]。
    ///
    /// 对中文等无空格分隔的 CJK 词典，`unicode61` 会把每个 CJK 字符拆成单个 token
    /// （召回高但精度低）；选 `trigram`（SQLite 3.34+ 内建）按 3-字符滑动窗口切分，
    /// 能支撑 CJK 子串与混合语料的全文检索召回，是当前离 jieba/ngram 最近的
    /// 零依赖选项。Jieba等复杂分词另一条路（需 C tokenizer ABI），后续可叠。
    pub fts_tokenizer: Option<String>,
}

impl DictConfig {
    /// Load config from a .toml file alongside the MDX file
    ///
    /// For `mdict/dict.mdx`, looks for `mdict/dict.toml`
    pub fn load(mdx_path: impl AsRef<Path>) -> Option<Self> {
        let mdx_path = mdx_path.as_ref();

        // Get the config file path (same name, .toml extension)
        let config_path = mdx_path.with_extension("toml");

        if !config_path.exists() {
            return None;
        }

        match fs::read_to_string(&config_path) {
            Ok(content) => match toml::from_str::<DictConfig>(&content) {
                Ok(config) => {
                    info!("Loaded dict config: {:?}", config_path);
                    Some(config)
                }
                Err(e) => {
                    warn!("Failed to parse dict config {:?}: {}", config_path, e);
                    None
                }
            },
            Err(e) => {
                warn!("Failed to read dict config {:?}: {}", config_path, e);
                None
            }
        }
    }

    /// Get the CSS content, resolving @file references
    pub fn get_css_content(&self, dict_dir: &Path) -> String {
        self.resolve_content(&self.css, dict_dir)
    }

    /// Get the JS content, resolving @file references
    pub fn get_js_content(&self, dict_dir: &Path) -> String {
        self.resolve_content(&self.js, dict_dir)
    }

    /// Resolve content that may be inline or a @file reference.
    /// File references use `@filename` syntax and are restricted to files
    /// within the dictionary directory to prevent path traversal.
    fn resolve_content(&self, content: &Option<String>, dict_dir: &Path) -> String {
        match content {
            Some(value) => {
                let trimmed = value.trim();
                if let Some(filename) = trimmed.strip_prefix('@') {
                    // File reference: @filename.css
                    // Reject obviously malicious paths before hitting the filesystem
                    if filename.contains("..")
                        || filename.starts_with('/')
                        || filename.starts_with('\\')
                    {
                        warn!(
                            "Rejected @file reference with path traversal: {:?}",
                            filename
                        );
                        return String::new();
                    }

                    let file_path = dict_dir.join(filename);

                    // Canonicalize and verify the resolved path stays within dict_dir
                    let canonical_file = match file_path.canonicalize() {
                        Ok(p) => p,
                        Err(e) => {
                            warn!("Failed to resolve referenced file {:?}: {}", file_path, e);
                            return String::new();
                        }
                    };
                    let canonical_dir = match dict_dir.canonicalize() {
                        Ok(p) => p,
                        Err(e) => {
                            warn!("Failed to canonicalize dict dir {:?}: {}", dict_dir, e);
                            return String::new();
                        }
                    };
                    if !canonical_file.starts_with(&canonical_dir) {
                        warn!(
                            "Rejected @file reference escaping dict dir: {:?} not under {:?}",
                            canonical_file, canonical_dir
                        );
                        return String::new();
                    }

                    match fs::read_to_string(&canonical_file) {
                        Ok(file_content) => file_content,
                        Err(e) => {
                            warn!("Failed to read referenced file {:?}: {}", file_path, e);
                            String::new()
                        }
                    }
                } else {
                    // Inline content
                    value.clone()
                }
            }
            None => String::new(),
        }
    }

    /// Get display name, falling back to MDX filename
    pub fn get_display_name(&self, mdx_path: impl AsRef<Path>) -> String {
        let mdx_path = mdx_path.as_ref();
        if let Some(ref name) = self.name {
            name.clone()
        } else {
            // Extract filename without extension
            mdx_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Unknown")
                .to_string()
        }
    }

    pub fn is_fts_enabled(&self) -> bool {
        self.fts.unwrap_or(true)
    }

    /// 该词典在 FTS5 建表时要用的分词器（默认 `unicode61`）。
    /// 未配置或配置了无法识别的名称时回退默认。
    pub fn fts_tokenizer(&self) -> mdict_core::indexing::FtsTokenizer {
        self.fts_tokenizer
            .as_deref()
            .and_then(mdict_core::indexing::FtsTokenizer::from_name)
            .unwrap_or_default()
    }
}

/// Dictionary info for API response
#[derive(Debug, Clone, serde_derive::Serialize)]
pub struct DictInfo {
    /// Stable dictionary ID used by route `/dict/{id}/...`
    pub id: String,

    /// Display name
    pub name: String,

    /// Description
    pub description: Option<String>,

    /// Container class for CSS scoping
    pub container_class: Option<String>,

    /// Whether this dict has custom CSS
    pub has_css: bool,

    /// Whether this dict has custom JS
    pub has_js: bool,

    /// Whether FTS is enabled for this dictionary
    pub fts_enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = DictConfig::default();
        assert!(config.name.is_none());
        assert!(config.css.is_none());
        assert!(config.is_fts_enabled());
    }

    #[test]
    fn test_get_display_name_fallback() {
        let config = DictConfig::default();
        let name = config.get_display_name(Path::new("/path/to/朗文词典.mdx"));
        assert_eq!(name, "朗文词典");
    }
}
