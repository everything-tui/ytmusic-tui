---
name: sync-changelog-version
description: >-
  Sync CHANGELOG.md, Cargo.toml and Cargo.lock versions after production changes:
  create or update the top entry, bump semver from the git diff since HEAD, refresh
  the date. Use when finishing production work, bumping versions, preparing a
  release, or editing CHANGELOG.md / the package version.
paths:
  - "src/**/*.rs"
  - "Cargo.toml"
  - "CHANGELOG.md"
---

# Sync Changelog & Version

Run after production changes in the same task, or when invoked explicitly (`/sync-changelog-version`). Skip test-only, docs-only, or CI-only tasks.

## Workflow

```
- [ ] Scope: production Rust code, dependencies or features changed
- [ ] Base version from `git show HEAD:Cargo.toml` ([package] version)
- [ ] Classify `git diff HEAD` → Added / Modified / Fixed / Removed
- [ ] Compute target semver (see references/version-rules.md)
- [ ] Create or update top CHANGELOG entry; set date to today
- [ ] Every bullet passes references/writing-entries.md (plain language, user-visible)
- [ ] Set Cargo.toml [package] version to the same X.Y.Z
- [ ] `cargo update -p ytmusic-tui` so Cargo.lock matches
- [ ] `scripts/version.sh check` passes
```

## Steps

1. **Scope**: proceed only if `src/**` (not `tests/`), `Cargo.toml` dependencies/features, or shipped config changed.
2. **Base version**: read `[package] version` from `git show HEAD:Cargo.toml`.
3. **Classify**: `git diff HEAD --name-only` and `git diff HEAD -- src`. Map to sections:

| Change type | Section |
|---|---|
| New command, keybinding, screen, CLI flag, config option, something a user can now do | **Added** |
| Behavior / UI / default / performance change a user notices | **Modified** |
| Internal refactor, API plumbing, code cleanup (nothing visible) | Leave out (see references/writing-entries.md) |
| Bug fix | **Fixed** |
| Deleted feature, flag, keybinding or config option | **Removed** |
| Vulnerability fix or hardening | **Security** |

4. **Target version**: apply [references/version-rules.md](references/version-rules.md). If the top CHANGELOG entry already equals the computed version (same unreleased batch), do **not** re-bump. Append bullets only.
5. **CHANGELOG**: create or update the top entry:
   - No matching top entry → insert below `# Changelog`: `## [X.Y.Z] - YYYY-MM-DD` + sections + bullets
   - Matching entry → append bullets; **always** rewrite the header date to today (`YYYY-MM-DD`)
   - Never use an `## Unreleased` heading: CI rejects it
   - **Write for end users, not developers.** Follow [references/writing-entries.md](references/writing-entries.md): describe what a user notices, in plain language; keep keys, CLI commands and `config.toml` settings; drop endpoints, function names, crypto details and invisible refactors
6. **Cargo.toml**: set `[package] version` to the same `X.Y.Z`, then run `cargo update -p ytmusic-tui` (updates only the ytmusic-tui entry in `Cargo.lock`).
7. **Verify**: `scripts/version.sh check` passes; date is today; bullets cover the user-visible changes since HEAD; reread each bullet as a non-technical user and rewrite any that need code knowledge to understand.

## Releasing (for reference)

Merging into `publish` releases the top CHANGELOG version: `create-release.yml` validates it and starts `release.yml`, which builds all targets, creates tag `vX.Y.Z` and the GitHub Release, updates Homebrew, and writes the release page from CHANGELOG.md. PRs into `publish` fail if the version is already released or not newer than the latest release.

## Definition of Done

CHANGELOG.md + Cargo.toml + Cargo.lock synced, enforced by the `sync-changelog-version` rule and the `Validate Version` workflow.
