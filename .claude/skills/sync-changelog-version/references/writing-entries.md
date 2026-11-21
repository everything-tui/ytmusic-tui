# Writing Changelog Entries

The CHANGELOG is copied verbatim onto the GitHub Release page. Its readers are **people who use ytmusic-tui**, not people who read its code. Write every bullet so a regular music listener understands it without knowing Rust, YouTube's internal APIs or audio codecs.

**Goal:** a user skims the release notes and can tell in seconds whether the update affects how they use the app: a shortcut moved, a bug they hit is fixed, a feature they wanted exists now. Changed or removed shortcuts, commands and settings matter most: always say what the old behavior was and what to do now.

## The one test

Before writing a bullet, ask: **"Would a user notice this while using the app?"**

| Answer | What to do |
|---|---|
| Yes: they see, press, type or feel something different | Write a bullet in plain language (rules below) |
| No, but it protects them (security fix, data-loss fix) | Write a bullet that says what risk is gone, in plain words |
| No: refactor, internal API, code cleanup, test helpers | **Leave it out** |
| The whole batch is invisible | One bullet under `### Modified`: `- Behind-the-scenes improvements to reliability` (or `…to speed` if users would feel it) |

## How to write a bullet

1. **Describe what the user experiences, not what the code does.** "Search results now appear as you type" not "Debounced search query evaluation".
2. **Start with the thing they touch**: a screen, a key, a command, a setting. "The queue…", "Pressing `e`…", "`ytmusic-tui doctor`…".
3. **Fixed bullets describe the old problem from the user's side.** "Unlocking no longer fails on some self-hosted servers" not "Handle missing `kdfMemory` in prelogin response".
4. **One change per bullet, about 25 words max.** Split long bullets. Group 4+ related bullets under a short bold lead bullet with sub-bullets (as with a redesign).
5. **Present tense, plain verbs**: "now shows", "opens", "no longer crashes", "you can now…".
6. **Say why it matters when that isn't obvious**: "…so you don't lose unsaved changes".

## Keep vs. translate

**Keep** these. Users type or see them:

- Keys: `Ctrl+t`, `g g`, `Enter`, `Esc` (always in backticks)
- CLI commands and flags: `ytmusic-tui mcp`, `--debug`
- Settings in `config.toml`: `theme = "nord"`, `[sync] background_interval_minutes`
- Environment variables a user sets: `BW_TUI_HIBP_API_KEY`
- Product terms users already know: song, album, artist, playlist, queue, radio, library, liked songs, lyrics, Music Premium, guest mode

**Translate or drop** these:

| Don't write | Write instead |
|---|---|
| Endpoint paths (`/youtubei/v1/player`), "Innertube" | "Some tracks failed to play" |
| Function/type names (`Resolver::resolve`, `PlayerState`) | What the user notices: "Skipping tracks is faster" |
| Codec / container details (Opus, WebM, itag 251, cipher, n-param) | "More tracks play", "Better sound quality" |
| "client", "WEB_REMIX", "PO token", "EJS solver" | Describe the visible effect, or drop |
| "SAPISID", "cookie rotation" | "You stay signed in" |
| "hit-test", "re-derive layout", "token", "struct", "parse" | Describe the visible effect, or drop |
| File names, crate names, module paths | Drop |
| "Implemented", "refactored", "plumbed" | "Added", "now", "you can" |

## Examples

| Too technical | User friendly |
|---|---|
| Added Opus decoding via `rusty-opus` for itag 251 WebM streams | More tracks play, in better sound quality |
| Resolver falls back to WEB_REMIX with EJS-deciphered URLs | Music Premium–only tracks now play when you are signed in with Premium |
| Cancel in-flight resolve on track change to avoid deadlock | Fixed: the app could freeze when you skipped tracks quickly |
| Background SAPISID rotation every 10 min | You now stay signed in without pasting a new request |
| `[ui.colors]` overrides were parsed but never applied | Fixed: custom colors in `config.toml` were ignored |
