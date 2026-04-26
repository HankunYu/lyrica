use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use lyrica_core::display::{DisplayState, SchedulerCommand};
use lyrica_core::lyrics::{self, Lyrics};
use lyrica_core::player::{PlaybackStatus, PlayerBackend, PlayerEvent};
use lyrica_core::provider::SearchRequest;
use lyrica_cache::LyricsCache;
use lyrica_provider::ProviderGroup;

/// Result of a background lyrics search, tagged with the track it was for so
/// the main loop can drop stale results from rapid track switches.
struct LyricsSearchResult {
    track_id: String,
    title: String,
    artist: String,
    result: Result<Option<Lyrics>>,
}

/// Core scheduler that orchestrates the lyrics lifecycle.
pub struct Scheduler {
    provider: Arc<ProviderGroup>,
    cache: LyricsCache,
    state_tx: watch::Sender<DisplayState>,
    cmd_rx: mpsc::Receiver<SchedulerCommand>,
    state: DisplayState,
    /// Sender side handed to background search tasks.
    search_tx: mpsc::Sender<LyricsSearchResult>,
    /// Receiver side polled by the main loop.
    search_rx: mpsc::Receiver<LyricsSearchResult>,
}

impl Scheduler {
    pub fn new(
        provider: ProviderGroup,
        cache: LyricsCache,
        state_tx: watch::Sender<DisplayState>,
        cmd_rx: mpsc::Receiver<SchedulerCommand>,
    ) -> Self {
        let (search_tx, search_rx) = mpsc::channel(8);
        Self {
            provider: Arc::new(provider),
            cache,
            state_tx,
            cmd_rx,
            state: DisplayState::default(),
            search_tx,
            search_rx,
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
            self.state.track = Some(track.clone());
            self.handle_track_change(&track);
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
                            self.state.track = Some(track.clone());
                            // New track starts at 0 from the player's perspective;
                            // a Seeked event will correct this if the player reports otherwise.
                            self.state.playback_position = Duration::ZERO;
                            last_position_update = tokio::time::Instant::now();
                            // Search runs in the background; the result arrives via search_rx
                            // and is applied without blocking this loop.
                            self.handle_track_change(&track);
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
                    match &cmd {
                        SchedulerCommand::PlayPause => {
                            if let Err(e) = player.play_pause().await {
                                warn!(error = %e, "PlayPause failed");
                            }
                        }
                        SchedulerCommand::Play => {
                            if let Err(e) = player.play().await {
                                warn!(error = %e, "Play failed");
                            }
                        }
                        SchedulerCommand::Pause => {
                            if let Err(e) = player.pause().await {
                                warn!(error = %e, "Pause failed");
                            }
                        }
                        SchedulerCommand::Stop => {
                            if let Err(e) = player.stop().await {
                                warn!(error = %e, "Stop failed");
                            }
                        }
                        SchedulerCommand::Next => {
                            if let Err(e) = player.next().await {
                                warn!(error = %e, "Next failed");
                            }
                        }
                        SchedulerCommand::Previous => {
                            if let Err(e) = player.previous().await {
                                warn!(error = %e, "Previous failed");
                            }
                        }
                        SchedulerCommand::SeekTo { position_ms } => {
                            let pos = Duration::from_millis(*position_ms);
                            if let Err(e) = player.seek_to(pos).await {
                                warn!(error = %e, "SeekTo failed");
                            }
                        }
                        _ => {
                            self.handle_command(&cmd);
                        }
                    }
                    self.update_line_index();
                    line_timer = self.schedule_next_line();
                    self.broadcast();
                }
                Some(found) = self.search_rx.recv() => {
                    self.advance_position(&mut last_position_update);
                    self.apply_search_result(found);
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

    /// Handle a scheduler command from API / display backend. Synchronous;
    /// long-running searches dispatched here are spawned in the background.
    fn handle_command(&mut self, cmd: &SchedulerCommand) {
        match cmd {
            SchedulerCommand::ResearchCurrent => {
                info!("Manual re-search requested");
                if let Some(ref track) = self.state.track {
                    let track = track.clone();
                    self.spawn_search(
                        &track.id,
                        &track.title,
                        &track.artist,
                        track.album.as_deref(),
                        track.duration,
                    );
                } else {
                    warn!("No track to re-search");
                }
            }
            SchedulerCommand::SearchCustom { title, artist } => {
                info!(title = %title, artist = %artist, "Custom search requested");
                let track_id = self
                    .state
                    .track
                    .as_ref()
                    .map(|t| t.id.clone())
                    .unwrap_or_default();
                self.spawn_search(&track_id, title, artist, None, None);
            }
            SchedulerCommand::SetLyrics { lrc_text } => {
                info!("Manual lyrics set");
                match lyrics::lrc::parse(lrc_text) {
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
                    let _ = self.cache.put(&track.title, &track.artist, lyrics);
                }
                self.state.lyrics = Some(lyrics.clone());
            }
            SchedulerCommand::AdjustOffset { delta_ms } => {
                if let Some(ref lyrics) = self.state.lyrics {
                    let mut updated = (**lyrics).clone();
                    updated.offset_ms += *delta_ms;
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
                    updated.offset_ms = *offset_ms;
                    info!(offset_ms, "Offset set");
                    if let Some(ref track) = self.state.track {
                        let _ = self.cache.put(&track.title, &track.artist, &updated);
                    }
                    self.state.lyrics = Some(Arc::new(updated));
                }
            }
            // Playback control commands are handled in the main loop
            // where we have access to the player reference.
            SchedulerCommand::PlayPause
            | SchedulerCommand::Play
            | SchedulerCommand::Pause
            | SchedulerCommand::Stop
            | SchedulerCommand::Next
            | SchedulerCommand::Previous
            | SchedulerCommand::SeekTo { .. } => {
                // Handled in run() directly.
            }
        }
    }

    /// Handle a track change: synchronous cache check, then spawn background
    /// search if needed. Never blocks the main loop on the network.
    fn handle_track_change(&mut self, track: &lyrica_core::player::Track) {
        self.state.lyrics = None;
        self.state.current_line_index = None;
        self.state.next_line_index = None;

        if let Some(cached) = self.cache.get(&track.title, &track.artist) {
            info!("Using cached lyrics");
            self.state.lyrics = Some(Arc::new(cached));
            return;
        }

        self.spawn_search(
            &track.id,
            &track.title,
            &track.artist,
            track.album.as_deref(),
            track.duration,
        );
    }

    /// Spawn a background lyrics search; result is delivered via search_tx.
    fn spawn_search(
        &self,
        track_id: &str,
        title: &str,
        artist: &str,
        album: Option<&str>,
        duration: Option<Duration>,
    ) {
        let provider = self.provider.clone();
        let tx = self.search_tx.clone();
        let track_id = track_id.to_string();
        let title = title.to_string();
        let artist = artist.to_string();
        let album = album.map(|s| s.to_string());

        tokio::spawn(async move {
            let request = SearchRequest {
                title: title.clone(),
                artist: artist.clone(),
                album,
                duration,
            };
            let result = provider.search(&request).await;
            let _ = tx
                .send(LyricsSearchResult {
                    track_id,
                    title,
                    artist,
                    result,
                })
                .await;
        });
    }

    /// Apply a background search result iff it still matches the current track.
    fn apply_search_result(&mut self, found: LyricsSearchResult) {
        // Drop stale results from rapid track switches. Match on track id when
        // available, otherwise fall back to title+artist.
        let stale = match self.state.track.as_ref() {
            Some(current) => {
                if !found.track_id.is_empty() && !current.id.is_empty() {
                    found.track_id != current.id
                } else {
                    found.title != current.title || found.artist != current.artist
                }
            }
            None => true,
        };
        if stale {
            info!(
                track = %found.title,
                "Discarding stale search result (track changed)"
            );
            return;
        }

        match found.result {
            Ok(Some(lyrics)) => {
                if let Err(e) = self.cache.put(&found.title, &found.artist, &lyrics) {
                    warn!(error = %e, "Failed to cache lyrics");
                }
                info!(
                    source = %lyrics.metadata.source,
                    lines = lyrics.lines.len(),
                    "Applying lyrics from background search"
                );
                self.state.lyrics = Some(Arc::new(lyrics));
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
