# yt-tui

**YouTube Music in the terminal** — a polished media-player TUI written in Rust.

Search, play, queue, library, radio, synced lyrics, album-art-driven accents, an FFT visualizer, and macOS Now Playing / media keys. Sign in once by pasting a cURL request; yt-tui then keeps the session alive on its own.

## Install

### From source

```bash
cargo install --path .
```

Requires a recent Rust toolchain (`rustup`).

### Dependencies

No external programs are required for playback — stream URLs are resolved natively via YouTube’s Innertube API (no `yt-dlp`, Node, Deno, or Bun).

No YouTube API key. No manual cookie copying in the happy path.

**Known limitations:** age-restricted and some region-locked tracks may not play (the stream client is unauthenticated). If YouTube changes mobile player clients, playback may need updated client constants in the resolver.

## Quick start

```bash
yt-tui
```

1. Press **Enter** to sign in (opens [music.youtube.com](https://music.youtube.com)), or **G** for guest mode.
2. Press **/** to search, **Enter** to play.
3. Press **?** for the full keyboard reference.

CLI helpers:

```bash
yt-tui login     # prints sign-in instructions
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
| `e` | Equalizer |
| `L` | Account / sign in |
| `Ctrl+P` | Command palette |
| `?` | Help |
| `q` | Quit |

## Sign-in details

1. **Paste once** — open a *private* window, sign in at music.youtube.com, copy any request as cURL (DevTools → Network) and paste it in yt-tui (`L` or `P`). Then close the private window.
2. yt-tui owns that session from then on: it rotates the short-lived cookies in the background the way a browser tab does, so you stay signed in. No browser access or Full Disk Access is needed.
3. **Guest** — search and play without an account (no library / likes).

The session is stored in a `0600` file (`auth.json`) under the app data directory. The OS keychain is not used, so there are no password prompts.

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
eq_enabled = true
eq_gains = [0.0, 0.0, 0.0, 0.0, 0.0]  # Bass … Treble, ±12 dB
```

Press **`e`** for the equalizer (presets include Bass Boost).

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

- [ ] Cold play works with no yt-dlp / node / deno installed
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

## Control from Claude (MCP)

yt-tui can be driven by an MCP client such as Claude Code. It is off by default.

1. Enable it in `config.toml`:
   ```toml
   [control]
   enabled = true
   ```
2. Start yt-tui as usual, then register the bridge:
   ```sh
   claude mcp add yt-tui -- yt-tui mcp
   ```

The TUI listens on `127.0.0.1` (random port) on Linux, macOS and Windows. The port and a
random per-launch token are written to `control.json` in the data directory (readable only
by you on Unix); clients must present the token, so other local processes can't drive
playback. On Windows use `yt-tui.exe mcp`.
Tools cover playback, volume, shuffle/repeat, search and queue management. Account
cookies and sign-in are never exposed, and searches run as a guest.
