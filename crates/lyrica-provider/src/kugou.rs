use anyhow::Result;
use serde::Deserialize;

use lyrica_core::lyrics::{self, LyricsMetadata, LyricsSource};
use lyrica_core::provider::{LyricsProvider, SearchRequest};

pub struct KugouProvider {
    client: reqwest::Client,
}

impl KugouProvider {
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
    data: Option<SearchData>,
}

#[derive(Debug, Deserialize)]
struct SearchData {
    info: Option<Vec<SongInfo>>,
}

#[derive(Debug, Deserialize)]
struct SongInfo {
    hash: String,
    songname: String,
    singername: Option<String>,
    album_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LyricSearchResponse {
    candidates: Option<Vec<LyricCandidate>>,
}

#[derive(Debug, Deserialize)]
struct LyricCandidate {
    id: String,
    accesskey: String,
}

#[derive(Debug, Deserialize)]
struct LyricDetailResponse {
    content: Option<String>,
}

#[async_trait::async_trait]
impl LyricsProvider for KugouProvider {
    fn name(&self) -> &str {
        "Kugou"
    }

    fn key(&self) -> &str {
        "kugou"
    }

    async fn search(&self, request: &SearchRequest) -> Result<Vec<lyrics::Lyrics>> {
        let query = format!("{} {}", request.title, request.artist);

        // Step 1: Search for songs.
        let search_url = "https://mobileservice.kugou.com/api/v3/search/song";
        let resp = self
            .client
            .get(search_url)
            .query(&[
                ("keyword", query.as_str()),
                ("page", "1"),
                ("pagesize", "5"),
            ])
            .send()
            .await?;

        let search_resp: SearchResponse = resp.json().await?;
        let songs = search_resp
            .data
            .and_then(|d| d.info)
            .unwrap_or_default();

        let mut lyrics_list = Vec::new();

        for song in songs.into_iter().take(3) {
            // Step 2: Search for lyrics by song hash.
            let lyric_search_url = "https://krcs.kugou.com/search";
            let duration_ms = request
                .duration
                .map(|d| d.as_millis().to_string())
                .unwrap_or_default();

            let resp = self
                .client
                .get(lyric_search_url)
                .query(&[
                    ("keyword", query.as_str()),
                    ("hash", song.hash.as_str()),
                    ("duration", duration_ms.as_str()),
                    ("client", "pc"),
                    ("ver", "1"),
                    ("man", "yes"),
                ])
                .send()
                .await?;

            let lyric_search: LyricSearchResponse = match resp.json().await {
                Ok(r) => r,
                Err(_) => continue,
            };

            let candidate = match lyric_search.candidates.and_then(|c| c.into_iter().next()) {
                Some(c) => c,
                None => continue,
            };

            // Step 3: Fetch the actual lyrics content.
            let lyric_detail_url = "https://krcs.kugou.com/download";
            let resp = self
                .client
                .get(lyric_detail_url)
                .query(&[
                    ("id", candidate.id.as_str()),
                    ("accesskey", candidate.accesskey.as_str()),
                    ("fmt", "lrc"),
                    ("charset", "utf8"),
                    ("client", "pc"),
                    ("ver", "1"),
                ])
                .send()
                .await?;

            let detail: LyricDetailResponse = match resp.json().await {
                Ok(r) => r,
                Err(_) => continue,
            };

            let lrc_text = match detail.content {
                Some(ref text) if !text.is_empty() => {
                    // Content may be base64 encoded.
                    decode_content(text)
                }
                _ => continue,
            };

            if let Ok(mut parsed) = lyrics::lrc::parse(&lrc_text) {
                let quality = calculate_quality(
                    request,
                    &song.songname,
                    song.singername.as_deref().unwrap_or(""),
                );

                parsed.metadata = LyricsMetadata {
                    title: Some(song.songname),
                    artist: song.singername,
                    album: song.album_name,
                    source: LyricsSource::Kugou,
                    quality,
                };

                lyrics_list.push(parsed);
            }
        }

        Ok(lyrics_list)
    }
}

/// Try to decode base64 content, fall back to raw text.
fn decode_content(text: &str) -> String {
    // Try base64 decode.
    if let Ok(decoded) = base64_decode(text.trim()) {
        if let Ok(s) = String::from_utf8(decoded) {
            return s;
        }
    }
    text.to_string()
}

fn base64_decode(input: &str) -> Result<Vec<u8>> {
    // Simple base64 decode without external dependency.
    let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = Vec::new();
    let input: Vec<u8> = input.bytes().filter(|&b| b != b'\n' && b != b'\r' && b != b' ').collect();
    for chunk in input.chunks(4) {
        let mut buf = [0u8; 4];
        let mut valid = 0;
        for (i, &byte) in chunk.iter().enumerate() {
            if byte == b'=' {
                break;
            }
            buf[i] = table.iter().position(|&b| b == byte)
                .ok_or_else(|| anyhow::anyhow!("invalid base64"))? as u8;
            valid = i + 1;
        }
        if valid >= 2 {
            output.push((buf[0] << 2) | (buf[1] >> 4));
        }
        if valid >= 3 {
            output.push((buf[1] << 4) | (buf[2] >> 2));
        }
        if valid >= 4 {
            output.push((buf[2] << 6) | buf[3]);
        }
    }
    Ok(output)
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
    score.min(1.0)
}
