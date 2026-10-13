# yt-tui

**YouTube Music in the terminal** — a polished media-player TUI written in Rust.

Search, play, queue, library, radio, synced lyrics, album-art-driven accents, an FFT visualizer, and macOS Now Playing / media keys. Sign-in is zero-friction: press Enter, log in in your browser, done.

## Install

### From source

```bash
cargo install --path .
```

Requires a recent Rust toolchain (`rustup`).

### Dependencies

On first run, **yt-tui downloads what it needs**:

- `yt-dlp` (stream extraction)
- `deno` (JS runtime for yt-dlp), if `node` / `bun` / `deno` are not already on your `PATH`

No YouTube API key. No manual cookie copying in the happy path.

## Quick start

```bash
yt-tui
```

1. Press **Enter** to sign in (opens [music.youtube.com](https://music.youtube.com)), or **G** for guest mode.
2. Press **/** to search, **Enter** to play.
3. Press **?** for the full keyboard reference.

CLI helpers:

```bash
yt-tui login     # browser sign-in only
yt-tui logout
yt-tui doctor    # print environment diagnostics
yt-tui --debug   # verbose file logging
```

## Features

| Area | What you get |
|------|----------------|
| Playback | Play / pause / seek / volume, shuffle, repeat, queue reorder |
| Discovery | Search (songs / albums / artists / playlists), home feed, radio autoplay |
| Library | Liked songs, playlists, albums, artists, history (after sign-in) |
| Look | Media-player layout, dynamic accent from album art, spectrum visualizer |
| Lyrics | Synced lyrics via [LRCLIB](https://lrclib.net), YouTube Music fallback |
| System | macOS media keys & Control Center Now Playing; session restore |

## Keyboard (essentials)

| Key | Action |
|-----|--------|
| `space` | Play / pause |
| `n` / `p` | Next / previous |
| `←` / `→` | Navigate Home shelves / Library sections (seek ±5s elsewhere) |
| `,` / `.` | Seek ±5s (`Shift+←/→` ±30s) |
| `/` | Search |
| `1`–`5` | Home / Search / Library / Queue / Lyrics |
| `Tab` / `f` | Full-screen Now Playing |
| `L` | Account / sign in |
| `Ctrl+P` | Command palette |
| `?` | Help |
| `q` | Quit |

## Sign-in details

1. **Browser (recommended)** — yt-tui opens YouTube Music and reads session cookies from Firefox, Chrome, Brave, Edge, Arc, Vivaldi, Opera, or Safari via [`rookie`](https://crates.io/crates/rookie).
2. On macOS, Chromium browsers may show a **Keychain** prompt — click **Always Allow**.
3. **Guest** — search and play without an account (no library / likes).
4. **Paste cookie** — last resort: paste a `Cookie` header or “Copy as cURL” from DevTools.

Credentials are stored in the OS keychain (`yt-tui` / `cookies`), with a `0600` file fallback under the app data directory.

## Config

Written on first run to the platform config dir, e.g.:

- macOS: `~/Library/Preferences/com.yt-tui.yt-tui/config.toml`
- Linux: `~/.config/yt-tui/config.toml`

```toml
[ui]
theme = "dynamic"          # or "static"
accent = "#FF0033"
visualizer = "bars"        # bars | mirror | wave | off
show_hints = true

[playback]
autoplay = true
volume = 0.8
cache_size_mb = 1024
prefetch = true
```

Logs: `…/Caches/…/logs/yt-tui.log` (never written to the terminal).

## Terminal tips

- Prefer a terminal with **truecolor** and a graphics protocol (Kitty, iTerm2, WezTerm, Ghostty) for best album art.
- Inside **tmux**, art may fall back to half-block characters unless passthrough is configured.
- Minimum comfortable size: **80×24**.

## Development

```bash
cargo build
cargo test
cargo run -- doctor
```

Project layout follows the architecture in the implementation plan: Elm-style `App` + async services (API, player, art, lyrics, auth).

## Distribution

Release binaries can be produced with [`cargo-dist`](https://opensource.axo.dev/cargo-dist/):

```bash
cargo install cargo-dist
dist init    # once
dist build
```

A Homebrew tap formula can point at those release assets:

```ruby
# Formula/yt-tui.rb (sketch)
class YtTui < Formula
  desc "YouTube Music in the terminal"
  homepage "https://github.com/francoisegner/yt-tui"
  url "https://github.com/francoisegner/yt-tui/releases/download/v0.1.0/yt-tui-aarch64-apple-darwin.tar.gz"
  sha256 "…"
  def install
    bin.install "yt-tui"
  end
end
```

## Manual QA checklist

- [ ] First run with no yt-dlp / deno on PATH — auto-download succeeds
- [ ] Guest mode: search + play
- [ ] Browser login (Firefox and Chrome); Keychain prompt handled
- [ ] Library + like after sign-in
- [ ] Seek, skip, shuffle, repeat, queue reorder
- [ ] Session restore (paused at last position)
- [ ] Lyrics panel with synced highlight
- [ ] Visualizer responds while playing
- [ ] Media keys / Now Playing on macOS
- [ ] Resize down to 80×24; Kitty / iTerm / Terminal.app
- [ ] `yt-tui doctor` prints sensible diagnostics

## Disclaimer

This project is **not** affiliated with, endorsed by, or supported by Google or YouTube. Use at your own risk and respect YouTube’s Terms of Service.

## License

MIT
