# Lyrica

A Linux desktop lyrics display tool. This is a Linux variant of [LyricsX](https://github.com/MxIris-LyricsX-Project/LyricsX), the popular macOS/iOS lyrics app.

Automatically detects the currently playing song via MPRIS, searches lyrics from multiple sources, and displays them in sync with playback.

## Features

- **MPRIS Integration** — Works with any Linux music player (Spotify, VLC, mpv, Firefox, Chrome, etc.)
- **Multi-source Lyrics Search** — NetEase Cloud Music, QQ Music, Kugou, LRCLIB
- **Parallel Search with Priority Window** — Searches all sources concurrently, picks the best match
- **Configurable Provider Weights** — Bias the ranking toward your preferred source (or disable a source) via TOML config
- **Non-blocking Search** — Lyrics search runs in the background; the timeline stays in sync with the music regardless of how long the search takes
- **Hot-reloadable Config** — Edit `config.toml` to change provider weights without restarting
- **Player Pinning** — Optionally lock onto a single MPRIS source (e.g. only Spotify) and ignore others
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

## Configuration

Lyrica reads `~/.config/lyrica/config.toml` (or `$XDG_CONFIG_HOME/lyrica/config.toml`) at startup. The file is optional — defaults are used if missing or unparseable.

### Example

```toml
# Preferred MPRIS player. Substring match (case-insensitive) against
# `org.mpris.MediaPlayer2.<name>`. Leave empty for auto-pick.
# List active players with: `busctl --user list | grep mpris`
player = "spotify"

# When true, only attach to the preferred player above. If it isn't running,
# lyrica waits instead of falling back to another MPRIS source.
strict_player = true

# Per-provider weight applied multiplicatively to each candidate's quality
# score during ranking. Default is 1.0. Set weight = 0 to disable a provider
# entirely (no request is sent).
[providers.netease]
weight = 2.0

[providers.lrclib]
weight = 1.0

[providers.qqmusic]
weight = 1.0

[providers.kugou]
weight = 0
```

### Hot reload

Provider weights are watched via `inotify` and re-applied on the **next** search without restarting lyrica. Other fields (`player`, `strict_player`, `api_port`, ...) require a restart.

### Field reference

| Field | Default | Hot reload | Notes |
|---|---|---|---|
| `player` | `""` | No | Preferred MPRIS player (substring match). Empty = auto-pick. |
| `strict_player` | `false` | No | If true, never fall back when `player` not present. |
| `search_timeout_secs` | `10` | No | Max wall time for a single search across all providers. |
| `priority_window_secs` | `2` | No | After the first non-empty result, keep collecting for this long. |
| `api_port` | `0` | No | Currently overridden by the `--api-port` CLI flag. |
| `cache_dir` | `""` | No | Defaults to `~/.cache/lyrica` (currently not user-overridable). |
| `providers.<key>.weight` | `1.0` | **Yes** | `<key>` ∈ `lrclib` / `netease` / `qqmusic` / `kugou`. |

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
- [x] TOML configuration file with hot-reloadable provider weights
- [ ] GTK4 transparent desktop overlay with karaoke effect
- [ ] WebSocket real-time push
- [ ] systemd user service

## Acknowledgements

Inspired by [LyricsX](https://github.com/MxIris-LyricsX-Project/LyricsX) for macOS/iOS.

## License

MIT
