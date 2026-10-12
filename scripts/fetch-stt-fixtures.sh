#!/usr/bin/env bash
# Seed the local STT model fixture cache (spec §18.3: real artifacts are
# fetched once, explicitly, never by a test run).
#
# The weights are NOT committed: they are 75 MB and they are not ours to
# redistribute from this repository. CI runs this once and caches the result,
# so the test suite itself performs zero network access.
set -euo pipefail
cd "$(dirname "$0")/.."

MODEL_DIR="tests/fixtures/stt/models"
MODEL_FILE="$MODEL_DIR/ggml-tiny.en.bin"
MODEL_URL="https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin"
MODEL_SHA256="921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f"

mkdir -p "$MODEL_DIR"
if [[ -f "$MODEL_FILE" ]]; then
  echo "model fixture already present: $MODEL_FILE"
else
  echo "downloading $MODEL_URL"
  curl --fail --location --silent --show-error --output "$MODEL_FILE.part" "$MODEL_URL"
  mv "$MODEL_FILE.part" "$MODEL_FILE"
fi

observed="$(shasum -a 256 "$MODEL_FILE" | cut -d' ' -f1)"
if [[ "$observed" != "$MODEL_SHA256" ]]; then
  echo "checksum mismatch for $MODEL_FILE" >&2
  echo "  expected $MODEL_SHA256" >&2
  echo "  observed $observed" >&2
  rm -f "$MODEL_FILE"
  exit 1
fi
echo "verified $MODEL_FILE ($observed)"
