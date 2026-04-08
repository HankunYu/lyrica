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

Start the API server with `--api-port <PORT>` (default: 8080).

### Endpoints

#### `GET /api/status`

Current player and track status.

```json
{
  "track_title": "Song Name",
  "track_artist": "Artist",
  "track_album": "Album",
  "playback_status": "playing",
  "position_ms": 45000,
  "has_lyrics": true
}
```

#### `GET /api/lyrics`

Full lyrics with current line index.

```json
{
  "lines": [
    { "position_ms": 12000, "content": "First line", "translation": "translated" },
    { "position_ms": 15000, "content": "Second line", "translation": null }
  ],
  "current_line_index": 3
}
```

#### `GET /api/lyrics/current`

Current line only, with progress within the line (0.0 - 1.0).

```json
{
  "content": "Current lyrics line",
  "translation": "translated line",
  "position_ms": 45000,
  "progress": 0.65
}
```

#### `POST /api/lyrics/search`

Search for lyrics by title and artist.

```json
{ "title": "Song Name", "artist": "Artist" }
```

#### `POST /api/lyrics/select`

Select a candidate from search results by index.

```json
{ "index": 0 }
```

#### `POST /api/lyrics/set`

Set lyrics from raw LRC text.

```json
{ "lrc": "[00:12.00]First line\n[00:15.00]Second line" }
```

#### `POST /api/lyrics/offset`

Adjust or set lyrics time offset.

```json
{ "adjust_ms": 100 }
```

```json
{ "set_ms": 0 }
```

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
