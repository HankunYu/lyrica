# Lyrica

A Linux desktop lyrics display tool. This is a Linux variant of [LyricsX](https://github.com/MxIris-LyricsX-Project/LyricsX), the popular macOS/iOS lyrics app.

Automatically detects the currently playing song via MPRIS, searches lyrics from multiple sources, and displays them in sync with playback.

## Features

- **MPRIS Integration** — Works with any Linux music player (Spotify, VLC, mpv, Firefox, Chrome, etc.)
- **Multi-source Lyrics Search** — NetEase Cloud Music, QQ Music, Kugou, LRCLIB
- **Parallel Search with Priority Window** — Searches all sources concurrently, picks the best match
- **LRC / LRCX Format** — Supports standard LRC and extended LRCX (word-level timestamps, translations)
- **TUI Mode** — Terminal-based synchronized lyrics display with search and offset adjustment
- **HTTP API** — REST endpoints for external devices and plugins to consume lyrics
- **Lyrics Cache** — Caches results locally to avoid repeated searches
- **Seek Detection** — Follows progress bar changes in real time

## Installation

### Build from source

```bash
git clone https://github.com/HankunYu/lyrica.git
cd lyrica
cargo install --path .
```

Or build manually:

```bash
cargo build --release
# Binary at target/release/lyrica
```

### Dependencies

- Rust 1.85+ (edition 2024)
- D-Bus (for MPRIS, should already be present on any Linux desktop)
- OpenSSL

## Usage

### TUI Mode (default)

```bash
# Basic TUI display
lyrica

# With custom API port
lyrica tui --api-port 8080
```

#### Keybindings

| Key | Action |
|-----|--------|
| `q` | Quit |
| `s` / `/` | Search lyrics |
| `r` | Re-search current track |
| `+` / `=` | Offset +100ms |
| `-` | Offset -100ms |
| `]` | Offset +500ms |
| `[` | Offset -500ms |
| `0` | Reset offset |

In search results: `j`/`k` or `↑`/`↓` to navigate, `Enter` to select, `Esc` to cancel.

### Headless Mode (API only)

```bash
lyrica headless --api-port 8080
```

### Environment

```bash
# Enable debug logging
RUST_LOG=debug lyrica

# Verbose logging for a specific module
RUST_LOG=lyrica_provider=debug lyrica
```

## HTTP API

Start the API server with `--api-port <PORT>` (default: 8080). Provides REST endpoints for status queries, lyrics management, and playback control.

See [Docs/API.md](Docs/API.md) for full endpoint documentation.

## Architecture

```
                                    watch channel
  ┌─────────────┐  PlayerEvent  ┌──────────────┐  DisplayState  ┌────────────────┐
  │ MPRIS Player│ ────────────> │  Scheduler   │ ─────────────> │ TUI (ratatui)  │
  │   (zbus)    │               │              │                │ HTTP API (axum)│
  └─────────────┘               └──────┬───────┘                └────────────────┘
                                       │
                                       v
                                ┌──────────────┐
                                │ ProviderGroup│
                                │  - LRCLIB    │
                                │  - NetEase   │
                                │  - QQ Music  │
                                │  - Kugou     │
                                └──────┬───────┘
                                       │
                                       v
                                ┌──────────────┐
                                │    Cache     │
                                │ ~/.cache/    │
                                │   lyrica/    │
                                └──────────────┘
```

### Crate Structure

| Crate | Purpose |
|---|---|
| `lyrica-core` | Core types, traits, LRC/LRCX parser |
| `lyrica-player` | MPRIS D-Bus player integration |
| `lyrica-provider` | Lyrics source providers (NetEase, QQ, Kugou, LRCLIB) |
| `lyrica-cache` | File-system lyrics cache |
| `lyrica-scheduler` | Core event loop and line position scheduling |
| `lyrica-display-tui` | Terminal UI with ratatui |
| `lyrica-server` | HTTP API server with axum |

## Lyrics Sources

| Source | Auth | Notes |
|---|---|---|
| LRCLIB | No | Open lyrics database, good for English songs |
| NetEase Cloud Music | No | Best coverage for Chinese songs, includes translations |
| QQ Music | No | Good Chinese song coverage |
| Kugou | No | Good coverage, supports translations |

## Roadmap

- [x] MPRIS player detection and seek tracking
- [x] Multi-source parallel lyrics search
- [x] LRC / LRCX parsing (with translations and word-level timestamps)
- [x] TUI synchronized display with search and offset adjustment
- [x] HTTP REST API
- [x] Lyrics caching
- [ ] GTK4 transparent desktop overlay with karaoke effect
- [ ] WebSocket real-time push
- [ ] TOML configuration file
- [ ] systemd user service

## Acknowledgements

Inspired by [LyricsX](https://github.com/MxIris-LyricsX-Project/LyricsX) for macOS/iOS.

## License

MIT
