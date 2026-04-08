use anyhow::Result;
use serde::Deserialize;

use lyrica_core::lyrics::{self, LyricsMetadata, LyricsSource};
use lyrica_core::provider::{LyricsProvider, SearchRequest};

const LRCLIB_API: &str = "https://lrclib.net/api";

pub struct LrclibProvider {
    client: reqwest::Client,
}

impl LrclibProvider {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("lyrica/0.1.0")
                .build()
                .unwrap_or_default(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct LrclibResult {
    #[serde(rename = "trackName")]
    track_name: Option<String>,
    #[serde(rename = "artistName")]
    artist_name: Option<String>,
    #[serde(rename = "albumName")]
    album_name: Option<String>,
    #[serde(rename = "syncedLyrics")]
    synced_lyrics: Option<String>,
    #[serde(rename = "plainLyrics")]
    plain_lyrics: Option<String>,
    #[allow(dead_code)]
    duration: Option<f64>,
}

#[async_trait::async_trait]
impl LyricsProvider for LrclibProvider {
    fn name(&self) -> &str {
        "LRCLIB"
    }

    async fn search(&self, request: &SearchRequest) -> Result<Vec<lyrics::Lyrics>> {
        let url = format!("{}/search", LRCLIB_API);
        let query = format!("{} {}", request.title, request.artist);

        let resp = self
            .client
            .get(&url)
            .query(&[("q", &query)])
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("LRCLIB search returned status {}", resp.status());
        }

        let results: Vec<LrclibResult> = resp.json().await?;
        let mut lyrics_list = Vec::new();

        for result in results.into_iter().take(5) {
            // Prefer synced lyrics over plain.
            let lrc_text = match (&result.synced_lyrics, &result.plain_lyrics) {
                (Some(synced), _) => synced.clone(),
                (None, Some(plain)) => plain.clone(),
                _ => continue,
            };

            if let Ok(mut parsed) = lyrics::lrc::parse(&lrc_text) {
                // Calculate quality based on title/artist match.
                let quality = calculate_quality(request, &result);
                parsed.metadata = LyricsMetadata {
                    title: result.track_name,
                    artist: result.artist_name,
                    album: result.album_name,
                    source: LyricsSource::Lrclib,
                    quality,
                };
                lyrics_list.push(parsed);
            }
        }

        Ok(lyrics_list)
    }
}

fn calculate_quality(request: &SearchRequest, result: &LrclibResult) -> f32 {
    let mut score = 0.0f32;

    if let Some(ref title) = result.track_name {
        if title.to_lowercase() == request.title.to_lowercase() {
            score += 0.5;
        } else if title.to_lowercase().contains(&request.title.to_lowercase()) {
            score += 0.3;
        }
    }

    if let Some(ref artist) = result.artist_name {
        if artist.to_lowercase() == request.artist.to_lowercase() {
            score += 0.4;
        } else if artist.to_lowercase().contains(&request.artist.to_lowercase()) {
            score += 0.2;
        }
    }

    // Bonus for having synced lyrics.
    if result.synced_lyrics.is_some() {
        score += 0.1;
    }

    score.min(1.0)
}
