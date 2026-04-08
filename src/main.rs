use std::fs;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tokio::sync::{mpsc, watch};
use tracing::info;

use lyrica_core::display::DisplayState;

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

    // Create the shared state channel.
    let (state_tx, state_rx) = watch::channel(DisplayState::default());

    // Create the command channel for API → scheduler communication.
    let (cmd_tx, cmd_rx) = mpsc::channel(32);

    // Initialize components.
    let player = lyrica_player::MprisPlayer::new("").await?;
    let provider = lyrica_provider::ProviderGroup::with_all_providers();
    let cache = lyrica_cache::LyricsCache::new(None)?;
    let mut scheduler = lyrica_scheduler::Scheduler::new(provider, cache, state_tx, cmd_rx);

    // Start the API server if requested.
    if cli.api_port > 0 {
        let api_rx = state_rx.clone();
        let api_cmd_tx = cmd_tx.clone();
        let api_provider = lyrica_provider::ProviderGroup::with_all_providers();
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
            let tui_provider = lyrica_provider::ProviderGroup::with_all_providers();
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
