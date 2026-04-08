# HTTP API

Start the API server with `--api-port <PORT>` (default: 8080).

```bash
# TUI mode with API
lyrica tui --api-port 8080

# Headless mode (API only)
lyrica headless --api-port 8080
```

## Status

### `GET /api/status`

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

## Lyrics

### `GET /api/lyrics`

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

### `GET /api/lyrics/current`

Current line only, with progress within the line (0.0 - 1.0).

```json
{
  "content": "Current lyrics line",
  "translation": "translated line",
  "position_ms": 45000,
  "progress": 0.65
}
```

### `POST /api/lyrics/search`

Search for lyrics by title and artist. Omit fields to use the current track info.

```json
{ "title": "Song Name", "artist": "Artist" }
```

### `POST /api/lyrics/select`

Select a candidate from search results by index.

```json
{ "index": 0 }
```

### `POST /api/lyrics/set`

Set lyrics from raw LRC text.

```json
{ "lrc": "[00:12.00]First line\n[00:15.00]Second line" }
```

### `POST /api/lyrics/offset`

Adjust or set lyrics time offset.

```json
{ "adjust": 100 }
```

```json
{ "set": 0 }
```

## Playback Control

Control the music player via MPRIS. All control endpoints return:

```json
{ "ok": true, "message": "PlayPause sent" }
```

### `POST /api/player/play-pause`

Toggle play/pause. No request body needed.

### `POST /api/player/play`

Start playback. No request body needed.

### `POST /api/player/pause`

Pause playback. No request body needed.

### `POST /api/player/stop`

Stop playback. No request body needed.

### `POST /api/player/next`

Skip to the next track. No request body needed.

### `POST /api/player/previous`

Skip to the previous track. No request body needed.

### `POST /api/player/seek`

Seek to an absolute position.

```json
{ "position_ms": 30000 }
```
