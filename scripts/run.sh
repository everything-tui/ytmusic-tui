#!/usr/bin/env bash
# Builds and starts ytmusic-tui. Extra arguments are passed to the app.
set -euo pipefail
cd "$(dirname "$0")/.."
exec cargo run -- "$@"
