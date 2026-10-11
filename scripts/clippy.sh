#!/usr/bin/env bash
# Lint the whole workspace, including tests and benches, with warnings denied.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo clippy --workspace --all-targets --all-features -- -D warnings
