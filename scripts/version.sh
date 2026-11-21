#!/usr/bin/env bash
# Version + release helper. The single source of truth for the release version
# is the top `## [X.Y.Z] - YYYY-MM-DD` heading in CHANGELOG.md; it must equal
# `[package] version` in Cargo.toml and the ytmusic-tui entry in Cargo.lock.
#
# Usage:
#   scripts/version.sh version              print the Cargo.toml version
#   scripts/version.sh date                 print the date of the top CHANGELOG entry
#   scripts/version.sh title                print the release title "X.Y.Z - YYYY-MM-DD"
#   scripts/version.sh check                CHANGELOG == Cargo.toml == Cargo.lock, valid format
#   scripts/version.sh released [X.Y.Z]     exit 0 if tag vX.Y.Z already has a release or tag
#   scripts/version.sh check-release        fail unless the version is newer than every release
#   scripts/version.sh previous [X.Y.Z]     print the newest release tag older than X.Y.Z
#   scripts/version.sh notes CUR [PREV]     CHANGELOG entries with PREV < version <= CUR
#   scripts/version.sh body [X.Y.Z]         full GitHub release body (markdown)
#
# Commands that talk to GitHub need `gh` and GH_TOKEN (CI) or `gh auth login`.
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
CHANGELOG="$ROOT/CHANGELOG.md"
CARGO_TOML="$ROOT/Cargo.toml"
CARGO_LOCK="$ROOT/Cargo.lock"
PACKAGE="ytmusic-tui"
BREW_FORMULA="everything-tui/tap/ytmusic-tui"

die() { echo "❌ $*" >&2; [ -n "${GITHUB_ACTIONS:-}" ] && echo "::error::$*"; exit 1; }
ok() { echo "✅ $*" >&2; }

repo() {
  if [ -n "${GITHUB_REPOSITORY:-}" ]; then echo "$GITHUB_REPOSITORY"; return; fi
  # git@host:owner/repo.git or https://host/owner/repo(.git)
  git -C "$ROOT" remote get-url origin | sed -E 's#\.git$##' | sed -E 's#^.*[:/]([^/:]+/[^/]+)$#\1#'
}
gh_() { GH_REPO="github.com/$(repo)" gh "$@"; }

# 0 if $1 > $2 (X.Y.Z only)
version_gt() {
  local IFS=.
  local -a a b
  read -ra a <<<"$1"
  read -ra b <<<"$2"
  local i
  for i in 0 1 2; do
    if (( ${a[i]:-0} > ${b[i]:-0} )); then return 0; fi
    if (( ${a[i]:-0} < ${b[i]:-0} )); then return 1; fi
  done
  return 1
}

cargo_version() {
  awk '
    /^\[/ { in_pkg = ($0 == "[package]") }
    in_pkg && /^version[[:space:]]*=/ { gsub(/.*=[[:space:]]*"|".*/, ""); print; exit }
  ' "$CARGO_TOML"
}

lock_version() {
  [ -f "$CARGO_LOCK" ] || return 0
  awk -v pkg="$PACKAGE" '
    $0 == "name = \"" pkg "\"" { found = 1; next }
    found && /^version = / { gsub(/^version = "|"$/, ""); print; exit }
  ' "$CARGO_LOCK"
}

# The first `## ` heading must be a released-version heading (no "Unreleased").
changelog_header() { grep -m1 '^## ' "$CHANGELOG" || true; }
changelog_version() { changelog_header | sed -nE 's/^## \[([0-9]+\.[0-9]+\.[0-9]+)\] - [0-9]{4}-[0-9]{2}-[0-9]{2}$/\1/p'; }
changelog_date() { changelog_header | sed -nE 's/^## \[[0-9]+\.[0-9]+\.[0-9]+\] - ([0-9]{4}-[0-9]{2}-[0-9]{2})$/\1/p'; }

cmd_check() {
  local header cv tv lv
  header="$(changelog_header)"
  cv="$(changelog_version)"
  [ -n "$cv" ] || die "Top CHANGELOG.md heading must be '## [X.Y.Z] - YYYY-MM-DD', found: '${header:-<none>}'"
  tv="$(cargo_version)"
  [ -n "$tv" ] || die "No version under [package] in Cargo.toml"
  [[ "$tv" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "Cargo.toml version '$tv' is not X.Y.Z"
  echo "📋 CHANGELOG.md: $cv ($(changelog_date))" >&2
  echo "📦 Cargo.toml:   $tv" >&2
  [ "$cv" = "$tv" ] || die "Version mismatch: CHANGELOG.md has $cv but Cargo.toml has $tv. These must match."
  lv="$(lock_version)"
  if [ -n "$lv" ]; then
    echo "🔒 Cargo.lock:   $lv" >&2
    [ "$lv" = "$tv" ] || die "Cargo.lock still has $PACKAGE $lv. Run 'cargo update -p $PACKAGE' and commit Cargo.lock."
  fi
  ok "Versions match: $tv"
}

cmd_released() {
  local v="${1:-$(cargo_version)}" err
  if err="$(gh_ release view "v$v" --json tagName 2>&1 >/dev/null)"; then return 0; fi
  [[ "$err" == *"release not found"* ]] || die "Cannot query GitHub releases for $(repo): $err"
  if [ -n "$(git -C "$ROOT" ls-remote --tags origin "refs/tags/v$v")" ]; then return 0; fi
  return 1
}

# All published (non-draft) release versions, one per line. Fails if GitHub is unreachable.
release_versions() {
  local out
  out="$(gh_ release list --limit 200 --exclude-drafts --json tagName --jq '.[].tagName')" \
    || die "Cannot list GitHub releases for $(repo)"
  sed -nE 's/^v?([0-9]+\.[0-9]+\.[0-9]+)$/\1/p' <<<"$out"
}

cmd_previous() {
  local cur="${1:-$(cargo_version)}" best="" v
  while read -r v; do
    [ -n "$v" ] || continue
    if version_gt "$cur" "$v" && { [ -z "$best" ] || version_gt "$v" "$best"; }; then best="$v"; fi
  done <<<"$(release_versions)"
  [ -n "$best" ] && echo "v$best"
  return 0
}

cmd_check_release() {
  local v latest="" r
  v="$(cargo_version)"
  echo "🔎 Checking GitHub releases for $v" >&2
  if cmd_released "$v"; then
    die "Version $v is already released (tag v$v). Bump the version in CHANGELOG.md and Cargo.toml."
  fi
  while read -r r; do
    [ -n "$r" ] || continue
    if [ -z "$latest" ] || version_gt "$r" "$latest"; then latest="$r"; fi
  done <<<"$(release_versions)"
  if [ -z "$latest" ]; then ok "No previous releases; $v will be the first"; return; fi
  echo "📊 Latest release: $latest" >&2
  version_gt "$v" "$latest" || die "Version $v is not newer than the latest release $latest."
  ok "$v is newer than $latest"
}

cmd_notes() {
  local cur="${1:?usage: notes CUR [PREV]}" prev="${2:-0.0.0}"
  prev="${prev#v}"
  awk -v cur="$cur" -v prev="$prev" '
    function cmp(a, b,   x, y, i) {
      split(a, x, "."); split(b, y, ".")
      for (i = 1; i <= 3; i++) { if (x[i] + 0 > y[i] + 0) return 1; if (x[i] + 0 < y[i] + 0) return -1 }
      return 0
    }
    { sub(/\r$/, "") }
    /^## \[[0-9]+\.[0-9]+\.[0-9]+\]/ {
      v = $0; sub(/^## \[/, "", v); sub(/\].*/, "", v)
      inc = (prev == "0.0.0" || cmp(v, prev) > 0) && cmp(v, cur) <= 0
    }
    /^## / && !/^## \[[0-9]/ { inc = 0 }
    inc { print }
  ' "$CHANGELOG" | sed -e :a -e '/^\n*$/{$d;N;ba' -e '}'
}

platform_label() {
  case "$1" in
    aarch64-apple-darwin) echo "macOS, Apple Silicon" ;;
    x86_64-apple-darwin) echo "macOS, Intel" ;;
    x86_64-unknown-linux-gnu) echo "Linux x86_64 (glibc)" ;;
    aarch64-unknown-linux-gnu) echo "Linux arm64 (glibc)" ;;
    x86_64-unknown-linux-musl) echo "Linux x86_64 (static, musl)" ;;
    aarch64-unknown-linux-musl) echo "Linux arm64 (static, musl)" ;;
    x86_64-pc-windows-msvc) echo "Windows x86_64" ;;
    aarch64-pc-windows-msvc) echo "Windows ARM64" ;;
    *) echo "$1" ;;
  esac
}

cmd_body() {
  local v="${1:-$(cargo_version)}" tag prev entries repo_ base assets name triple
  tag="v$v"
  repo_="$(repo)"
  base="https://github.com/$repo_"
  prev="$(cmd_previous "$v")"
  entries="$(cmd_notes "$v" "${prev:-0.0.0}")"
  [ -n "$entries" ] || entries="## Changes"$'\n\n'"No changelog entries found between ${prev:-the start} and $tag."

  printf '%s\n\n' "$entries"

  cat <<EOF
## 📦 Install

**Homebrew** (macOS / Linux)

\`\`\`bash
brew install $BREW_FORMULA
\`\`\`

**Shell installer** (macOS / Linux)

\`\`\`bash
curl --proto '=https' --tlsv1.2 -LsSf $base/releases/download/$tag/$PACKAGE-installer.sh | sh
\`\`\`

**PowerShell installer** (Windows)

\`\`\`powershell
powershell -ExecutionPolicy Bypass -c "irm $base/releases/download/$tag/$PACKAGE-installer.ps1 | iex"
\`\`\`

EOF

  assets="$(gh_ release view "$tag" --json assets --jq '.assets[].name' 2>/dev/null || true)"
  if [ -n "$assets" ]; then
    echo "## ⬇️ Downloads"
    echo
    echo "| Platform | Archive | Checksum |"
    echo "|---|---|---|"
    while read -r name; do
      [[ "$name" =~ ^$PACKAGE-(.+)\.(tar\.xz|zip)$ ]] || continue
      triple="${BASH_REMATCH[1]}"
      echo "| $(platform_label "$triple") | [\`$name\`]($base/releases/download/$tag/$name) | [sha256]($base/releases/download/$tag/$name.sha256) |"
    done <<<"$assets"
    echo
  fi

  if [ -n "$prev" ]; then
    printf '## 📊 Changes\n\n[View changes](%s/compare/%s...%s) since %s\n\n' "$base" "$prev" "$tag" "$prev"
  fi

  printf -- '---\n\n**Full Changelog:** [View CHANGELOG.md](%s/blob/%s/CHANGELOG.md)\n' "$base" "$tag"
}

cmd="${1:-}"; shift || true
case "$cmd" in
  version) cargo_version ;;
  date) changelog_date ;;
  title) echo "$(changelog_version) - $(changelog_date)" ;;
  check) cmd_check ;;
  released) cmd_released "$@" ;;
  check-release) cmd_check_release ;;
  previous) cmd_previous "$@" ;;
  notes) cmd_notes "$@" ;;
  body) cmd_body "$@" ;;
  *) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
