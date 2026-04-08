use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::lyrics::Lyrics;
use super::player::{PlaybackStatus, Track};

/// State snapshot sent to display backends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayState {
    pub track: Option<Track>,
    pub lyrics: Option<Arc<Lyrics>>,
    pub current_line_index: Option<usize>,
    pub next_line_index: Option<usize>,
    pub playback_position: Duration,
    pub status: PlaybackStatus,
    /// Current lyrics offset in milliseconds.
    pub offset_ms: i64,
}

impl Default for DisplayState {
    fn default() -> Self {
        Self {
            track: None,
            lyrics: None,
            current_line_index: None,
            next_line_index: None,
            playback_position: Duration::ZERO,
            status: PlaybackStatus::Stopped,
            offset_ms: 0,
        }
    }
}

/// Commands that can be sent to the scheduler from display backends / API.
#[derive(Debug, Clone)]
pub enum SchedulerCommand {
    /// Re-search lyrics for the current track (ignore cache).
    ResearchCurrent,
    /// Search with custom title/artist override.
    SearchCustom { title: String, artist: String },
    /// Manually set lyrics from raw LRC text.
    SetLyrics { lrc_text: String },
    /// Directly apply a parsed Lyrics object (e.g. from search candidate selection).
    ApplyLyrics { lyrics: Arc<Lyrics> },
    /// Adjust lyrics offset by a delta in milliseconds (positive = lyrics earlier, negative = later).
    AdjustOffset { delta_ms: i64 },
    /// Set lyrics offset to an absolute value in milliseconds.
    SetOffset { offset_ms: i64 },
    /// Toggle play/pause.
    PlayPause,
    /// Start playback.
    Play,
    /// Pause playback.
    Pause,
    /// Stop playback.
    Stop,
    /// Skip to the next track.
    Next,
    /// Skip to the previous track.
    Previous,
    /// Seek to an absolute position in milliseconds.
    SeekTo { position_ms: u64 },
}

/// Trait for display backends (GTK, TUI, HTTP API).
#[async_trait::async_trait]
pub trait DisplayBackend: Send {
    /// Run the display loop, receiving state updates from the watch channel.
    async fn run(
        &mut self,
        state_rx: tokio::sync::watch::Receiver<DisplayState>,
    ) -> anyhow::Result<()>;
}
