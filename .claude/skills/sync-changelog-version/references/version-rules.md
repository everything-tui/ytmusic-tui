# Version Bump Rules

Semver stays on `0.x.y` (no `1.0.0` major bump unless the maintainer asks for it). The base version is always the **committed** `[package] version` in `HEAD:Cargo.toml`.

## Bump Matrix

| Diff since `HEAD` (production only) | Bump | Result |
|---|---|---|
| Bug fixes only (`Fixed`, `Security` fixes) | **Patch** | `x.y.Z+1` |
| Added / Modified / Removed features, keybindings, CLI, config | **Minor** | `x.Y+1.0` |
| Mix of fix + feature in the same batch | **Minor** (higher wins) | `x.Y+1.0` |

Ignore test-only, docs-only, CI-only and agent-skill/rule changes when classifying.

## Same Unreleased Batch

If the working tree already has a top CHANGELOG entry whose version equals the **computed** target (and Cargo.toml already matches), **do not bump again**. Append bullets and refresh the date only.

Example: committed `0.2.0`, first change → create `0.3.0`. Second production change before commit → keep `0.3.0`, append bullets, set date to today.

A version that is committed but not yet released (no `v0.3.0` GitHub Release) is still the same batch across commits: keep adding to it until it ships through `publish`. Check with `scripts/version.sh released 0.3.0` (exit 0 = released).

## Entry Format

```markdown
## [X.Y.Z] - YYYY-MM-DD

### Added
- …

### Modified
- …

### Fixed
- …

### Removed
- …
```

- Include only sections that have bullets (`### Security` is allowed too).
- One blank line between version entries.
- Date is always the day of the **latest** change to that entry (local `YYYY-MM-DD`), never frozen at the first change.
- The header must match `## [X.Y.Z] - YYYY-MM-DD` exactly; `scripts/version.sh` and the release page parse it.

## Worked Examples

### Patch only (`0.2.0` → `0.2.1`)

Diff: unlock fails when the server omits KDF memory parameters.

```markdown
## [0.2.1] - 2026-12-02

### Fixed
- Tracks no longer stop a few seconds early on some albums
```

### Minor with Added + Modified (`0.2.0` → `0.3.0`)

```markdown
## [0.3.0] - 2026-12-10

### Added
- `yt-tui mcp` lets Claude add songs to the queue

### Modified
- Search now shows songs before albums and playlists
```

### After release, fix-only

Released `0.3.0`, then a bug fix → base `0.3.0`, patch → new top entry `0.3.1` dated today.
