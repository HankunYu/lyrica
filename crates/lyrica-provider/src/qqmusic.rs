use anyhow::Result;
use serde::Deserialize;

use lyrica_core::lyrics::{self, LyricsMetadata, LyricsSource};
use lyrica_core::provider::{LyricsProvider, SearchRequest};

pub struct QQMusicProvider {
    client: reqwest::Client,
}

impl QQMusicProvider {
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
    song: Option<SongList>,
}

#[derive(Debug, Deserialize)]
struct SongList {
    list: Vec<SongItem>,
}

#[derive(Debug, Deserialize)]
struct SongItem {
    songmid: String,
    songname: String,
    singer: Vec<Singer>,
    albumname: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Singer {
    name: String,
}

#[derive(Debug, Deserialize)]
struct LyricResponse {
    lyric: Option<String>,
    trans: Option<String>,
}

#[async_trait::async_trait]
impl LyricsProvider for QQMusicProvider {
    fn name(&self) -> &str {
        "QQ Music"
    }

    fn key(&self) -> &str {
        "qqmusic"
    }

    async fn search(&self, request: &SearchRequest) -> Result<Vec<lyrics::Lyrics>> {
        let query = format!("{} {}", request.title, request.artist);

        // Step 1: Search for songs.
        let search_url = "https://c.y.qq.com/soso/fcgi-bin/client_search_cp";
        let resp = self
            .client
            .get(search_url)
            .query(&[
                ("w", query.as_str()),
                ("format", "json"),
                ("p", "1"),
                ("n", "5"),
            ])
            .header("Referer", "https://y.qq.com/")
            .send()
            .await?;

        let search_resp: SearchResponse = resp.json().await?;
        let songs = search_resp
            .data
            .and_then(|d| d.song)
            .map(|s| s.list)
            .unwrap_or_default();

        let mut lyrics_list = Vec::new();

        for song in songs.into_iter().take(3) {
            // Step 2: Fetch lyrics by songmid.
            let lyric_url = "https://c.y.qq.com/lyric/fcgi-bin/fcg_query_lyric_new.fcg";
            let resp = self
                .client
                .get(lyric_url)
                .query(&[
                    ("songmid", song.songmid.as_str()),
                    ("format", "json"),
                    ("nobase64", "1"),
                ])
                .header("Referer", "https://y.qq.com/")
                .send()
                .await?;

            let lyric_resp: LyricResponse = match resp.json().await {
                Ok(r) => r,
                Err(_) => continue,
            };

            let lrc_text = lyric_resp.lyric.unwrap_or_default();
            if lrc_text.is_empty() {
                continue;
            }

            if let Ok(mut parsed) = lyrics::lrc::parse(&lrc_text) {
                // Apply translation.
                if let Some(trans_text) = lyric_resp.trans {
                    if let Ok(trans) = lyrics::lrc::parse(&trans_text) {
                        apply_translations(&mut parsed, &trans);
                    }
                }

                let artist_names: Vec<&str> =
                    song.singer.iter().map(|s| s.name.as_str()).collect();

                let quality = calculate_quality(
                    request,
                    &song.songname,
                    &artist_names.join(", "),
                );

                parsed.metadata = LyricsMetadata {
                    title: Some(song.songname),
                    artist: Some(artist_names.join(", ")),
                    album: song.albumname,
                    source: LyricsSource::QQMusic,
                    quality,
                };

                lyrics_list.push(parsed);
            }
        }

        Ok(lyrics_list)
    }
}

fn apply_translations(main: &mut lyrics::Lyrics, trans: &lyrics::Lyrics) {
    for tline in &trans.lines {
        if tline.content.is_empty() {
            continue;
        }
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
    score.min(1.0)
}
