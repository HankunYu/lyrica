use std::fs;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use lyrica_core::config::Config;
use lyrica_core::display::DisplayState;
use lyrica_provider::{WeightsHandle, weights_from_config};

#[derive(Parser)]
#[command(name = "lyrica", about = "Desktop lyrics display for Linux")]
struct Cli {
    #[command(subcommand)]
    mode: Option<DisplayMode>,

    /// HTTP API server port (0 = disabled).
    #[arg(long, default_value = "8080", global = true)]
    api_port: u16,
}

#[derive(Subcommand, PartialEq)]
enum DisplayMode {
    /// Terminal UI mode (default).
    Tui,
    /// Headless mode (API server only).
    Headless,
}

/// Watch the config file and hot-reload provider weights on change.
/// Other config fields (player, api_port, ...) are not hot-reloadable;
/// changes to those are logged and require a restart.
fn spawn_config_watcher(weights: WeightsHandle) {
    let config_path = Config::default_path();
    let watch_dir = match config_path.parent() {
        Some(p) => p.to_path_buf(),
        None => {
            warn!("Config has no parent directory; hot reload disabled");
            return;
        }
    };

    if !watch_dir.exists() {
        info!(path = %watch_dir.display(), "Config dir missing; hot reload disabled");
        return;
    }

    // Channel from the OS watcher (sync) into our async debouncer.
    let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<()>();
    let target = config_path.clone();

    let mut watcher = match notify::recommended_watcher(move |res: notify::Result<Event>| {
        let Ok(event) = res else { return };
        let relevant = matches!(
            event.kind,
            EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
        );
        if !relevant {
            return;
        }
        // Editors often write via a sibling temp file + rename; coalesce by
        // checking that any event in the directory touched our file (or is
        // the rename target). We just trigger on anything and let the
        // reload re-read the file.
        if event.paths.iter().any(|p| p == &target) || event.paths.is_empty() {
            let _ = raw_tx.send(());
        }
    }) {
        Ok(w) => w,
        Err(e) => {
            warn!(error = %e, "Failed to start config watcher; hot reload disabled");
            return;
        }
    };

    if let Err(e) = watcher.watch(&watch_dir, RecursiveMode::NonRecursive) {
        warn!(path = %watch_dir.display(), error = %e, "Failed to watch config dir");
        return;
    }

    info!(path = %config_path.display(), "Hot-reloading provider weights on change");

    tokio::spawn(async move {
        // Move the watcher into the task so its lifetime matches the loop.
        let _watcher = watcher;
        // Track the previously-applied weights so we only log on real changes.
        let mut last_weights = weights.read().map(|g| g.clone()).unwrap_or_default();

        while raw_rx.recv().await.is_some() {
            // Debounce: drain any further events that arrive within ~150 ms.
            tokio::time::sleep(Duration::from_millis(150)).await;
            while raw_rx.try_recv().is_ok() {}

            let new_config = Config::load();
            let new_weights = weights_from_config(&new_config);

            if new_weights != last_weights {
                if let Ok(mut guard) = weights.write() {
                    *guard = new_weights.clone();
                }
                let mut diffs: Vec<String> = new_weights
                    .iter()
                    .filter(|(k, v)| last_weights.get(*k) != Some(*v))
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect();
                diffs.sort();
                info!(changes = ?diffs, "Reloaded provider weights");
                last_weights = new_weights;
            }
        }
    });
}

fn log_path() -> std::path::PathBuf {
    let dir = if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        std::path::PathBuf::from(xdg)
    } else if let Ok(home) = std::env::var("HOME") {
        std::path::PathBuf::from(home).join(".cache")
    } else {
        std::path::PathBuf::from("/tmp")
    };
    dir.join("lyrica").join("lyrica.log")
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mode = cli.mode.unwrap_or(DisplayMode::Tui);

    // TUI mode: log to file to avoid polluting the terminal.
    // Headless mode: log to stderr as usual.
    if mode == DisplayMode::Tui {
        let path = log_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = fs::File::create(&path)?;
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .with_writer(file)
            .with_ansi(false)
            .init();
        // Print log location before entering TUI.
        eprintln!("Logs: {}", path.display());
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .init();
    }

    info!("Lyrica starting...");

    let config = Config::load();

    // Create the shared state channel.
    let (state_tx, state_rx) = watch::channel(DisplayState::default());

    // Create the command channel for API → scheduler communication.
    let (cmd_tx, cmd_rx) = mpsc::channel(32);

    // Initialize components. All ProviderGroups share one weights handle so
    // hot-reload updates propagate to scheduler / API / TUI at once.
    let player = lyrica_player::MprisPlayer::new(&config.player, config.strict_player).await?;
    let provider = lyrica_provider::ProviderGroup::with_config(&config);
    let weights_handle = provider.weights_handle();
    let cache = lyrica_cache::LyricsCache::new(None)?;
    let mut scheduler = lyrica_scheduler::Scheduler::new(provider, cache, state_tx, cmd_rx);

    spawn_config_watcher(weights_handle.clone());

    // Start the API server if requested.
    if cli.api_port > 0 {
        let api_rx = state_rx.clone();
        let api_cmd_tx = cmd_tx.clone();
        let api_provider = lyrica_provider::ProviderGroup::with_shared_weights(&config, weights_handle.clone());
        let port = cli.api_port;
        tokio::spawn(async move {
            let mut server = lyrica_server::ApiServer::new(port, api_cmd_tx, api_provider);
            use lyrica_core::display::DisplayBackend;
            if let Err(e) = server.run(api_rx).await {
                tracing::error!(error = %e, "API server error");
            }
        });
    }

    // Start display backend and scheduler concurrently.
    match mode {
        DisplayMode::Tui => {
            let tui_rx = state_rx.clone();
            let tui_cmd_tx = cmd_tx.clone();
            let tui_provider = lyrica_provider::ProviderGroup::with_shared_weights(&config, weights_handle.clone());
            let tui_handle = tokio::spawn(async move {
                let mut tui = lyrica_display_tui::TuiDisplay::new(tui_cmd_tx, tui_provider);
                use lyrica_core::display::DisplayBackend;
                tui.run(tui_rx).await
            });

            let scheduler_handle = tokio::spawn(async move {
                scheduler.run(&player).await
            });

            tokio::select! {
                result = tui_handle => {
                    info!("TUI exited");
                    result??;
                }
                result = scheduler_handle => {
                    info!("Scheduler exited");
                    result??;
                }
            }
        }
        DisplayMode::Headless => {
            info!("Running in headless mode");
            if cli.api_port == 0 {
                tracing::warn!("Headless mode without API port is not very useful. Use --api-port <PORT>.");
            }
            scheduler.run(&player).await?;
        }
    }

    Ok(())
}
