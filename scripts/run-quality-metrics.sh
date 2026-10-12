#!/usr/bin/env bash
# Run hermetic quality-metric tests and emit the JSON document for the release
# evidence bundle (issue M2.4, contract with Team-N).
#
# Validates the emitted document against schemas/quality-metrics.v1.schema.json.
# Prints nothing to stdout except errors. Exits non-zero if the document fails
# schema validation.
#
# Usage:
#   scripts/run-quality-metrics.sh --out <path>
#   scripts/run-quality-metrics.sh --help
set -euo pipefail
cd "$(dirname "$0")/.."

OUT=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out)
      OUT="$2"
      shift 2
      ;;
    --help)
      echo "Usage: $0 --out <path>"
      echo ""
      echo "Run quality metrics tests and write JSON to <path>."
      echo "Validates output against schemas/quality-metrics.v1.schema.json"
      exit 0
      ;;
    *)
      echo "Error: Unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [[ -z "$OUT" ]]; then
  echo "Error: --out is required" >&2
  exit 2
fi

# Spec §17.4: run the hermetic test suites that produce the metrics
echo "Running diarization and tool quality tests..." >&2
cargo test -p plaudon-ai-diarization -p plaudon-ai-tools --lib --quiet

# The emitter binary (MetricsHarness agent is creating it)
EMITTER_BIN="crates/ai-diarization/src/bin/quality_metrics.rs"

if [[ ! -f "$EMITTER_BIN" ]]; then
  echo "Error: Metrics emitter binary not yet available at $EMITTER_BIN" >&2
  echo "       (MetricsHarness agent is creating it)" >&2
  exit 1
fi

# Create parent directory if needed
mkdir -p "$(dirname "$OUT")"

# Run the emitter to generate the JSON document
echo "Generating quality metrics document..." >&2
cargo run -q -p plaudon-ai-diarization --bin quality_metrics -- --out "$OUT"

if [[ ! -f "$OUT" ]]; then
  echo "Error: Emitter did not produce output at $OUT" >&2
  exit 1
fi

echo "Validating against schema..." >&2

# Validate the output - try the dedicated test first if it exists
SCHEMA_TEST="crates/ai-diarization/tests/quality_metrics_schema.rs"

if [[ -f "$SCHEMA_TEST" ]]; then
  # Run the Rust test that validates the schema
  QUALITY_METRICS_OUTPUT="$OUT" cargo test -p plaudon-ai-diarization \
    --test quality_metrics_schema --quiet
else
  # Fallback to Python jsonschema if available
  if python3 -c "import jsonschema" 2>/dev/null; then
    python3 - "$OUT" <<'PY'
import json, sys
try:
    import jsonschema
except ImportError:
    print("Error: jsonschema Python module not available", file=sys.stderr)
    sys.exit(1)

schema_path = "schemas/quality-metrics.v1.schema.json"
output_path = sys.argv[1]

with open(schema_path) as f:
    schema = json.load(f)

with open(output_path) as f:
    instance = json.load(f)

try:
    jsonschema.validate(instance, schema)
except jsonschema.ValidationError as e:
    print(f"Error: Output failed schema validation: {e}", file=sys.stderr)
    sys.exit(1)
PY
  else
    echo "Error: Cannot validate schema - neither Rust test nor Python jsonschema available" >&2
    exit 1
  fi
fi

echo "Quality metrics document written to $OUT" >&2
