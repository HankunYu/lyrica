use std::sync::Arc;

use anyhow::Result;
use axum::extract::State;
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch, Mutex};
use tracing::info;

use lyrica_core::display::{DisplayBackend, DisplayState, SchedulerCommand};
use lyrica_core::lyrics::Lyrics;
use lyrica_core::player::PlaybackStatus;
use lyrica_core::provider::SearchRequest;
use lyrica_provider::ProviderGroup;

/// HTTP API server for external lyrics consumers.
pub struct ApiServer {
    port: u16,
    cmd_tx: mpsc::Sender<SchedulerCommand>,
    provider: Arc<ProviderGroup>,
}

impl ApiServer {
    pub fn new(port: u16, cmd_tx: mpsc::Sender<SchedulerCommand>, provider: ProviderGroup) -> Self {
        Self {
            port,
            cmd_tx,
            provider: Arc::new(provider),
        }
    }
}

#[async_trait::async_trait]
impl DisplayBackend for ApiServer {
    async fn run(&mut self, state_rx: watch::Receiver<DisplayState>) -> Result<()> {
        let app_state = Arc::new(AppState {
            state_rx,
            cmd_tx: self.cmd_tx.clone(),
            provider: self.provider.clone(),
            last_search_results: Mutex::new(Vec::new()),
        });

        let app = Router::new()
            .route("/api/status", get(get_status))
            .route("/api/lyrics", get(get_lyrics))
            .route("/api/lyrics/current", get(get_current_line))
            .route("/api/lyrics/search", post(post_search))
            .route("/api/lyrics/select", post(post_select))
            .route("/api/lyrics/set", post(post_set_lyrics))
            .route("/api/lyrics/offset", post(post_offset))
            .with_state(app_state);

        let addr = format!("0.0.0.0:{}", self.port);
        info!(addr = %addr, "Starting HTTP API server");

        let listener = tokio::net::TcpListener::bind(&addr).await?;
        axum::serve(listener, app).await?;
        Ok(())
    }
}

struct AppState {
    state_rx: watch::Receiver<DisplayState>,
    cmd_tx: mpsc::Sender<SchedulerCommand>,
    provider: Arc<ProviderGroup>,
    /// Cached search results so user can select by index.
    last_search_results: Mutex<Vec<Lyrics>>,
}

// --- Response types ---

#[derive(Serialize)]
struct StatusResponse {
    track_title: Option<String>,
    track_artist: Option<String>,
    track_album: Option<String>,
    playback_status: String,
    position_ms: u64,
    has_lyrics: bool,
}

#[derive(Serialize)]
struct LyricsResponse {
    lines: Vec<LyricLineResponse>,
    current_line_index: Option<usize>,
}

#[derive(Serialize)]
struct LyricLineResponse {
    position_ms: u64,
    content: String,
    translation: Option<String>,
}

#[derive(Serialize)]
struct CurrentLineResponse {
    content: Option<String>,
    translation: Option<String>,
    position_ms: u64,
    progress: f32,
}

#[derive(Serialize)]
struct CommandResponse {
    ok: bool,
    message: String,
}

#[derive(Serialize)]
struct SearchResultItem {
    index: usize,
    source: String,
    title: Option<String>,
    artist: Option<String>,
    quality: f32,
    line_count: usize,
    /// First few lines as preview.
    preview: Vec<String>,
}

#[derive(Serialize)]
struct SearchResponse {
    ok: bool,
    count: usize,
    results: Vec<SearchResultItem>,
}

// --- Request types ---

#[derive(Deserialize)]
struct SearchBody {
    title: Option<String>,
    artist: Option<String>,
}

#[derive(Deserialize)]
struct SelectBody {
    /// Index from search results (0-based).
    index: usize,
}

#[derive(Deserialize)]
struct SetLyricsBody {
    lrc: String,
}

#[derive(Deserialize)]
struct OffsetBody {
    /// Absolute offset in ms (used if provided).
    set: Option<i64>,
    /// Delta adjustment in ms (used if `set` is not provided).
    adjust: Option<i64>,
}

// --- GET handlers ---

async fn get_status(State(state): State<Arc<AppState>>) -> Json<StatusResponse> {
    let s = state.state_rx.borrow().clone();
    Json(StatusResponse {
        track_title: s.track.as_ref().map(|t| t.title.clone()),
        track_artist: s.track.as_ref().map(|t| t.artist.clone()),
        track_album: s.track.as_ref().and_then(|t| t.album.clone()),
        playback_status: match s.status {
            PlaybackStatus::Playing => "playing".to_string(),
            PlaybackStatus::Paused => "paused".to_string(),
            PlaybackStatus::Stopped => "stopped".to_string(),
        },
        position_ms: s.playback_position.as_millis() as u64,
        has_lyrics: s.lyrics.is_some(),
    })
}

async fn get_lyrics(State(state): State<Arc<AppState>>) -> Json<LyricsResponse> {
    let s = state.state_rx.borrow().clone();
    let lines = s
        .lyrics
        .as_ref()
        .map(|l| {
            l.lines
                .iter()
                .map(|line| LyricLineResponse {
                    position_ms: line.position.as_millis() as u64,
                    content: line.content.clone(),
                    translation: line.translation.clone(),
                })
                .collect()
        })
        .unwrap_or_default();

    Json(LyricsResponse {
        lines,
        current_line_index: s.current_line_index,
    })
}

async fn get_current_line(State(state): State<Arc<AppState>>) -> Json<CurrentLineResponse> {
    let s = state.state_rx.borrow().clone();

    let (content, translation, progress) = if let (Some(lyrics), Some(idx)) =
        (&s.lyrics, s.current_line_index)
    {
        let line = &lyrics.lines[idx];
        let line_progress = if let Some(next_idx) = s.next_line_index {
            let next_pos = lyrics.lines[next_idx].position;
            let line_duration = next_pos.saturating_sub(line.position);
            let elapsed = s.playback_position.saturating_sub(line.position);
            if line_duration.as_millis() > 0 {
                (elapsed.as_millis() as f32) / (line_duration.as_millis() as f32)
            } else {
                0.0
            }
        } else {
            0.0
        };
        (
            Some(line.content.clone()),
            line.translation.clone(),
            line_progress.min(1.0),
        )
    } else {
        (None, None, 0.0)
    };

    Json(CurrentLineResponse {
        content,
        translation,
        position_ms: s.playback_position.as_millis() as u64,
        progress,
    })
}

// --- POST handlers ---

/// Search for lyrics candidates. Returns a list the user can choose from.
async fn post_search(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SearchBody>,
) -> Json<SearchResponse> {
    // Determine search terms.
    let (title, artist) = {
        let s = state.state_rx.borrow();
        let t = body
            .title
            .filter(|s| !s.is_empty())
            .or_else(|| s.track.as_ref().map(|t| t.title.clone()))
            .unwrap_or_default();
        let a = body
            .artist
            .filter(|s| !s.is_empty())
            .or_else(|| s.track.as_ref().map(|t| t.artist.clone()))
            .unwrap_or_default();
        (t, a)
    };

    if title.is_empty() {
        return Json(SearchResponse {
            ok: false,
            count: 0,
            results: vec![],
        });
    }

    let request = SearchRequest {
        title,
        artist,
        album: None,
        duration: None,
    };

    let candidates = state.provider.search_all(&request).await.unwrap_or_default();

    let results: Vec<SearchResultItem> = candidates
        .iter()
        .enumerate()
        .map(|(i, lyrics)| {
            let preview: Vec<String> = lyrics
                .lines
                .iter()
                .filter(|l| !l.content.is_empty())
                .take(3)
                .map(|l| l.content.clone())
                .collect();

            SearchResultItem {
                index: i,
                source: lyrics.metadata.source.to_string(),
                title: lyrics.metadata.title.clone(),
                artist: lyrics.metadata.artist.clone(),
                quality: lyrics.metadata.quality,
                line_count: lyrics.lines.len(),
                preview,
            }
        })
        .collect();

    let count = results.len();

    // Store candidates for later selection.
    *state.last_search_results.lock().await = candidates;

    Json(SearchResponse {
        ok: true,
        count,
        results,
    })
}

/// Select a lyrics candidate by index from the last search results.
async fn post_select(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SelectBody>,
) -> Json<CommandResponse> {
    let results = state.last_search_results.lock().await;

    if body.index >= results.len() {
        return Json(CommandResponse {
            ok: false,
            message: format!(
                "Index {} out of range, {} candidates available",
                body.index,
                results.len()
            ),
        });
    }

    let selected = results[body.index].clone();
    drop(results); // Release lock before sending.

    let cmd = SchedulerCommand::ApplyLyrics {
        lyrics: Arc::new(selected),
    };

    match state.cmd_tx.send(cmd).await {
        Ok(()) => Json(CommandResponse {
            ok: true,
            message: format!("Applied lyrics candidate #{}", body.index),
        }),
        Err(e) => Json(CommandResponse {
            ok: false,
            message: format!("Failed: {}", e),
        }),
    }
}

async fn post_set_lyrics(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SetLyricsBody>,
) -> Json<CommandResponse> {
    if body.lrc.trim().is_empty() {
        return Json(CommandResponse {
            ok: false,
            message: "LRC text is empty".to_string(),
        });
    }

    let cmd = SchedulerCommand::SetLyrics { lrc_text: body.lrc };
    match state.cmd_tx.send(cmd).await {
        Ok(()) => Json(CommandResponse {
            ok: true,
            message: "Lyrics set".to_string(),
        }),
        Err(e) => Json(CommandResponse {
            ok: false,
            message: format!("Failed: {}", e),
        }),
    }
}

/// Adjust or set lyrics time offset.
async fn post_offset(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OffsetBody>,
) -> Json<CommandResponse> {
    let cmd = if let Some(abs) = body.set {
        SchedulerCommand::SetOffset { offset_ms: abs }
    } else if let Some(delta) = body.adjust {
        SchedulerCommand::AdjustOffset { delta_ms: delta }
    } else {
        return Json(CommandResponse {
            ok: false,
            message: "Provide 'set' or 'adjust' field".to_string(),
        });
    };

    match state.cmd_tx.send(cmd).await {
        Ok(()) => Json(CommandResponse {
            ok: true,
            message: "Offset updated".to_string(),
        }),
        Err(e) => Json(CommandResponse {
            ok: false,
            message: format!("Failed: {}", e),
        }),
    }
}
