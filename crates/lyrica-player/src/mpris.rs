use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use zbus::Connection;
use zbus::zvariant::OwnedValue;

use lyrica_core::player::{
    PlaybackStatus, PlayerBackend, PlayerEvent, PlayerState, Track,
};

/// MPRIS2 D-Bus player backend with automatic player discovery and reconnection.
pub struct MprisPlayer {
    connection: Connection,
    preferred_player: String,
    current_player: Option<String>,
    last_known_position: Duration,
    last_position_time: Instant,
    current_status: PlaybackStatus,
}

impl MprisPlayer {
    pub async fn new(preferred_player: &str) -> Result<Self> {
        let connection = Connection::session()
            .await
            .context("Failed to connect to D-Bus session bus")?;

        let mut player = Self {
            connection,
            preferred_player: preferred_player.to_string(),
            current_player: None,
            last_known_position: Duration::ZERO,
            last_position_time: Instant::now(),
            current_status: PlaybackStatus::Stopped,
        };

        player.discover_player().await?;
        Ok(player)
    }

    async fn discover_player(&mut self) -> Result<()> {
        let proxy = zbus::fdo::DBusProxy::new(&self.connection).await?;
        let names = proxy.list_names().await?;

        let mpris_names: Vec<String> = names
            .iter()
            .filter(|n| n.as_str().starts_with("org.mpris.MediaPlayer2."))
            .map(|n| n.to_string())
            .collect();

        if mpris_names.is_empty() {
            info!("No MPRIS players found, will wait for one to appear...");
            self.current_player = None;
            return Ok(());
        }

        let selected = if !self.preferred_player.is_empty() {
            mpris_names
                .iter()
                .find(|n| n.to_lowercase().contains(&self.preferred_player.to_lowercase()))
                .cloned()
        } else {
            None
        }
        .unwrap_or_else(|| mpris_names[0].clone());

        info!(player = %selected, "Selected MPRIS player");
        self.current_player = Some(selected);
        Ok(())
    }

    async fn make_proxy(&self) -> Result<zbus::Proxy<'_>> {
        let bus_name = self
            .current_player
            .as_deref()
            .context("No player selected")?;

        let proxy = zbus::Proxy::new(
            &self.connection,
            bus_name,
            "/org/mpris/MediaPlayer2",
            "org.mpris.MediaPlayer2.Player",
        )
        .await?;

        Ok(proxy)
    }
}

#[async_trait::async_trait]
impl PlayerBackend for MprisPlayer {
    async fn subscribe(&self) -> Result<mpsc::Receiver<PlayerEvent>> {
        let (tx, rx) = mpsc::channel(64);
        let connection = self.connection.clone();
        let preferred = self.preferred_player.clone();

        tokio::spawn(async move {
            run_resilient_loop(connection, preferred, tx).await;
        });

        Ok(rx)
    }

    async fn current_state(&self) -> Result<PlayerState> {
        if self.current_player.is_none() {
            return Ok(PlayerState {
                status: PlaybackStatus::Stopped,
                position: Duration::ZERO,
                track: None,
                player_name: String::new(),
            });
        }

        let proxy = self.make_proxy().await?;
        let track = read_track(&proxy).await?;
        let status = read_status(&proxy).await;
        let position = read_position(&proxy).await;

        let player_name = self
            .current_player
            .as_deref()
            .unwrap_or("")
            .trim_start_matches("org.mpris.MediaPlayer2.")
            .to_string();

        Ok(PlayerState {
            status,
            position,
            track,
            player_name,
        })
    }

    async fn position(&self) -> Result<Duration> {
        if self.current_status == PlaybackStatus::Playing {
            let elapsed = self.last_position_time.elapsed();
            Ok(self.last_known_position + elapsed)
        } else {
            Ok(self.last_known_position)
        }
    }
}

/// Find an MPRIS player on the bus. Returns None if no player found.
async fn find_player(connection: &Connection, preferred: &str) -> Option<String> {
    let proxy = zbus::fdo::DBusProxy::new(connection).await.ok()?;
    let names = proxy.list_names().await.ok()?;

    let mpris_names: Vec<String> = names
        .iter()
        .filter(|n| n.as_str().starts_with("org.mpris.MediaPlayer2."))
        .map(|n| n.to_string())
        .collect();

    if mpris_names.is_empty() {
        return None;
    }

    if !preferred.is_empty() {
        if let Some(found) = mpris_names
            .iter()
            .find(|n| n.to_lowercase().contains(&preferred.to_lowercase()))
        {
            return Some(found.clone());
        }
    }

    Some(mpris_names[0].clone())
}

/// Outer loop: keeps trying to find a player, monitor it, and reconnect when it disappears.
async fn run_resilient_loop(
    connection: Connection,
    preferred: String,
    tx: mpsc::Sender<PlayerEvent>,
) {
    loop {
        // Wait until we find a player.
        let bus_name = loop {
            if let Some(name) = find_player(&connection, &preferred).await {
                break name;
            }
            // No player found, wait and retry.
            tokio::time::sleep(Duration::from_secs(2)).await;
        };

        info!(player = %bus_name, "Connected to MPRIS player");

        // Monitor this player until it disconnects or errors.
        match run_event_loop(&connection, &bus_name, &tx).await {
            Ok(()) => {
                info!(player = %bus_name, "Player disconnected");
            }
            Err(e) => {
                warn!(player = %bus_name, error = %e, "Player monitoring error");
            }
        }

        // Notify scheduler that the player is gone.
        let _ = tx.send(PlayerEvent::PlayerQuit).await;

        // Brief pause before searching for a new player.
        info!("Waiting for a new MPRIS player...");
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Monitor a single player. Returns Ok(()) when the player disappears, Err on fatal error.
async fn run_event_loop(
    connection: &Connection,
    bus_name: &str,
    tx: &mpsc::Sender<PlayerEvent>,
) -> Result<()> {
    let proxy = zbus::Proxy::new(
        connection,
        bus_name,
        "/org/mpris/MediaPlayer2",
        "org.mpris.MediaPlayer2.Player",
    )
    .await?;

    let mut signal_stream = proxy.receive_all_signals().await?;

    let mut last_track_id = String::new();
    let mut last_position = read_position(&proxy).await;
    let mut consecutive_errors = 0u32;

    // Read initial state and emit events.
    if let Ok(Some(track)) = read_track(&proxy).await {
        last_track_id = track.id.clone();
        let _ = tx.send(PlayerEvent::TrackChanged(track)).await;
    }
    let mut last_status = read_status(&proxy).await;
    let _ = tx.send(PlayerEvent::PlaybackStateChanged(last_status)).await;

    let mut interval = tokio::time::interval(Duration::from_secs(1));

    loop {
        tokio::select! {
            signal = signal_stream.next() => {
                if signal.is_none() {
                    return Ok(());
                }
                let msg = signal.unwrap();
                let is_seeked = msg.header().member().is_some_and(|m| m.as_str() == "Seeked");

                if is_seeked {
                    // Handle Seeked signal directly.
                    let position = read_position(&proxy).await;
                    debug!(position_ms = position.as_millis(), "Seeked signal received");
                    last_position = position;
                    let _ = tx.send(PlayerEvent::Seeked(position)).await;
                }

                match check_state_changes(&proxy, tx, &mut last_track_id, &mut last_status).await {
                    Ok(()) => { consecutive_errors = 0; }
                    Err(_) => {
                        consecutive_errors += 1;
                        if consecutive_errors >= 5 {
                            return Ok(());
                        }
                    }
                }
            }
            _ = interval.tick() => {
                // Detect seek by position jump (fallback for players that don't emit Seeked signal).
                let position = read_position(&proxy).await;
                let diff = if position > last_position {
                    position - last_position
                } else {
                    last_position - position
                };
                // If position jumped more than 2 seconds from expected, treat as seek.
                if diff > Duration::from_secs(2) {
                    debug!(position_ms = position.as_millis(), "Seek detected via polling");
                    let _ = tx.send(PlayerEvent::Seeked(position)).await;
                }
                last_position = position;

                match check_state_changes(&proxy, tx, &mut last_track_id, &mut last_status).await {
                    Ok(()) => { consecutive_errors = 0; }
                    Err(_) => {
                        consecutive_errors += 1;
                        if consecutive_errors >= 5 {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

async fn check_state_changes(
    proxy: &zbus::Proxy<'_>,
    tx: &mpsc::Sender<PlayerEvent>,
    last_track_id: &mut String,
    last_status: &mut PlaybackStatus,
) -> Result<()> {
    // Check track change.
    match read_track(proxy).await {
        Ok(Some(track)) => {
            if track.id != *last_track_id && !track.id.is_empty() {
                *last_track_id = track.id.clone();
                info!(title = %track.title, artist = %track.artist, "Track changed");
                let _ = tx.send(PlayerEvent::TrackChanged(track)).await;
            }
        }
        Ok(None) => {}
        Err(e) => return Err(e),
    }

    // Check playback status change.
    let status = read_status(proxy).await;
    if status != *last_status {
        *last_status = status;
        let _ = tx.send(PlayerEvent::PlaybackStateChanged(status)).await;
    }

    Ok(())
}

async fn read_track(proxy: &zbus::Proxy<'_>) -> Result<Option<Track>> {
    let metadata: HashMap<String, OwnedValue> = match proxy.get_property("Metadata").await {
        Ok(m) => m,
        Err(_) => return Ok(None),
    };

    if metadata.is_empty() {
        return Ok(None);
    }

    let title = get_metadata_string(&metadata, "xesam:title").unwrap_or_default();
    let artist = get_metadata_string_list(&metadata, "xesam:artist")
        .map(|a| a.join(", "))
        .unwrap_or_default();
    let album = get_metadata_string(&metadata, "xesam:album");
    let track_id = get_metadata_string(&metadata, "mpris:trackid").unwrap_or_default();

    let duration = metadata
        .get("mpris:length")
        .and_then(|v| {
            let us: i64 = TryInto::<i64>::try_into(v.clone()).ok().or_else(|| {
                let u: Option<u64> = v.clone().try_into().ok();
                u.map(|u| u as i64)
            })?;
            Some(Duration::from_micros(us.max(0) as u64))
        });

    if title.is_empty() && artist.is_empty() {
        return Ok(None);
    }

    Ok(Some(Track {
        id: track_id,
        title,
        artist,
        album,
        duration,
    }))
}

fn get_metadata_string(metadata: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let value = metadata.get(key)?.clone();
    let s: Result<String, _> = value.try_into();
    s.ok()
}

fn get_metadata_string_list(metadata: &HashMap<String, OwnedValue>, key: &str) -> Option<Vec<String>> {
    let value = metadata.get(key)?;
    let list: Result<Vec<String>, _> = value.clone().try_into();
    if let Ok(l) = list {
        return Some(l);
    }
    let s: Result<String, _> = value.clone().try_into();
    s.ok().map(|s| vec![s])
}

async fn read_status(proxy: &zbus::Proxy<'_>) -> PlaybackStatus {
    let status_str: String = proxy
        .get_property("PlaybackStatus")
        .await
        .unwrap_or_else(|_| "Stopped".to_string());

    match status_str.as_str() {
        "Playing" => PlaybackStatus::Playing,
        "Paused" => PlaybackStatus::Paused,
        _ => PlaybackStatus::Stopped,
    }
}

async fn read_position(proxy: &zbus::Proxy<'_>) -> Duration {
    let pos_us: i64 = proxy
        .get_property("Position")
        .await
        .unwrap_or(0);
    Duration::from_micros(pos_us.max(0) as u64)
}
