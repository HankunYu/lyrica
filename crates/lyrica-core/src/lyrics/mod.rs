pub mod lrc;
pub mod lrcx;

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// A single word with its timing info (for karaoke-style display).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WordTimestamp {
    /// Offset from the start of the line.
    pub offset: Duration,
    /// Duration of this word.
    pub duration: Duration,
    /// The word text.
    pub word: String,
}

/// A single lyrics line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LyricsLine {
    /// Start time of this line.
    pub position: Duration,
    /// Lyrics text content.
    pub content: String,
    /// Optional translation.
    pub translation: Option<String>,
    /// Word-level timestamps for karaoke effect (LRCX).
    pub word_timestamps: Option<Vec<WordTimestamp>>,
}

/// Source provider name for lyrics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LyricsSource {
    NetEase,
    QQMusic,
    Kugou,
    Lrclib,
    Local,
    Unknown,
}

impl std::fmt::Display for LyricsSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NetEase => write!(f, "NetEase"),
            Self::QQMusic => write!(f, "QQ Music"),
            Self::Kugou => write!(f, "Kugou"),
            Self::Lrclib => write!(f, "LRCLIB"),
            Self::Local => write!(f, "Local"),
            Self::Unknown => write!(f, "Unknown"),
        }
    }
}

impl LyricsSource {
    /// Stable lowercase key matching `LyricsProvider::key()`, used to look up
    /// per-provider weights in `Config`. `None` for sources that have no
    /// associated provider (Local / Unknown).
    pub fn config_key(&self) -> Option<&'static str> {
        match self {
            Self::Lrclib => Some("lrclib"),
            Self::NetEase => Some("netease"),
            Self::QQMusic => Some("qqmusic"),
            Self::Kugou => Some("kugou"),
            Self::Local | Self::Unknown => None,
        }
    }
}

/// Metadata associated with lyrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LyricsMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub source: LyricsSource,
    /// Match quality score 0.0 - 1.0.
    pub quality: f32,
}

/// A complete set of lyrics for a song.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lyrics {
    pub lines: Vec<LyricsLine>,
    pub metadata: LyricsMetadata,
    /// User-adjusted offset in milliseconds.
    pub offset_ms: i64,
}

impl Lyrics {
    /// Given a playback time, return (current_line_index, next_line_index).
    /// Applies the user offset before searching.
    pub fn line_at(&self, time: Duration) -> (Option<usize>, Option<usize>) {
        let adjusted = if self.offset_ms >= 0 {
            time.saturating_add(Duration::from_millis(self.offset_ms as u64))
        } else {
            time.saturating_sub(Duration::from_millis((-self.offset_ms) as u64))
        };

        if self.lines.is_empty() {
            return (None, None);
        }

        // Binary search for the last line whose position <= adjusted time.
        let idx = self
            .lines
            .partition_point(|line| line.position <= adjusted);

        if idx == 0 {
            // Before the first line.
            (None, Some(0))
        } else {
            let current = idx - 1;
            let next = if idx < self.lines.len() {
                Some(idx)
            } else {
                None
            };
            (Some(current), next)
        }
    }

    /// Get the duration until the next line change from the given time.
    pub fn time_to_next_line(&self, time: Duration) -> Option<Duration> {
        let (_, next) = self.line_at(time);
        next.map(|idx| {
            let next_pos = self.lines[idx].position;
            let adjusted = if self.offset_ms >= 0 {
                time.saturating_add(Duration::from_millis(self.offset_ms as u64))
            } else {
                time.saturating_sub(Duration::from_millis((-self.offset_ms) as u64))
            };
            next_pos.saturating_sub(adjusted)
        })
    }
}
