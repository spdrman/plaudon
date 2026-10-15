#!/usr/bin/env bash
# Re-derive the committed interview excerpt (issue #308) from its pinned
# public-domain source, and check the result against the committed bytes.
#
# The excerpt itself is committed (it is 1.8 MB and a US government work), so
# tests never need this script and never touch the network. It exists so the
# provenance in tests/fixtures/stt/interview/PROVENANCE.md can be checked by
# anyone: download the source, verify its SHA-256, cut the same span with a
# stream copy (no re-encode), and compare.
#
# Usage:
#   scripts/make-interview-fixture.sh            # download the source
#   scripts/make-interview-fixture.sh --source F # use an already downloaded F
#
# Needs curl, shasum and ffmpeg. The downloaded source goes into a fresh
# temporary directory and is only ever read by ffmpeg.
set -euo pipefail
cd "$(dirname "$0")/.."

SOURCE_URL="https://upload.wikimedia.org/wikipedia/commons/6/67/HWHAP_Ep407_A_Place_in_Space_to_Call_Home-_Part_1_%28Ep407_A_Place_in_Space_to_Call_Home_Part_1%29.mp3"
SOURCE_SHA256="a6b65f84a64161ca52ab5d6146db69c55a4add90bc0b914abfa84de0e61f60bb"
START_S="130.3"
END_S="282.7"
OUT="tests/fixtures/stt/interview/hwhap-407-excerpt.mp3"
OUT_SHA256="90a533a5cfefc05a5ffbf0d5f7c5c13a3a0673c2e5a468bc44c6fb23b3e7e630"

source_file=""
if [[ "${1:-}" == "--source" ]]; then
  source_file="${2:?--source needs a file}"
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

if [[ -z "$source_file" ]]; then
  echo "downloading $SOURCE_URL"
  curl --fail --location --silent --show-error --output "$work/source.mp3" "$SOURCE_URL"
  source_file="$work/source.mp3"
fi

observed="$(shasum -a 256 "$source_file" | cut -d' ' -f1)"
if [[ "$observed" != "$SOURCE_SHA256" ]]; then
  echo "checksum mismatch for the source recording" >&2
  echo "  expected $SOURCE_SHA256" >&2
  echo "  observed $observed" >&2
  exit 1
fi
echo "verified source ($observed)"

# Stream copy: the excerpt is the source's own MP3 frames, so there is no
# second generation of lossy encoding. Metadata and the Xing header are
# dropped so the bytes do not depend on the ffmpeg build that wrote them.
ffmpeg -v error -nostdin -y -ss "$START_S" -to "$END_S" -i "$source_file" \
  -map 0:a:0 -c copy -map_metadata -1 -id3v2_version 0 -write_xing 0 \
  -fflags +bitexact -flags:a +bitexact "$work/excerpt.mp3"

made="$(shasum -a 256 "$work/excerpt.mp3" | cut -d' ' -f1)"
if [[ "$made" != "$OUT_SHA256" ]]; then
  echo "re-derived excerpt does not match the pinned digest" >&2
  echo "  expected $OUT_SHA256" >&2
  echo "  observed $made" >&2
  echo "A different ffmpeg may cut on a different frame; $OUT is left alone." >&2
  exit 1
fi
mkdir -p "$(dirname "$OUT")"
if [[ -f "$OUT" ]] && [[ "$(shasum -a 256 "$OUT" | cut -d' ' -f1)" == "$made" ]]; then
  echo "re-derived excerpt matches $OUT ($made)"
else
  cp "$work/excerpt.mp3" "$OUT"
  echo "wrote $OUT ($made)"
fi
