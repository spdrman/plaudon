#!/usr/bin/env bash
# A1.3 Apple runtime availability probe (spec 2.3, 2.6, 11.5).
#
# Opt-in and macOS-only. It builds the bridge, links the probe binary and
# prints the raw probe documents so a compatibility cell can be recorded
# honestly. It transcribes nothing unless you pass an audio file.
#
#   scripts/probe-apple.sh [audio-file] [locale]
set -euo pipefail
cd "$(dirname "$0")/.."

if [ "$(uname -s)" != "Darwin" ]; then
  echo "the Apple probe runs on macOS only" >&2
  exit 1
fi

scripts/build-apple-bridge.sh build >/dev/null
BRIDGE_DIR="$(pwd)/native/apple-bridge/build"

echo "host: $(sw_vers -productName) $(sw_vers -productVersion) ($(sw_vers -buildVersion))"
echo "cpu:  $(sysctl -n machdep.cpu.brand_string)"
echo

RUSTFLAGS="-L $BRIDGE_DIR" cargo run -q -p plaudon-ai-apple --features apple-bridge \
  --bin plaud-apple-probe -- "$@"

echo
echo "Record the result in docs/spikes/A1.3-apple-availability.md and, if a"
echo "transcription succeeded, in compatibility.yaml with a test_run_id."
