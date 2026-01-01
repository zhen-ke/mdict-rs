//! Dictionary configuration module
//!
//! Each MDX dictionary can have an optional companion .toml config file
//! for custom styles, scripts, and metadata.

use std::fs;
use std::path::{Path, PathBuf};

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
    pub version: Option<String>,

    /// Custom CSS (inline or @file reference)
    pub css: Option<String>,

    /// Custom JavaScript (inline or @file reference)
    pub js: Option<String>,

    /// CSS container class for style scoping
    pub container_class: Option<String>,
}

impl DictConfig {
    /// Load config from a .toml file alongside the MDX file
    ///
    /// For `mdict/dict.mdx`, looks for `mdict/dict.toml`
    pub fn load(mdx_path: &str) -> Option<Self> {
        let mdx_path = Path::new(mdx_path);

        // Get the config file path (same name, .toml extension)
        let config_path = mdx_path.with_extension("toml");

        if !config_path.exists() {
            return None;
        }

        match fs::read_to_string(&config_path) {
            Ok(content) => {
                match toml::from_str::<DictConfig>(&content) {
                    Ok(config) => {
                        info!("Loaded dict config: {:?}", config_path);
                        Some(config)
                    }
                    Err(e) => {
                        warn!("Failed to parse dict config {:?}: {}", config_path, e);
                        None
                    }
                }
            }
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

    /// Resolve content that may be inline or a @file reference
    fn resolve_content(&self, content: &Option<String>, dict_dir: &Path) -> String {
        match content {
            Some(value) => {
                let trimmed = value.trim();
                if trimmed.starts_with('@') {
                    // File reference: @filename.css
                    let filename = &trimmed[1..];
                    let file_path = dict_dir.join(filename);

                    match fs::read_to_string(&file_path) {
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
    pub fn get_display_name(&self, mdx_path: &str) -> String {
        if let Some(ref name) = self.name {
            name.clone()
        } else {
            // Extract filename without extension
            Path::new(mdx_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Unknown")
                .to_string()
        }
    }
}

/// Dictionary info for API response
#[derive(Debug, Clone, serde_derive::Serialize)]
pub struct DictInfo {
    /// Internal ID (file path)
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = DictConfig::default();
        assert!(config.name.is_none());
        assert!(config.css.is_none());
    }

    #[test]
    fn test_get_display_name_fallback() {
        let config = DictConfig::default();
        let name = config.get_display_name("/path/to/朗文词典.mdx");
        assert_eq!(name, "朗文词典");
    }
}
