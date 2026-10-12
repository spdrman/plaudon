#!/usr/bin/env bash
# A1.2 OpenRouter contract probe (spec 2.2, 2.5B, 8.6).
#
# Two modes:
#
#   catalog   Reads the PUBLIC, unauthenticated model catalog and writes a
#             sanitized snapshot. No credential is used or required.
#
# The same ground can be covered from Rust by the opt-in live suite, which is
# inert unless both flags are present and is capped at one bundled 32 KiB clip:
#
#   PLAUDON_OPENROUTER_LIVE=1 OPENROUTER_API_KEY=sk-or-... \
#   PLAUDON_OPENROUTER_LIVE_MODEL=openai/whisper-1 \
#     cargo test -p plaudon-ai-openrouter --test live_provider -- --nocapture
#   exchange  Sends ONE real transcription request for ONE model, so the
#             request family, accepted formats, response fields, provider
#             route and retention behaviour can be observed. Opt-in,
#             rate-limited, cost-capped and never run in CI.
#
# Usage:
#   scripts/probe-openrouter.sh catalog [output.json]
#   OPENROUTER_API_KEY=... scripts/probe-openrouter.sh exchange <model-id> <audio-file>
#
# The script never writes a credential to disk and never prints a request or
# response body that could contain one: `exchange` reports the status line,
# the response field names and the provider route only.
set -euo pipefail
cd "$(dirname "$0")/.."

API_BASE="https://openrouter.ai/api/v1"
MODE="${1:-}"

sanitize() {
  python3 - "$1" <<'PY'
import json, sys
raw = json.load(open(sys.argv[1]))
models = raw.get("data", [])
audio = [m for m in models
         if "audio" in ((m.get("architecture") or {}).get("input_modalities") or [])]
def slim(m):
    return {
        "id": m["id"],
        "name": m.get("name", ""),
        "context_length": m.get("context_length"),
        "architecture": {
            "input_modalities": (m.get("architecture") or {}).get("input_modalities", []),
            "output_modalities": (m.get("architecture") or {}).get("output_modalities", []),
        },
        "pricing": {k: v for k, v in (m.get("pricing") or {}).items() if isinstance(v, str)},
    }
print(json.dumps({
    "_source": "GET https://openrouter.ai/api/v1/models (no credential required)",
    "_captured_on": __import__("datetime").date.today().isoformat(),
    "_note": "Sanitized A1.2 evidence snapshot. No credential, header or request body is present.",
    "_total_models_in_catalog": len(models),
    "_models_declaring_audio_input": len(audio),
    "data": [slim(m) for m in sorted(audio, key=lambda x: x["id"])],
}, indent=2))
PY
}

case "$MODE" in
  catalog)
    OUT="${2:-tests/fixtures/openrouter/catalog-snapshot.json}"
    TMP="$(mktemp)"
    trap 'rm -f "$TMP"' EXIT
    curl --fail --silent --show-error --max-time 60 "$API_BASE/models" -o "$TMP"
    sanitize "$TMP" > "$OUT"
    echo "wrote $OUT"
    ;;

  exchange)
    MODEL="${2:?usage: probe-openrouter.sh exchange <model-id> <audio-file>}"
    AUDIO="${3:?usage: probe-openrouter.sh exchange <model-id> <audio-file>}"
    : "${OPENROUTER_API_KEY:?OPENROUTER_API_KEY must be set for an exchange probe}"
    MAX_BYTES="${PROBE_MAX_BYTES:-5242880}"

    SIZE=$(wc -c < "$AUDIO" | tr -d ' ')
    if [ "$SIZE" -gt "$MAX_BYTES" ]; then
      echo "refusing to upload $SIZE bytes; cost cap is $MAX_BYTES (set PROBE_MAX_BYTES to raise)" >&2
      exit 2
    fi

    HEADERS="$(mktemp)"; BODY="$(mktemp)"
    trap 'rm -f "$HEADERS" "$BODY"' EXIT

    # One request only. No retry: a retransmission can be billed twice.
    curl --silent --show-error --max-time 300 --no-keepalive \
      --dump-header "$HEADERS" \
      --output "$BODY" \
      --write-out 'status=%{http_code}\n' \
      -H "Authorization: Bearer ${OPENROUTER_API_KEY}" \
      -F "model=${MODEL}" \
      -F "file=@${AUDIO}" \
      "$API_BASE/audio/transcriptions"

    echo "--- provider route (header names only) ---"
    grep -iE '^(x-openrouter|server|content-type):' "$HEADERS" || true
    echo "--- response field names (no values) ---"
    python3 - "$BODY" <<'PY'
import json, sys
try:
    body = json.load(open(sys.argv[1]))
except Exception as exc:          # noqa: BLE001 - diagnostic only
    print(f"body is not JSON: {type(exc).__name__}")
    raise SystemExit(0)
def walk(node, prefix=""):
    if isinstance(node, dict):
        for key in sorted(node):
            print(f"{prefix}{key}")
            walk(node[key], prefix=f"{prefix}{key}.")
    elif isinstance(node, list) and node:
        walk(node[0], prefix=f"{prefix}0.")
walk(body)
PY
    echo "--- record the result in docs/spikes/A1.4 style and schemas/openrouter-model-record.v1.schema.json ---"
    ;;

  *)
    echo "usage: $0 {catalog|exchange} ..." >&2
    exit 64
    ;;
esac
