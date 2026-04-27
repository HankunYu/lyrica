use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use lyrica_core::player::{
    PlaybackStatus, PlayerBackend, PlayerEvent, PlayerState, Track,
};

/// How often to probe music apps for state changes.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Position drift over POLL_INTERVAL beyond which we treat the change as a Seek.
/// Polling jitter alone shouldn't exceed ~250ms.
const SEEK_THRESHOLD: Duration = Duration::from_millis(800);

/// macOS player backend driven by AppleScript probes against music apps
/// (Spotify in phase 2; Music.app added in phase 4). Polling-based; events
/// are derived by diffing successive snapshots.
pub struct MacOsPlayer {
    preferred_player: String,
    strict: bool,
    /// Last snapshot observed by the polling loop, used by `position()` for
    /// interpolation between polls.
    interp: Arc<Mutex<Interp>>,
}

#[derive(Clone, Copy)]
struct Interp {
    status: PlaybackStatus,
    last_position: Duration,
    /// Wall time when `last_position` was sampled.
    sampled_at: Instant,
}

impl Default for Interp {
    fn default() -> Self {
        Self {
            status: PlaybackStatus::Stopped,
            last_position: Duration::ZERO,
            sampled_at: Instant::now(),
        }
    }
}

/// One snapshot of a music app's state.
#[derive(Clone, Debug)]
struct Snapshot {
    source: Source,
    status: PlaybackStatus,
    position: Duration,
    track: Track,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Spotify,
    Music,
}

impl Source {
    fn display_name(self) -> &'static str {
        match self {
            Source::Spotify => "Spotify",
            Source::Music => "Music",
        }
    }

    /// Process name to look up via System Events when checking liveness.
    /// (Same as display_name today; kept separate so renames don't surprise.)
    fn process_name(self) -> &'static str {
        match self {
            Source::Spotify => "Spotify",
            Source::Music => "Music",
        }
    }

    fn matches_preferred(self, preferred: &str) -> bool {
        self.display_name()
            .to_lowercase()
            .contains(&preferred.to_lowercase())
    }
}

impl MacOsPlayer {
    pub async fn new(preferred_player: &str, strict: bool) -> Result<Self> {
        Ok(Self {
            preferred_player: preferred_player.to_string(),
            strict,
            interp: Arc::new(Mutex::new(Interp::default())),
        })
    }
}

#[async_trait::async_trait]
impl PlayerBackend for MacOsPlayer {
    async fn subscribe(&self) -> Result<mpsc::Receiver<PlayerEvent>> {
        let (tx, rx) = mpsc::channel(64);
        let preferred = self.preferred_player.clone();
        let strict = self.strict;
        let interp = self.interp.clone();

        tokio::spawn(async move {
            run_event_loop(preferred, strict, interp, tx).await;
        });

        Ok(rx)
    }

    async fn current_state(&self) -> Result<PlayerState> {
        match probe_current(&self.preferred_player, self.strict).await {
            Some(snap) => Ok(PlayerState {
                status: snap.status,
                position: snap.position,
                track: Some(snap.track),
                player_name: snap.source.display_name().to_string(),
            }),
            None => Ok(PlayerState {
                status: PlaybackStatus::Stopped,
                position: Duration::ZERO,
                track: None,
                player_name: String::new(),
            }),
        }
    }

    async fn position(&self) -> Result<Duration> {
        let interp = *self.interp.lock().expect("interp lock poisoned");
        if interp.status == PlaybackStatus::Playing {
            Ok(interp.last_position + interp.sampled_at.elapsed())
        } else {
            Ok(interp.last_position)
        }
    }

    async fn play_pause(&self) -> Result<()> {
        let src = self.active_source().await?;
        run_osascript(&command_script(src, "playpause")).await?;
        Ok(())
    }

    async fn play(&self) -> Result<()> {
        let src = self.active_source().await?;
        run_osascript(&command_script(src, "play")).await?;
        Ok(())
    }

    async fn pause(&self) -> Result<()> {
        let src = self.active_source().await?;
        run_osascript(&command_script(src, "pause")).await?;
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        let src = self.active_source().await?;
        // Spotify lacks a `stop` verb; degrade to `pause`.
        let verb = match src {
            Source::Spotify => "pause",
            Source::Music => "stop",
        };
        run_osascript(&command_script(src, verb)).await?;
        Ok(())
    }

    async fn next(&self) -> Result<()> {
        let src = self.active_source().await?;
        run_osascript(&command_script(src, "next track")).await?;
        Ok(())
    }

    async fn previous(&self) -> Result<()> {
        let src = self.active_source().await?;
        run_osascript(&command_script(src, "previous track")).await?;
        Ok(())
    }

    async fn seek_to(&self, position: Duration) -> Result<()> {
        let src = self.active_source().await?;
        let secs = position.as_secs_f64();
        let script = format!(
            r#"tell application "{}" to set player position to {}"#,
            src.display_name(),
            secs
        );
        run_osascript(&script).await?;
        Ok(())
    }
}

impl MacOsPlayer {
    /// Pick the source that any control should currently target. Reuses the
    /// same selection logic as the polling loop so the user is always
    /// controlling whichever player is showing in the TUI.
    async fn active_source(&self) -> Result<Source> {
        match probe_current(&self.preferred_player, self.strict).await {
            Some(s) => Ok(s.source),
            None => anyhow::bail!("no active macOS player to control"),
        }
    }
}

/// Build a one-liner like: tell application "Spotify" to playpause
fn command_script(src: Source, verb: &str) -> String {
    format!(r#"tell application "{}" to {}"#, src.display_name(), verb)
}

/// Outer loop: poll music apps every POLL_INTERVAL, derive events from
/// snapshot diffs, recover from transient failures.
async fn run_event_loop(
    preferred: String,
    strict: bool,
    interp: Arc<Mutex<Interp>>,
    tx: mpsc::Sender<PlayerEvent>,
) {
    // First snapshot seeds `last` without emitting — the scheduler already
    // received the equivalent state via current_state() before subscribing.
    // Re-emitting TrackChanged here would cause the scheduler to reset
    // playback_position to 0 (TrackChanged semantics = "new track from 0").
    let mut last: Option<Snapshot> = None;
    let mut seeded = false;

    let mut interval = tokio::time::interval(POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        let current = probe_current(&preferred, strict).await;

        // Update position interpolation cache regardless of event emission.
        if let Some(ref snap) = current {
            let mut g = interp.lock().expect("interp lock poisoned");
            g.status = snap.status;
            g.last_position = snap.position;
            g.sampled_at = Instant::now();
        }

        if seeded {
            emit_diff_events(&last, &current, &tx).await;
        } else if current.is_some() {
            seeded = true;
        }
        last = current;

        if tx.is_closed() {
            return;
        }
    }
}

/// Compare last vs current and emit events for what changed.
async fn emit_diff_events(
    last: &Option<Snapshot>,
    current: &Option<Snapshot>,
    tx: &mpsc::Sender<PlayerEvent>,
) {
    match (last, current) {
        (Some(prev), Some(curr)) => {
            // Source switched -> treat as a fresh track + state.
            if prev.source != curr.source {
                info!(from = prev.source.display_name(), to = curr.source.display_name(),
                      "macOS source switched");
                let _ = tx.send(PlayerEvent::TrackChanged(curr.track.clone())).await;
                let _ = tx.send(PlayerEvent::PlaybackStateChanged(curr.status)).await;
                return;
            }

            if prev.track.id != curr.track.id {
                info!(title = %curr.track.title, artist = %curr.track.artist,
                      "Track changed");
                let _ = tx.send(PlayerEvent::TrackChanged(curr.track.clone())).await;
            }

            if prev.status != curr.status {
                let _ = tx.send(PlayerEvent::PlaybackStateChanged(curr.status)).await;
            }

            // Seek detection: predict where position should be assuming uniform
            // playback since the last sample, then compare with the actual.
            let elapsed = POLL_INTERVAL;
            let predicted = if prev.status == PlaybackStatus::Playing {
                prev.position + elapsed
            } else {
                prev.position
            };
            let drift = curr.position.abs_diff(predicted);
            // Only flag a Seek when the track id matches — track changes already
            // reset position naturally and shouldn't masquerade as a seek.
            if prev.track.id == curr.track.id && drift > SEEK_THRESHOLD {
                debug!(position_ms = curr.position.as_millis(), drift_ms = drift.as_millis(),
                       "Seek detected");
                let _ = tx.send(PlayerEvent::Seeked(curr.position)).await;
            }
        }
        (None, Some(curr)) => {
            info!(player = curr.source.display_name(), "Connected to macOS player");
            let _ = tx.send(PlayerEvent::TrackChanged(curr.track.clone())).await;
            let _ = tx.send(PlayerEvent::PlaybackStateChanged(curr.status)).await;
        }
        (Some(prev), None) => {
            info!(player = prev.source.display_name(), "macOS player gone");
            let _ = tx.send(PlayerEvent::PlayerQuit).await;
        }
        (None, None) => {}
    }
}

/// Probe enabled sources and pick the best snapshot according to:
///   1. preferred-substring match wins;
///   2. else any source with state=Playing;
///   3. else any non-stopped source;
///   4. else None.
///
/// In strict mode with a non-empty `preferred`, only that source is allowed.
async fn probe_current(preferred: &str, strict: bool) -> Option<Snapshot> {
    let candidates = enabled_sources(preferred, strict);

    // Probe in parallel; AppleScript subprocess fork is the dominant cost.
    let probes = futures_join_all(candidates.into_iter().map(|src| async move {
        match probe_source(src).await {
            Ok(opt) => opt,
            Err(e) => {
                warn!(source = src.display_name(), error = %e, "macOS source probe failed");
                None
            }
        }
    }))
    .await;

    let snaps: Vec<Snapshot> = probes.into_iter().flatten().collect();

    if !preferred.is_empty() {
        if let Some(s) = snaps.iter().find(|s| s.source.matches_preferred(preferred)) {
            return Some(s.clone());
        }
        if strict {
            return None;
        }
    }

    if let Some(s) = snaps.iter().find(|s| s.status == PlaybackStatus::Playing) {
        return Some(s.clone());
    }
    snaps.into_iter().next()
}

fn enabled_sources(preferred: &str, strict: bool) -> Vec<Source> {
    let all = [Source::Spotify, Source::Music];
    if strict && !preferred.is_empty() {
        return all
            .into_iter()
            .filter(|s| s.matches_preferred(preferred))
            .collect();
    }
    all.to_vec()
}

/// Tiny stand-in for `futures::future::join_all` so we don't have to add the
/// `futures` crate dependency on macOS just for this.
async fn futures_join_all<F: std::future::Future>(iter: impl IntoIterator<Item = F>) -> Vec<F::Output> {
    let mut out = Vec::new();
    let handles: Vec<_> = iter.into_iter().collect();
    for f in handles {
        out.push(f.await);
    }
    out
}

async fn probe_source(src: Source) -> Result<Option<Snapshot>> {
    // Skip osascript entirely when the app process isn't running. AppleScript
    // resolves `tell application "X"` references at script-compile time, and
    // Launch Services makes that resolution take 10-20s on macOS Sequoia+ when
    // the app isn't open — even when an early `return` would otherwise skip
    // that branch. `pgrep` is ~30 ms.
    if !is_process_running(src.process_name()).await {
        return Ok(None);
    }
    let raw = run_osascript(&probe_script(src)).await?;
    Ok(parse_probe_output(src, &raw))
}

async fn is_process_running(name: &str) -> bool {
    match Command::new("pgrep").arg("-xq").arg(name).output().await {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

/// Build the AppleScript probe for one source.
///
/// Output schema (one line):
///   "not_running"
///   "stopped"
///   "ok|<state>|<title>|<artist>|<album>|<duration_ms>|<position_s>|<id>"
///
/// Notes:
/// - Spotify exposes `duration` in milliseconds; Music.app exposes it in
///   seconds. Each source's script normalizes the value to milliseconds so
///   the Rust parser stays uniform.
/// - The `tell System Events` guard prevents AppleScript from auto-launching
///   the app just because we asked about it.
fn probe_script(src: Source) -> String {
    let app = src.display_name();
    let proc = src.process_name();
    let dur_expr = match src {
        // Spotify: duration is integer milliseconds.
        Source::Spotify => "duration of t",
        // Music.app: duration is real seconds; convert to ms.
        Source::Music => "(duration of t) * 1000",
    };
    let id_expr = match src {
        // Spotify: stable "spotify:track:..." URI.
        Source::Spotify => "id of t",
        // Music.app: persistent ID is stable across restarts (unlike database id).
        Source::Music => "persistent ID of t",
    };
    format!(
        r#"tell application "System Events"
    if not (exists process "{proc}") then return "not_running"
end tell
tell application "{app}"
    try
        set ps to player state as text
    on error
        return "stopped"
    end try
    if ps is "stopped" then return "stopped"
    try
        set t to current track
    on error
        return "stopped"
    end try
    set dur to {dur_expr}
    set pos to player position
    return "ok|" & ps & "|" & (name of t) & "|" & (artist of t) & "|" & (album of t) & "|" & dur & "|" & pos & "|" & ({id_expr})
end tell"#
    )
}

fn parse_probe_output(src: Source, raw: &str) -> Option<Snapshot> {
    let line = raw.trim();
    if line == "not_running" || line == "stopped" || line.is_empty() {
        return None;
    }

    let mut parts = line.splitn(8, '|');
    let head = parts.next().unwrap_or("");
    if head != "ok" {
        debug!(source = src.display_name(), raw = %line, "Unexpected probe output");
        return None;
    }
    let state = parts.next().unwrap_or("");
    let title = parts.next().unwrap_or("").to_string();
    let artist = parts.next().unwrap_or("").to_string();
    let album = parts.next().unwrap_or("").to_string();
    let duration_ms_str = parts.next().unwrap_or("0");
    let position_s_str = parts.next().unwrap_or("0");
    let id = parts.next().unwrap_or("").to_string();

    let duration_ms: f64 = duration_ms_str.parse().unwrap_or(0.0);
    let position_s: f64 = position_s_str.parse().unwrap_or(0.0);

    let status = match state {
        "playing" => PlaybackStatus::Playing,
        "paused" => PlaybackStatus::Paused,
        _ => PlaybackStatus::Stopped,
    };

    let duration = if duration_ms > 0.0 {
        Some(Duration::from_millis(duration_ms.max(0.0) as u64))
    } else {
        None
    };
    let position = Duration::from_micros((position_s.max(0.0) * 1_000_000.0) as u64);

    if title.is_empty() && artist.is_empty() {
        return None;
    }

    let album_opt = if album.is_empty() { None } else { Some(album) };

    Some(Snapshot {
        source: src,
        status,
        position,
        track: Track {
            id,
            title,
            artist,
            album: album_opt,
            duration,
        },
    })
}

async fn run_osascript(script: &str) -> Result<String> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("osascript failed: {}", stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
