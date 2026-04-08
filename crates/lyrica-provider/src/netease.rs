use anyhow::Result;
use serde::Deserialize;

use lyrica_core::lyrics::{self, LyricsMetadata, LyricsSource};
use lyrica_core::provider::{LyricsProvider, SearchRequest};

const NETEASE_API: &str = "https://music.163.com/api";

pub struct NeteaseProvider {
    client: reqwest::Client,
}

impl NeteaseProvider {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")
                .build()
                .unwrap_or_default(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    result: Option<SearchResult>,
}

#[derive(Debug, Deserialize)]
struct SearchResult {
    songs: Option<Vec<Song>>,
}

#[derive(Debug, Deserialize)]
struct Song {
    id: u64,
    name: String,
    artists: Vec<Artist>,
    album: Option<Album>,
    #[allow(dead_code)]
    duration: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct Artist {
    name: String,
}

#[derive(Debug, Deserialize)]
struct Album {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LyricResponse {
    lrc: Option<LrcContent>,
    tlyric: Option<LrcContent>,
}

#[derive(Debug, Deserialize)]
struct LrcContent {
    lyric: Option<String>,
}

#[async_trait::async_trait]
impl LyricsProvider for NeteaseProvider {
    fn name(&self) -> &str {
        "NetEase"
    }

    async fn search(&self, request: &SearchRequest) -> Result<Vec<lyrics::Lyrics>> {
        // Step 1: Search for the song.
        let query = format!("{} {}", request.title, request.artist);
        let search_url = format!("{}/search/get", NETEASE_API);

        let resp = self
            .client
            .post(&search_url)
            .header("Referer", "https://music.163.com/")
            .form(&[("s", &query), ("type", &"1".to_string()), ("limit", &"5".to_string())])
            .send()
            .await?;

        let search_resp: SearchResponse = resp.json().await?;
        let songs = search_resp
            .result
            .and_then(|r| r.songs)
            .unwrap_or_default();

        let mut lyrics_list = Vec::new();

        for song in songs.into_iter().take(3) {
            // Step 2: Fetch lyrics for each song.
            let lyric_url = format!("{}/song/lyric", NETEASE_API);
            let resp = self
                .client
                .get(&lyric_url)
                .query(&[("id", &song.id.to_string()), ("lv", &"1".to_string()), ("tv", &"1".to_string())])
                .header("Referer", "https://music.163.com/")
                .send()
                .await?;

            let lyric_resp: LyricResponse = resp.json().await?;

            let lrc_text = lyric_resp
                .lrc
                .and_then(|l| l.lyric)
                .unwrap_or_default();

            if lrc_text.is_empty() {
                continue;
            }

            if let Ok(mut parsed) = lyrics::lrc::parse(&lrc_text) {
                // Apply translation if available.
                if let Some(tlyric) = lyric_resp.tlyric.and_then(|t| t.lyric) {
                    if let Ok(trans) = lyrics::lrc::parse(&tlyric) {
                        apply_translations(&mut parsed, &trans);
                    }
                }

                let artist_names: Vec<&str> =
                    song.artists.iter().map(|a| a.name.as_str()).collect();

                let quality = calculate_quality(
                    request,
                    &song.name,
                    &artist_names.join(", "),
                );

                parsed.metadata = LyricsMetadata {
                    title: Some(song.name),
                    artist: Some(artist_names.join(", ")),
                    album: song.album.and_then(|a| a.name),
                    source: LyricsSource::NetEase,
                    quality,
                };

                lyrics_list.push(parsed);
            }
        }

        Ok(lyrics_list)
    }
}

/// Merge translation lines into the main lyrics by matching positions.
fn apply_translations(main: &mut lyrics::Lyrics, trans: &lyrics::Lyrics) {
    for tline in &trans.lines {
        if tline.content.is_empty() {
            continue;
        }
        // Find the closest main line by position (within 100ms tolerance).
        if let Some(main_line) = main.lines.iter_mut().min_by_key(|l| {
            let diff = if l.position > tline.position {
                l.position - tline.position
            } else {
                tline.position - l.position
            };
            diff.as_millis()
        }) {
            let diff = if main_line.position > tline.position {
                main_line.position - tline.position
            } else {
                tline.position - main_line.position
            };
            if diff.as_millis() < 100 {
                main_line.translation = Some(tline.content.clone());
            }
        }
    }
}

fn calculate_quality(request: &SearchRequest, title: &str, artist: &str) -> f32 {
    let mut score = 0.0f32;

    if title.to_lowercase() == request.title.to_lowercase() {
        score += 0.5;
    } else if title.to_lowercase().contains(&request.title.to_lowercase()) {
        score += 0.3;
    }

    if artist.to_lowercase() == request.artist.to_lowercase() {
        score += 0.4;
    } else if artist.to_lowercase().contains(&request.artist.to_lowercase()) {
        score += 0.2;
    }

    score += 0.1; // NetEase generally has good quality synced lyrics.
    score.min(1.0)
}
