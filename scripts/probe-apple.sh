#!/usr/bin/env bash
# A1.3 Apple runtime availability probe (spec 2.3, 2.6, 11.5).
#
# Opt-in and macOS-only. It builds the bridge, links the probe binary and
# prints the raw probe documents so a compatibility cell can be recorded
# honestly. It transcribes nothing unless you pass an audio file.
#
#   scripts/probe-apple.sh [audio-file] [locale]
#   scripts/probe-apple.sh --json --locale en-US --audio fixture.wav
set -euo pipefail
cd "$(dirname "$0")/.."

if [ "$(uname -s)" != "Darwin" ]; then
  echo "the Apple probe runs on macOS only" >&2
  exit 1
fi

echo "host: $(sw_vers -productName) $(sw_vers -productVersion) ($(sw_vers -buildVersion))"
echo "cpu:  $(sysctl -n machdep.cpu.brand_string)"
echo

# The crate's build script compiles and links the Swift library itself; no
# RUSTFLAGS and no prebuilt directory are needed.
cargo run -q -p plaudon-ai-apple --features apple-bridge \
  --bin plaud-apple-probe -- "$@"

echo
echo "Add --json to emit an evidence record for the release bundle, and record"
echo "the result in docs/benchmarks/apple-native-ai.md with a test_run_id."
