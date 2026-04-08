use serde::{Deserialize, Serialize};

/// Global application configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Preferred player name (empty = auto-detect).
    #[serde(default)]
    pub player: String,

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
}

fn default_search_timeout() -> u64 {
    10
}

fn default_priority_window() -> u64 {
    2
}

impl Default for Config {
    fn default() -> Self {
        Self {
            player: String::new(),
            search_timeout_secs: default_search_timeout(),
            priority_window_secs: default_priority_window(),
            api_port: 0,
            cache_dir: String::new(),
        }
    }
}
