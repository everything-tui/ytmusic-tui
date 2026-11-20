#!/usr/bin/env bash
# Builds and starts yt-tui. Extra arguments are passed to the app.
set -euo pipefail
cd "$(dirname "$0")/.."
exec cargo run -- "$@"
