# Changelog

## [0.3.0] - 2026-11-21

### Modified

- The sign-in window (`L` or `P`) now walks you through every step of copying your sign-in from the browser, and warns you to use a fresh private / incognito window. Copying it from your normal YouTube Music tab stops working soon after, because that tab keeps refreshing the sign-in
- When you get signed out, the app now tells you to sign in again from a new private window

### Fixed

- In the full-screen player with lyrics, the play, skip, shuffle, repeat and like buttons are now bigger and sit above the progress bar, with a longer volume bar next to them, instead of being squeezed into a tiny row at the very bottom

## [0.2.0] - 2026-11-21

### Modified

- The app is now called **ytmusic-tui**: run `ytmusic-tui` instead of `yt-tui` (also `ytmusic-tui mcp`, `ytmusic-tui doctor`, and `brew install everything-tui/tap/ytmusic-tui`). Your settings and sign-in move over automatically the first time you start it

## [0.1.0] - 2026-11-20

### Added

- **YouTube Music in your terminal**: search songs, albums, artists and playlists, browse the home feed and play anything with `Enter`
- Play, pause, seek, volume, shuffle, repeat and a reorderable queue. Radio keeps the music going when the queue runs out
- Sign in once by pasting a request copied from your browser. yt-tui keeps you signed in on its own, or use guest mode without an account
- Your library after signing in: liked songs, playlists, albums, artists and history
- Music Premium–only and age-restricted tracks play when your account allows them
- Synced lyrics that highlight the current line
- A spectrum visualizer, album art, and an accent color taken from the playing track's cover
- A 5-band equalizer (`e`) with presets such as Bass Boost
- Nine built-in themes, switchable live from the command palette (`Ctrl+P`)
- macOS media keys and Control Center Now Playing
- Picks up where you left off: the last track opens paused at the same position
- Control playback from Claude or another MCP client with `yt-tui mcp` (off by default)
- `yt-tui doctor` prints diagnostics when something doesn't work
