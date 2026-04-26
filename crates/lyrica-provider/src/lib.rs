pub mod lrclib;
pub mod netease;
pub mod qqmusic;
pub mod kugou;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Result;
use tracing::info;

use lyrica_core::config::Config;
use lyrica_core::lyrics::Lyrics;
use lyrica_core::provider::{LyricsProvider, SearchRequest};

/// Shared, hot-swappable per-provider weights. Cloned across ProviderGroup
/// instances so a single update propagates everywhere.
pub type WeightsHandle = Arc<RwLock<HashMap<String, f32>>>;

pub fn weights_from_config(config: &Config) -> HashMap<String, f32> {
    let keys = ["lrclib", "netease", "qqmusic", "kugou"];
    keys.into_iter()
        .map(|k| (k.to_string(), config.provider_weight(k)))
        .collect()
}

/// Aggregated provider that searches multiple sources in parallel.
pub struct ProviderGroup {
    providers: Vec<Box<dyn LyricsProvider>>,
    /// Per-provider weight, shared via RwLock so it can be hot-reloaded.
    /// Missing entries default to 1.0; weight <= 0 skips the provider entirely.
    weights: WeightsHandle,
    /// Timeout for the entire search operation.
    pub search_timeout: Duration,
    /// After first result, wait this long for better results.
    pub priority_window: Duration,
}

impl ProviderGroup {
    /// Create a ProviderGroup with all available providers and default weights.
    pub fn with_all_providers() -> Self {
        Self::with_config(&Config::default())
    }

    /// Create a ProviderGroup driven by the given config. Initial weights are
    /// snapshotted from `config`; use `weights_handle()` if callers need to
    /// hot-reload weights later.
    pub fn with_config(config: &Config) -> Self {
        let handle = Arc::new(RwLock::new(weights_from_config(config)));
        Self::with_shared_weights(config, handle)
    }

    /// Like `with_config`, but reuses an externally-owned weights handle so
    /// multiple ProviderGroups (scheduler / API / TUI) share the same live config.
    pub fn with_shared_weights(config: &Config, weights: WeightsHandle) -> Self {
        let providers: Vec<Box<dyn LyricsProvider>> = vec![
            Box::new(lrclib::LrclibProvider::new()),
            Box::new(netease::NeteaseProvider::new()),
            Box::new(qqmusic::QQMusicProvider::new()),
            Box::new(kugou::KugouProvider::new()),
        ];
        Self {
            providers,
            weights,
            search_timeout: Duration::from_secs(config.search_timeout_secs),
            priority_window: Duration::from_secs(config.priority_window_secs),
        }
    }

    /// Handle to the shared weights map. Callers can `write()` to update
    /// weights at runtime; new values are picked up on the next search.
    pub fn weights_handle(&self) -> WeightsHandle {
        self.weights.clone()
    }

    /// Snapshot current weights for use during a single search.
    fn snapshot_weights(&self) -> HashMap<String, f32> {
        self.weights.read().map(|g| g.clone()).unwrap_or_default()
    }

    /// Search all providers in parallel, returning all candidates sorted by quality.
    pub async fn search_all(&self, request: &SearchRequest) -> Result<Vec<Lyrics>> {
        if self.providers.is_empty() {
            return Ok(Vec::new());
        }

        let weights = self.snapshot_weights();

        // Decide which providers to query under the current weights snapshot.
        let active: Vec<&dyn LyricsProvider> = self
            .providers
            .iter()
            .filter(|p| weights.get(p.key()).copied().unwrap_or(1.0) > 0.0)
            .map(|p| p.as_ref())
            .collect();

        if active.is_empty() {
            info!("All providers disabled by weight=0; skipping search");
            return Ok(Vec::new());
        }

        info!(
            title = %request.title,
            artist = %request.artist,
            "Searching {} providers for lyrics",
            active.len()
        );

        let mut join_set = tokio::task::JoinSet::new();
        for provider in &active {
            let name = provider.name().to_string();
            let req = request.clone();
            // SAFETY: Provider references are valid for the duration of search.
            let provider_ptr = *provider as *const dyn LyricsProvider;
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

        // Sort by weighted quality (quality * provider weight) descending.
        // Weight is not baked into metadata.quality so cached/API output stays unbiased.
        all_results.sort_by(|a, b| {
            let aw = a
                .metadata
                .source
                .config_key()
                .and_then(|k| weights.get(k).copied())
                .unwrap_or(1.0);
            let bw = b
                .metadata
                .source
                .config_key()
                .and_then(|k| weights.get(k).copied())
                .unwrap_or(1.0);
            (b.metadata.quality * bw)
                .partial_cmp(&(a.metadata.quality * aw))
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

#[cfg(test)]
mod tests {
    use super::*;
    use lyrica_core::config::ProviderConfig;
    use lyrica_core::lyrics::{Lyrics, LyricsMetadata, LyricsSource};

    fn make_lyrics(source: LyricsSource, quality: f32) -> Lyrics {
        Lyrics {
            lines: Vec::new(),
            metadata: LyricsMetadata {
                title: None,
                artist: None,
                album: None,
                source,
                quality,
            },
            offset_ms: 0,
        }
    }

    #[test]
    fn weight_zero_filters_at_search_time() {
        // weight=0 no longer removes the provider from the group; it gates the
        // search at runtime so live reloads can re-enable it.
        let mut config = Config::default();
        config.providers.insert("kugou".into(), ProviderConfig { weight: 0.0 });
        let group = ProviderGroup::with_config(&config);
        assert_eq!(group.providers.len(), 4);
        let weights = group.snapshot_weights();
        assert_eq!(weights.get("kugou"), Some(&0.0));
    }

    #[test]
    fn weighted_quality_orders_results() {
        let mut config = Config::default();
        config.providers.insert("netease".into(), ProviderConfig { weight: 2.0 });
        config.providers.insert("lrclib".into(), ProviderConfig { weight: 0.5 });
        let group = ProviderGroup::with_config(&config);
        let weights = group.snapshot_weights();

        let mut results = vec![
            make_lyrics(LyricsSource::Lrclib, 0.9),   // weighted: 0.45
            make_lyrics(LyricsSource::NetEase, 0.6),  // weighted: 1.20
        ];
        results.sort_by(|a, b| {
            let aw = a
                .metadata
                .source
                .config_key()
                .and_then(|k| weights.get(k).copied())
                .unwrap_or(1.0);
            let bw = b
                .metadata
                .source
                .config_key()
                .and_then(|k| weights.get(k).copied())
                .unwrap_or(1.0);
            (b.metadata.quality * bw)
                .partial_cmp(&(a.metadata.quality * aw))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        assert_eq!(results[0].metadata.source, LyricsSource::NetEase);
    }

    #[test]
    fn shared_weights_propagate_across_groups() {
        let config = Config::default();
        let group_a = ProviderGroup::with_config(&config);
        let handle = group_a.weights_handle();
        let group_b = ProviderGroup::with_shared_weights(&config, handle.clone());

        // Mutate via the handle; both groups should see the update.
        handle.write().unwrap().insert("netease".into(), 5.0);
        assert_eq!(group_a.snapshot_weights().get("netease"), Some(&5.0));
        assert_eq!(group_b.snapshot_weights().get("netease"), Some(&5.0));
    }
}
