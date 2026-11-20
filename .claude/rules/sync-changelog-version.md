---
description: "Keep CHANGELOG.md, Cargo.toml and Cargo.lock versions in sync with production changes"
---

# Keep Changelog & Version in Sync

For every **production** change in this repo (`src/**`, `Cargo.toml` dependencies or features, release config that changes what ships), load skill `sync-changelog-version` and follow it. Skip test-only, docs-only, CI-only and agent rule/skill changes.

Do **not** mark the task complete until `scripts/version.sh check` passes: the top CHANGELOG version equals `[package] version` in `Cargo.toml` and the `yt-tui` entry in `Cargo.lock`, and the bullets cover the user-visible changes since `HEAD`, written in plain language a non-technical user understands (see the skill's `references/writing-entries.md`).
