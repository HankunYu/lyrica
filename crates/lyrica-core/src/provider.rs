use std::time::Duration;

use super::lyrics::Lyrics;

/// A request to search for lyrics.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration: Option<Duration>,
}

/// Trait for lyrics source providers to implement.
#[async_trait::async_trait]
pub trait LyricsProvider: Send + Sync {
    /// Provider name for display/logging.
    fn name(&self) -> &str;

    /// Stable lowercase key used in config files (e.g. "lrclib", "netease").
    /// Distinct from `name()` because config keys must remain stable even if
    /// the user-visible name changes.
    fn key(&self) -> &str;

    /// Search for lyrics matching the request.
    /// Returns a list of candidates sorted by relevance.
    async fn search(&self, request: &SearchRequest) -> anyhow::Result<Vec<Lyrics>>;
}
