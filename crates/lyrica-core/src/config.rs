use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Global application configuration.
///
/// Loaded from `$XDG_CONFIG_HOME/lyrica/config.toml`
/// (defaults to `~/.config/lyrica/config.toml`).
/// Missing file is not an error; defaults are used.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Preferred player name (empty = auto-detect).
    #[serde(default)]
    pub player: String,

    /// If true, only attach to the preferred player and never fall back to
    /// another MPRIS source. Has no effect when `player` is empty.
    #[serde(default)]
    pub strict_player: bool,

    /// Lyrics search timeout in seconds.
    #[serde(default = "default_search_timeout")]
    pub search_timeout_secs: u64,

    /// Priority window: after first result arrives, wait this long for better results.
    #[serde(default = "default_priority_window")]
    pub priority_window_secs: u64,

    /// HTTP API server port (0 = disabled).
    #[serde(default)]
    pub api_port: u16,

    /// Lyrics cache directory (empty = default ~/.cache/lyrica).
    #[serde(default)]
    pub cache_dir: String,

    /// Per-provider configuration, keyed by provider key (lrclib / netease / qqmusic / kugou).
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
}

/// Per-provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Multiplicative weight applied to the quality score when ranking results.
    /// Default 1.0. Setting weight to 0 disables the provider entirely.
    #[serde(default = "default_weight")]
    pub weight: f32,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self { weight: default_weight() }
    }
}

fn default_search_timeout() -> u64 {
    10
}

fn default_priority_window() -> u64 {
    2
}

fn default_weight() -> f32 {
    1.0
}

impl Default for Config {
    fn default() -> Self {
        Self {
            player: String::new(),
            strict_player: false,
            search_timeout_secs: default_search_timeout(),
            priority_window_secs: default_priority_window(),
            api_port: 0,
            cache_dir: String::new(),
            providers: HashMap::new(),
        }
    }
}

impl Config {
    /// Resolve the default config file path. Per-platform:
    ///   Linux:  `$XDG_CONFIG_HOME/lyrica/config.toml` (or `~/.config/lyrica/config.toml`)
    ///   macOS:  `~/Library/Application Support/lyrica/config.toml`
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("lyrica")
            .join("config.toml")
    }

    /// Load config from the default path. Returns defaults if the file does
    /// not exist or fails to parse (a warning is logged in the latter case).
    pub fn load() -> Self {
        Self::load_from(&Self::default_path())
    }

    /// Load config from an explicit path.
    pub fn load_from(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => {
                    info!(path = %path.display(), "Loaded config");
                    cfg
                }
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "Failed to parse config, using defaults");
                    Config::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                info!(path = %path.display(), "No config file, using defaults");
                Config::default()
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Failed to read config, using defaults");
                Config::default()
            }
        }
    }

    /// Get the weight for a provider key. Returns 1.0 if not configured.
    pub fn provider_weight(&self, key: &str) -> f32 {
        self.providers
            .get(key)
            .map(|p| p.weight)
            .unwrap_or(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_returns_default() {
        let cfg = Config::load_from(std::path::Path::new("/nonexistent/lyrica/config.toml"));
        assert_eq!(cfg.search_timeout_secs, 10);
        assert_eq!(cfg.priority_window_secs, 2);
        assert!(cfg.providers.is_empty());
        assert_eq!(cfg.provider_weight("netease"), 1.0);
    }

    #[test]
    fn parses_provider_weights() {
        let toml = r#"
[providers.netease]
weight = 1.5

[providers.kugou]
weight = 0.0
"#;
        let cfg: Config = toml::from_str(toml).expect("valid toml");
        assert_eq!(cfg.provider_weight("netease"), 1.5);
        assert_eq!(cfg.provider_weight("kugou"), 0.0);
        assert_eq!(cfg.provider_weight("lrclib"), 1.0); // unset -> default
    }

    #[test]
    fn empty_table_uses_default_weight() {
        let toml = "[providers.qqmusic]\n";
        let cfg: Config = toml::from_str(toml).expect("valid toml");
        assert_eq!(cfg.provider_weight("qqmusic"), 1.0);
    }
}
