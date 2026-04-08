use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Information about the currently playing track.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Track {
    /// Unique identifier (e.g. MPRIS trackid).
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration: Option<Duration>,
}

/// Playback status of the music player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

/// Full snapshot of the player's current state.
#[derive(Debug, Clone)]
pub struct PlayerState {
    pub status: PlaybackStatus,
    pub position: Duration,
    pub track: Option<Track>,
    /// Name of the player application (e.g. "Spotify", "Firefox").
    pub player_name: String,
}

/// Events emitted by a player backend.
#[derive(Debug, Clone)]
pub enum PlayerEvent {
    TrackChanged(Track),
    PlaybackStateChanged(PlaybackStatus),
    /// Emitted when the user seeks to a new position.
    Seeked(Duration),
    /// Player application was closed.
    PlayerQuit,
}

/// Trait for player backends to implement.
#[async_trait::async_trait]
pub trait PlayerBackend: Send + Sync {
    /// Start listening for events. Returns a receiver for player events.
    async fn subscribe(&self) -> anyhow::Result<tokio::sync::mpsc::Receiver<PlayerEvent>>;

    /// Get the current player state snapshot.
    async fn current_state(&self) -> anyhow::Result<PlayerState>;

    /// Get the estimated current playback position (interpolated).
    async fn position(&self) -> anyhow::Result<Duration>;
}
