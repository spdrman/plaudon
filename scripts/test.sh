#!/usr/bin/env bash
# Run the hermetic test suite. No network, no vendor CLI, no paid endpoint.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo test --workspace --all-targets
cargo test --workspace --doc
