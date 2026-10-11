#!/usr/bin/env bash
# Format the Rust workspace. Pass --check to fail instead of rewriting.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all "$@"
