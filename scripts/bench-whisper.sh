#!/usr/bin/env bash
# A1.1 local speech-to-text benchmark (spec 2.6, 17.5).
#
# Emits one machine-readable result validated against
# schemas/benchmark-result.v1.schema.json. Requires a whisper.cpp binary that
# you built or installed yourself; it is never downloaded by this script.
#
#   scripts/bench-whisper.sh --model <id> --backend <cpu|metal|cuda|vulkan> \
#                            --audio <file> [--binary <path>] [--out <file>]
set -euo pipefail
cd "$(dirname "$0")/.."

MODEL=""; BACKEND=""; AUDIO=""; BINARY="${WHISPER_BIN:-whisper-cli}"; OUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --model)   MODEL="$2"; shift 2 ;;
    --backend) BACKEND="$2"; shift 2 ;;
    --audio)   AUDIO="$2"; shift 2 ;;
    --binary)  BINARY="$2"; shift 2 ;;
    --out)     OUT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 64 ;;
  esac
done

: "${MODEL:?--model is required}"
: "${BACKEND:?--backend is required}"
: "${AUDIO:?--audio is required}"
[ -f "$AUDIO" ] || { echo "audio file not found: $AUDIO" >&2; exit 66; }
command -v "$BINARY" >/dev/null 2>&1 || {
  echo "whisper binary not found: $BINARY (set WHISPER_BIN or pass --binary)" >&2
  exit 127
}

MODEL_PATH="${WHISPER_MODEL_DIR:-$HOME/Library/Application Support/plaudon/models/whisper}/$MODEL.bin"
[ -f "$MODEL_PATH" ] || { echo "model not found: $MODEL_PATH" >&2; exit 66; }

# Refuse to benchmark weights that do not match their manifest. A benchmark of
# the wrong bytes is worse than no benchmark.
if [ -f "$MODEL_PATH.sha256" ]; then
  EXPECTED="$(cut -d" " -f1 < "$MODEL_PATH.sha256")"
  ACTUAL="$(shasum -a 256 "$MODEL_PATH" | cut -d" " -f1)"
  [ "$EXPECTED" = "$ACTUAL" ] || { echo "model digest mismatch for $MODEL" >&2; exit 65; }
fi

case "$(uname -s)" in
  Darwin)
    HOST_OS="macOS $(sw_vers -productVersion) ($(sw_vers -buildVersion))"
    HOST_CPU="$(sysctl -n machdep.cpu.brand_string)"
    HOST_RAM="$(sysctl -n hw.memsize)"
    ;;
  Linux)
    HOST_OS="$(uname -sr)"
    HOST_CPU="$(awk -F": " "/model name/ {print \$2; exit}" /proc/cpuinfo)"
    HOST_RAM="$(( $(awk "/MemTotal/ {print \$2}" /proc/meminfo) * 1024 ))"
    ;;
  *)
    echo "use scripts/bench-whisper.ps1 on Windows" >&2
    exit 64
    ;;
esac

AUDIO_MS="$(python3 - "$AUDIO" <<'PY'
import struct, sys, wave
try:
    with wave.open(sys.argv[1], "rb") as handle:
        print(int(handle.getnframes() * 1000 / handle.getframerate()))
except Exception:
    print(0)
PY
)"
[ "$AUDIO_MS" -gt 0 ] || { echo "could not determine audio duration; use a WAV input" >&2; exit 65; }

OUT="${OUT:-docs/benchmarks/results/$(date +%Y%m%d-%H%M%S)-$BACKEND-$MODEL.json}"
mkdir -p "$(dirname "$OUT")"

START="$(python3 -c "import time; print(int(time.time()*1000))")"
# /usr/bin/time -l on macOS reports peak RSS in bytes.
TIME_OUTPUT="$(mktemp)"; trap 'rm -f "$TIME_OUTPUT"' EXIT
/usr/bin/time -l "$BINARY" -m "$MODEL_PATH" -f "$AUDIO" >/dev/null 2>"$TIME_OUTPUT" || {
  echo "whisper run failed; see $TIME_OUTPUT" >&2
  exit 70
}
END="$(python3 -c "import time; print(int(time.time()*1000))")"
PEAK_RSS="$(awk "/maximum resident set size/ {print \$1; exit}" "$TIME_OUTPUT")"
PEAK_RSS="${PEAK_RSS:-0}"

python3 - "$OUT" "$HOST_OS" "$HOST_CPU" "$HOST_RAM" "$BACKEND" "$MODEL" "$AUDIO_MS" \
         "$(( END - START ))" "$PEAK_RSS" <<'PY'
import json, sys, datetime
out, host_os, host_cpu, host_ram, backend, model, audio_ms, wall_ms, peak_rss = sys.argv[1:10]
document = {
    "schema_version": 1,
    "host_os": host_os,
    "host_cpu": host_cpu,
    "host_ram_bytes": int(host_ram),
    "backend": backend,
    "model_id": model,
    "audio_duration_ms": int(audio_ms),
    "wall_clock_ms": int(wall_ms),
    "peak_rss_bytes": int(peak_rss),
    "cancellation_latency_ms": None,
    "segments_reported": False,
    "word_timestamps_reported": False,
    "run_on": datetime.date.today().isoformat(),
}
with open(out, "w", encoding="utf-8") as handle:
    json.dump(document, handle, indent=2)
    handle.write("\n")
print(f"wrote {out}")
PY

echo
echo "Set segments_reported and word_timestamps_reported from the engine output you observed."
echo "An unobserved capability stays false; spec 8.3 treats unknown as unsupported."
