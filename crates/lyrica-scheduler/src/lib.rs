use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use lyrica_core::display::{DisplayState, SchedulerCommand};
use lyrica_core::lyrics;
use lyrica_core::player::{PlaybackStatus, PlayerBackend, PlayerEvent};
use lyrica_core::provider::SearchRequest;
use lyrica_cache::LyricsCache;
use lyrica_provider::ProviderGroup;

/// Core scheduler that orchestrates the lyrics lifecycle.
pub struct Scheduler {
    provider: ProviderGroup,
    cache: LyricsCache,
    state_tx: watch::Sender<DisplayState>,
    cmd_rx: mpsc::Receiver<SchedulerCommand>,
    state: DisplayState,
}

impl Scheduler {
    pub fn new(
        provider: ProviderGroup,
        cache: LyricsCache,
        state_tx: watch::Sender<DisplayState>,
        cmd_rx: mpsc::Receiver<SchedulerCommand>,
    ) -> Self {
        Self {
            provider,
            cache,
            state_tx,
            cmd_rx,
            state: DisplayState::default(),
        }
    }

    /// Main event loop: process player events, commands, and update display state.
    pub async fn run(
        &mut self,
        player: &dyn PlayerBackend,
    ) -> Result<()> {
        // Get initial state.
        let initial = player.current_state().await?;
        self.state.status = initial.status;
        self.state.playback_position = initial.position;

        if let Some(track) = initial.track {
            info!(title = %track.title, artist = %track.artist, "Initial track detected");
            self.handle_track_change(&track).await;
            self.state.track = Some(track);
        }

        self.broadcast();

        // Subscribe to player events.
        let mut event_rx = player.subscribe().await?;

        // Line position timer.
        let mut line_timer: Option<tokio::time::Instant> = None;

        // Track real time for accurate position updates.
        let mut last_position_update = tokio::time::Instant::now();

        // Position polling interval (for display updates during playback).
        let mut position_interval = tokio::time::interval(Duration::from_millis(500));

        loop {
            tokio::select! {
                Some(event) = event_rx.recv() => {
                    self.advance_position(&mut last_position_update);
                    match event {
                        PlayerEvent::TrackChanged(track) => {
                            info!(title = %track.title, artist = %track.artist, "Track changed");
                            self.handle_track_change(&track).await;
                            self.state.track = Some(track);
                            self.state.playback_position = Duration::ZERO;
                            last_position_update = tokio::time::Instant::now();
                            self.update_line_index();
                            line_timer = self.schedule_next_line();
                            self.broadcast();
                        }
                        PlayerEvent::PlaybackStateChanged(status) => {
                            info!(status = ?status, "Playback state changed");
                            self.state.status = status;
                            last_position_update = tokio::time::Instant::now();
                            if status == PlaybackStatus::Playing {
                                line_timer = self.schedule_next_line();
                            } else {
                                line_timer = None;
                            }
                            self.broadcast();
                        }
                        PlayerEvent::Seeked(position) => {
                            info!(position_ms = position.as_millis(), "Seeked");
                            self.state.playback_position = position;
                            last_position_update = tokio::time::Instant::now();
                            self.update_line_index();
                            line_timer = self.schedule_next_line();
                            self.broadcast();
                        }
                        PlayerEvent::PlayerQuit => {
                            info!("Player quit");
                            self.state = DisplayState::default();
                            line_timer = None;
                            self.broadcast();
                        }
                    }
                }
                Some(cmd) = self.cmd_rx.recv() => {
                    self.advance_position(&mut last_position_update);
                    self.handle_command(cmd).await;
                    self.update_line_index();
                    line_timer = self.schedule_next_line();
                    self.broadcast();
                }
                _ = async {
                    if let Some(deadline) = line_timer {
                        tokio::time::sleep_until(deadline).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    // Line timer fired: advance to next line.
                    self.advance_position(&mut last_position_update);
                    self.update_line_index();
                    line_timer = self.schedule_next_line();
                    self.broadcast();
                }
                _ = position_interval.tick(), if self.state.status == PlaybackStatus::Playing => {
                    self.advance_position(&mut last_position_update);
                    self.broadcast();
                }
            }
        }
    }

    /// Handle a scheduler command from API / display backend.
    async fn handle_command(&mut self, cmd: SchedulerCommand) {
        match cmd {
            SchedulerCommand::ResearchCurrent => {
                info!("Manual re-search requested");
                if let Some(ref track) = self.state.track {
                    let track = track.clone();
                    self.force_search(&track.title, &track.artist, track.album.as_deref(), track.duration).await;
                } else {
                    warn!("No track to re-search");
                }
            }
            SchedulerCommand::SearchCustom { title, artist } => {
                info!(title = %title, artist = %artist, "Custom search requested");
                self.force_search(&title, &artist, None, None).await;
            }
            SchedulerCommand::SetLyrics { lrc_text } => {
                info!("Manual lyrics set");
                match lyrics::lrc::parse(&lrc_text) {
                    Ok(parsed) => {
                        if let Some(ref track) = self.state.track {
                            let _ = self.cache.put(&track.title, &track.artist, &parsed);
                        }
                        self.state.lyrics = Some(Arc::new(parsed));
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to parse provided LRC text");
                    }
                }
            }
            SchedulerCommand::ApplyLyrics { lyrics } => {
                info!(
                    source = %lyrics.metadata.source,
                    lines = lyrics.lines.len(),
                    "Applying selected lyrics"
                );
                if let Some(ref track) = self.state.track {
                    let _ = self.cache.put(&track.title, &track.artist, &lyrics);
                }
                self.state.lyrics = Some(lyrics);
            }
            SchedulerCommand::AdjustOffset { delta_ms } => {
                if let Some(ref lyrics) = self.state.lyrics {
                    let mut updated = (**lyrics).clone();
                    updated.offset_ms += delta_ms;
                    info!(offset_ms = updated.offset_ms, "Offset adjusted");
                    if let Some(ref track) = self.state.track {
                        let _ = self.cache.put(&track.title, &track.artist, &updated);
                    }
                    self.state.lyrics = Some(Arc::new(updated));
                }
            }
            SchedulerCommand::SetOffset { offset_ms } => {
                if let Some(ref lyrics) = self.state.lyrics {
                    let mut updated = (**lyrics).clone();
                    updated.offset_ms = offset_ms;
                    info!(offset_ms, "Offset set");
                    if let Some(ref track) = self.state.track {
                        let _ = self.cache.put(&track.title, &track.artist, &updated);
                    }
                    self.state.lyrics = Some(Arc::new(updated));
                }
            }
        }
    }

    /// Search providers ignoring cache.
    async fn force_search(&mut self, title: &str, artist: &str, album: Option<&str>, duration: Option<Duration>) {
        self.state.lyrics = None;
        self.state.current_line_index = None;
        self.state.next_line_index = None;

        let request = SearchRequest {
            title: title.to_string(),
            artist: artist.to_string(),
            album: album.map(|s| s.to_string()),
            duration,
        };

        match self.provider.search(&request).await {
            Ok(Some(found)) => {
                // Cache with the current track's identity.
                if let Some(ref track) = self.state.track {
                    let _ = self.cache.put(&track.title, &track.artist, &found);
                }
                info!(
                    source = %found.metadata.source,
                    lines = found.lines.len(),
                    "Lyrics found via manual search"
                );
                self.state.lyrics = Some(Arc::new(found));
            }
            Ok(None) => {
                info!("No lyrics found");
            }
            Err(e) => {
                warn!(error = %e, "Search failed");
            }
        }
    }

    /// Handle a track change: search cache then providers.
    async fn handle_track_change(&mut self, track: &lyrica_core::player::Track) {
        self.state.lyrics = None;
        self.state.current_line_index = None;
        self.state.next_line_index = None;

        // Check cache first.
        if let Some(cached) = self.cache.get(&track.title, &track.artist) {
            info!("Using cached lyrics");
            self.state.lyrics = Some(Arc::new(cached));
            return;
        }

        // Search providers.
        let request = SearchRequest {
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            duration: track.duration,
        };

        match self.provider.search(&request).await {
            Ok(Some(found)) => {
                if let Err(e) = self.cache.put(&track.title, &track.artist, &found) {
                    warn!(error = %e, "Failed to cache lyrics");
                }
                self.state.lyrics = Some(Arc::new(found));
            }
            Ok(None) => {
                info!("No lyrics found");
            }
            Err(e) => {
                warn!(error = %e, "Lyrics search failed");
            }
        }
    }

    /// Advance playback position based on elapsed real time.
    fn advance_position(&mut self, last_update: &mut tokio::time::Instant) {
        if self.state.status == PlaybackStatus::Playing {
            let now = tokio::time::Instant::now();
            let elapsed = now.duration_since(*last_update);
            self.state.playback_position += elapsed;
            *last_update = now;
        }
    }

    fn update_line_index(&mut self) {
        if let Some(ref lyrics) = self.state.lyrics {
            let (current, next) = lyrics.line_at(self.state.playback_position);
            self.state.current_line_index = current;
            self.state.next_line_index = next;
        }
    }

    fn schedule_next_line(&self) -> Option<tokio::time::Instant> {
        if self.state.status != PlaybackStatus::Playing {
            return None;
        }

        if let Some(ref lyrics) = self.state.lyrics {
            if let Some(wait) = lyrics.time_to_next_line(self.state.playback_position) {
                if wait > Duration::ZERO {
                    return Some(tokio::time::Instant::now() + wait);
                }
            }
        }
        None
    }

    fn broadcast(&mut self) {
        self.state.offset_ms = self
            .state
            .lyrics
            .as_ref()
            .map(|l| l.offset_ms)
            .unwrap_or(0);
        let _ = self.state_tx.send(self.state.clone());
    }
}
