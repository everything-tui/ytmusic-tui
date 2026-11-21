# ytmusic-tui

**YouTube Music in the terminal** — a polished media-player TUI written in Rust.

Search, play, queue, library, radio, synced lyrics, album-art-driven accents, an FFT visualizer, and macOS Now Playing / media keys. Sign in once by pasting a cURL request; ytmusic-tui then keeps the session alive on its own.

## Install

### Homebrew (macOS / Linux)

```bash
brew install everything-tui/tap/ytmusic-tui
```

Update later with `brew upgrade ytmusic-tui`.

### Installer scripts

```bash
# Linux / macOS
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/everything-tui/ytmusic-tui/releases/latest/download/ytmusic-tui-installer.sh | sh
```

```powershell
# Windows (PowerShell)
powershell -ExecutionPolicy Bypass -c "irm https://github.com/everything-tui/ytmusic-tui/releases/latest/download/ytmusic-tui-installer.ps1 | iex"
```

The installers place the `ytmusic-tui` binary on your `PATH` (under `CARGO_HOME` by default).

### Prebuilt binaries

Every [GitHub Release](https://github.com/everything-tui/ytmusic-tui/releases) has these archives. Each comes with a `.sha256` checksum:

| Platform | Archive |
|---|---|
| macOS, Apple Silicon | `ytmusic-tui-aarch64-apple-darwin.tar.xz` |
| macOS, Intel | `ytmusic-tui-x86_64-apple-darwin.tar.xz` |
| Linux x86_64 (glibc) | `ytmusic-tui-x86_64-unknown-linux-gnu.tar.xz` |
| Linux arm64 (glibc) | `ytmusic-tui-aarch64-unknown-linux-gnu.tar.xz` |
| Windows x86_64 | `ytmusic-tui-x86_64-pc-windows-msvc.zip` |
| Windows ARM64 | `ytmusic-tui-aarch64-pc-windows-msvc.zip` |

On Linux the binaries need ALSA (`libasound2`) and D-Bus (`libdbus-1-3`) at runtime; both are present on nearly every desktop distribution.

### Build from source

You need the stable Rust toolchain from [rustup](https://rustup.rs). Then, from the repository root:

```bash
cargo install --path . --locked
```

That puts `ytmusic-tui` in `~/.cargo/bin`. To get just the binary in `target/release/ytmusic-tui` instead, run `cargo build --release`.

Prerequisites for each platform:

**macOS**: `xcode-select --install`

**Linux (Debian / Ubuntu)**

```bash
sudo apt install build-essential pkg-config libasound2-dev libdbus-1-dev
```

On Fedora: `sudo dnf install gcc pkgconf-pkg-config alsa-lib-devel dbus-devel`. On Arch: `sudo pacman -S base-devel alsa-lib dbus`.

**Windows**: install [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with the "Desktop development with C++" workload, then run `cargo build --release`.

### Dependencies

No external programs are required for playback — stream URLs are resolved natively via YouTube’s Innertube API (no `yt-dlp`, Node, Deno, or Bun). When signed in, tracks the default clients refuse (Music Premium–only, age-restricted) fall back to the YouTube Music web client; its ciphered URLs are solved with [yt-dlp’s EJS solver](https://github.com/yt-dlp/ejs) running on an embedded QuickJS.

No YouTube API key. No manual cookie copying in the happy path.

**Known limitations:** Premium-only tracks need a signed-in Music Premium account; age-restricted tracks need a signed-in, age-verified account (without Premium, YouTube may additionally demand a PO token, which isn’t supported). Region-locked tracks may not play. If YouTube changes player clients, playback may need updated client constants in the resolver.

## Quick start

```bash
ytmusic-tui
```

1. Press **Enter** to sign in (opens [music.youtube.com](https://music.youtube.com)), or **G** for guest mode.
2. Press **/** to search, **Enter** to play.
3. Press **?** for the full keyboard reference.

CLI helpers:

```bash
ytmusic-tui login     # prints sign-in instructions
ytmusic-tui logout
ytmusic-tui doctor    # print environment diagnostics
ytmusic-tui --debug   # verbose file logging
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

1. **Paste once** — open a *private* window, sign in at music.youtube.com, copy any request as cURL (DevTools → Network) and paste it in ytmusic-tui (`L` or `P`). Then close the private window.
2. ytmusic-tui owns that session from then on: it rotates the short-lived cookies in the background the way a browser tab does, so you stay signed in. No browser access or Full Disk Access is needed.
3. **Guest** — search and play without an account (no library / likes).

The session is stored in a `0600` file (`auth.json`) under the app data directory. The OS keychain is not used, so there are no password prompts.

## Config

Written on first run to the platform config dir, e.g.:

- macOS: `~/Library/Application Support/com.ytmusic-tui.ytmusic-tui/config.toml`
- Linux: `~/.config/ytmusic-tui/config.toml`

```toml
[ui]
theme = "midnight"         # midnight | oled | catppuccin | tokyo-night | nord | gruvbox | rose-pine | dracula | light
accent = ""                # "#rrggbb" to override the theme's accent
dynamic_accent = true      # tint the accent with the playing track's cover art
icons = "text"             # "nerd" for Nerd Font icons in the sidebar and lists
visualizer = "bars"        # bars | mirror | wave | off
show_hints = true

[ui.colors]                # optional per-color overrides on top of the theme
# bg = "#101014"
# bg_sidebar = "#0a0a0d"   # also: bg_player bg_hover bg_selected bg_modal
# text = "#ececf1"         # also: dim muted faint border error success

[playback]
autoplay = true
volume = 0.8
cache_size_mb = 1024
prefetch = true
prewarm = true             # fetch the start of highlighted/upcoming tracks so they play instantly
eq_enabled = true
eq_gains = [0.0, 0.0, 0.0, 0.0, 0.0]  # Bass … Treble, ±12 dB
```

Press **`e`** for the equalizer (presets include Bass Boost). Switch themes live from the command palette (**`Ctrl+P`**, type "theme"); the choice is saved to the config.

Logs: `…/Caches/…/logs/ytmusic-tui.log` (never written to the terminal).

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

## Releasing

The version lives in three places that must match: the top `## [X.Y.Z] - YYYY-MM-DD` entry in `CHANGELOG.md`, `version` in `Cargo.toml`, and `Cargo.lock`. Check with `scripts/version.sh check` (CI runs it on every PR).

To release, open a PR from `main` into `publish` and merge it. CI builds every platform with [cargo-dist](https://opensource.axo.dev/cargo-dist/), creates tag `vX.Y.Z` and the GitHub Release (notes come from `CHANGELOG.md`), and updates the Homebrew formula in `everything-tui/homebrew-tap`.

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
- [ ] `ytmusic-tui doctor` prints sensible diagnostics

## Disclaimer

This project is **not** affiliated with, endorsed by, or supported by Google or YouTube. Use at your own risk and respect YouTube’s Terms of Service.

## License

MIT

## Control from Claude (MCP)

ytmusic-tui can be driven by an MCP client such as Claude Code. It is off by default.

1. Enable it in `config.toml`:
   ```toml
   [control]
   enabled = true
   ```
2. Start ytmusic-tui as usual, then register the bridge:
   ```sh
   claude mcp add ytmusic-tui -- ytmusic-tui mcp
   ```

The TUI listens on `127.0.0.1` (random port) on Linux, macOS and Windows. The port and a
random per-launch token are written to `control.json` in the data directory (readable only
by you on Unix); clients must present the token, so other local processes can't drive
playback. On Windows use `ytmusic-tui.exe mcp`.
Tools cover playback, volume, shuffle/repeat, search and queue management. Account
cookies and sign-in are never exposed, and searches run as a guest.
