#!/bin/sh
# Run the hermetic extraction evaluation suite (EPIC I task I2.5).
#
# This script:
# 1. Runs the extraction-eval binary with the Perfect profile
# 2. Writes the report to target/eval/extraction-eval.json
# 3. Applies threshold-based gating with --check
#
# Must be run from the repository root.

set -eu

# Ensure we're in the repo root
if [ ! -f "Cargo.toml" ] || [ ! -d "tests/fixtures/ai/corpus" ]; then
    echo "Error: Must run from repository root" >&2
    exit 1
fi

# Create output directory
mkdir -p target/eval

# Run the evaluation
echo "Running extraction evaluation suite..."
cargo run --release -p plaudon-extraction-eval --bin extraction-eval -- \
    --out target/eval/extraction-eval.json \
    --check

echo "Done."
