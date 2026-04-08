pub mod lrclib;
pub mod netease;
pub mod qqmusic;
pub mod kugou;

use std::time::Duration;

use anyhow::Result;
use tracing::info;

use lyrica_core::lyrics::Lyrics;
use lyrica_core::provider::{LyricsProvider, SearchRequest};

/// Aggregated provider that searches multiple sources in parallel.
pub struct ProviderGroup {
    providers: Vec<Box<dyn LyricsProvider>>,
    /// Timeout for the entire search operation.
    pub search_timeout: Duration,
    /// After first result, wait this long for better results.
    pub priority_window: Duration,
}

impl ProviderGroup {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
            search_timeout: Duration::from_secs(10),
            priority_window: Duration::from_secs(2),
        }
    }

    /// Create a ProviderGroup with all available providers.
    pub fn with_all_providers() -> Self {
        let mut group = Self::new();
        group.add(Box::new(lrclib::LrclibProvider::new()));
        group.add(Box::new(netease::NeteaseProvider::new()));
        group.add(Box::new(qqmusic::QQMusicProvider::new()));
        group.add(Box::new(kugou::KugouProvider::new()));
        group
    }

    pub fn add(&mut self, provider: Box<dyn LyricsProvider>) {
        self.providers.push(provider);
    }

    /// Search all providers in parallel, returning all candidates sorted by quality.
    pub async fn search_all(&self, request: &SearchRequest) -> Result<Vec<Lyrics>> {
        if self.providers.is_empty() {
            return Ok(Vec::new());
        }

        info!(
            title = %request.title,
            artist = %request.artist,
            "Searching {} providers for lyrics",
            self.providers.len()
        );

        let mut join_set = tokio::task::JoinSet::new();
        for provider in &self.providers {
            let name = provider.name().to_string();
            let req = request.clone();
            // SAFETY: Provider references are valid for the duration of search.
            let provider_ptr = provider.as_ref() as *const dyn LyricsProvider;
            let provider_ref = unsafe { &*provider_ptr };
            join_set.spawn(async move {
                let result = provider_ref.search(&req).await;
                (name, result)
            });
        }

        let mut all_results: Vec<Lyrics> = Vec::new();
        let deadline = tokio::time::Instant::now() + self.search_timeout;
        let mut first_received = false;
        let mut priority_deadline = deadline;

        while let Ok(Some(result)) =
            tokio::time::timeout_at(priority_deadline, join_set.join_next()).await
        {
            match result {
                Ok((name, Ok(mut lyrics_list))) => {
                    info!(
                        provider = %name,
                        count = lyrics_list.len(),
                        "Provider returned results"
                    );
                    if !first_received && !lyrics_list.is_empty() {
                        first_received = true;
                        let window_end = tokio::time::Instant::now() + self.priority_window;
                        if window_end < deadline {
                            priority_deadline = window_end;
                        }
                    }
                    all_results.append(&mut lyrics_list);
                }
                Ok((name, Err(e))) => {
                    tracing::warn!(provider = %name, error = %e, "Provider search failed");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Provider task panicked");
                }
            }
        }

        // Sort by quality descending.
        all_results.sort_by(|a, b| {
            b.metadata
                .quality
                .partial_cmp(&a.metadata.quality)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        info!(count = all_results.len(), "Total candidates found");
        Ok(all_results)
    }

    /// Search all providers, returning only the best result.
    pub async fn search(&self, request: &SearchRequest) -> Result<Option<Lyrics>> {
        let all = self.search_all(request).await?;
        if let Some(best) = all.into_iter().next() {
            info!(
                source = %best.metadata.source,
                quality = best.metadata.quality,
                lines = best.lines.len(),
                "Selected best lyrics"
            );
            Ok(Some(best))
        } else {
            info!("No lyrics found from any provider");
            Ok(None)
        }
    }
}
