#!/usr/bin/env bash
# Regenerate docs/assets/demo.gif from the real user interface (task O2.9).
#
# The whole job runs in the e2e container, never on a workstation:
#
#   1. `apps/plaudon-egui/tests/demo_capture.rs` drives the application — the
#      real engine over a temporary library, fakes only at the edges the
#      operating system owns — and renders each step of the story through the
#      software wgpu adapter, writing one PNG per step plus an `ffconcat`
#      manifest that gives each one its hold;
#   2. ffmpeg, installed into the same container, downscales the frames and
#      assembles them with a generated palette;
#   3. ffprobe measures the result and writes a report.
#
# Nothing is installed on the host and nothing is rendered on it: the frames
# come from the one renderer the snapshot policy trusts (mesa/lavapipe under
# Docker, docs/EGUI_TESTING.md §4), so the demo looks the same whoever
# regenerates it.
#
# Usage:
#   scripts/make-demo-gif.sh                 # host picked by scripts/e2e-remote.sh
#   PLAUDON_E2E_HOST=local scripts/make-demo-gif.sh
#
# Environment overrides:
#   PLAUDON_DEMO_WIDTH   output width in pixels          (960)
#   PLAUDON_DEMO_COLORS  palette size                    (128)
#   PLAUDON_DEMO_MAX_MB  fail if the GIF is larger       (8)
# Everything scripts/e2e-remote.sh understands applies too (PLAUDON_E2E_HOST,
# PLAUDON_E2E_CPUS, …).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

WIDTH="${PLAUDON_DEMO_WIDTH:-960}"
COLORS="${PLAUDON_DEMO_COLORS:-128}"
MAX_MB="${PLAUDON_DEMO_MAX_MB:-8}"

RESULTS_DIR="${PLAUDON_E2E_RESULTS:-$REPO_ROOT/target/e2e-results}"
# The remote runner copies back what it finds under a `visual/` directory, so
# that is where the container leaves the finished GIF and its report. It writes
# them into its own disposable copy of the working tree, never into this one.
RELAY_DIR="apps/plaudon-egui/tests/visual"
GIF_DEST="$REPO_ROOT/docs/assets/demo.gif"

rm -f "$RESULTS_DIR/$RELAY_DIR/demo.gif" "$RESULTS_DIR/$RELAY_DIR/demo-gif-report.json"

# `fontconfig` and `libdbus-1-dev` are what the application's own Linux build
# needs (docs/EGUI_TESTING.md §5); `ffmpeg` is what assembles the result.
export PLAUDON_E2E_APT_PACKAGES="${PLAUDON_E2E_APT_PACKAGES:-} ffmpeg libdbus-1-dev"

scripts/e2e-remote.sh shell "
set -euo pipefail
frames=/work/target/demo-frames
rm -rf \"\$frames\"

cargo test -p plaudon-egui --test demo_capture -- --ignored --nocapture

test -s \"\$frames/frames.txt\"
mkdir -p '$RELAY_DIR'

# One pass: the scaled frames feed both the palette generator and the
# quantizer. There is deliberately no `fps` filter: the capture wrote one frame
# per step with the hold it wants, GIF carries a delay per frame, and
# resampling a mostly static recording to a constant rate would multiply its
# size by the frame rate for no visible gain. `-fps_mode vfr` is what keeps
# those durations instead of duplicating frames to a constant rate.
ffmpeg -hide_banner -loglevel error -y \
  -f concat -safe 0 -i \"\$frames/frames.txt\" \
  -filter_complex \
    'scale=$WIDTH:-2:flags=lanczos,split[a][b];[a]palettegen=max_colors=$COLORS:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle' \
  -fps_mode vfr -loop 0 '$RELAY_DIR/demo.gif'

ffprobe -hide_banner -v error -count_frames \
  -select_streams v:0 -show_entries stream=width,height,nb_read_frames \
  -show_entries format=duration,size -of json '$RELAY_DIR/demo.gif' \
  > '$RELAY_DIR/demo-gif-report.json'
cat '$RELAY_DIR/demo-gif-report.json'
"

report="$RESULTS_DIR/$RELAY_DIR/demo-gif-report.json"
gif="$RESULTS_DIR/$RELAY_DIR/demo.gif"
[[ -s "$gif" ]] || { echo "error: the runner produced no GIF" >&2; exit 1; }
[[ -s "$report" ]] || { echo "error: the runner produced no ffprobe report" >&2; exit 1; }

bytes=$(wc -c <"$gif")
limit=$((MAX_MB * 1024 * 1024))
if ((bytes > limit)); then
  echo "error: demo.gif is $bytes bytes, over the ${MAX_MB} MB budget" >&2
  echo "       lower PLAUDON_DEMO_COLORS or PLAUDON_DEMO_WIDTH, or shorten the capture" >&2
  exit 1
fi

mkdir -p "$(dirname "$GIF_DEST")"
cp "$gif" "$GIF_DEST"
echo "==> wrote $GIF_DEST ($bytes bytes)"
echo "==> ffprobe report: $report"
cat "$report"
