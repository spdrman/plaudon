#!/usr/bin/env bash
# N2.3 — collect the release evidence bundle from CI artifacts (spec 24, 18.8).
#
# The assembler (`plaudon-release evidence assemble`) refuses an unknown
# category, an empty required category, a secret-bearing artifact and a dirty
# working tree. This script only populates the staging directory; it invents
# no values and copies nothing it did not find on disk.
#
#   scripts/collect-evidence.sh --staging <dir> [--artifacts <dir>] [--sign]
#
# Expects, where available:
#   ci-artifacts/build-metadata/*.json      from the CI matrix legs
#   ci-artifacts/sbom/sbom.json             from scripts/sbom.sh
#   ci-artifacts/coverage/                  from scripts/coverage.sh
#   ci-artifacts/scenario-coverage/         from `plaudon-release scenarios --json`
#   <staging>/signing-verification/         from the packaging scripts
#   <staging>/release-gate/                 from scripts/release-gate.sh
set -euo pipefail
cd "$(dirname "$0")/.."
REPO_ROOT="$PWD"

STAGING="$REPO_ROOT/ci-artifacts/evidence"
ARTIFACTS="$REPO_ROOT/ci-artifacts"
SIGN=0
while [ $# -gt 0 ]; do
  case "$1" in
    --staging) STAGING="$2"; shift 2 ;;
    --artifacts) ARTIFACTS="$2"; shift 2 ;;
    --sign) SIGN=1; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

mkdir -p "$STAGING"
copy_into() { # category source...
  local category="$1"; shift
  mkdir -p "$STAGING/$category"
  local copied=0
  for source in "$@"; do
    if [ -e "$source" ]; then
      cp -R "$source" "$STAGING/$category/"
      copied=1
    fi
  done
  return $(( copied == 1 ? 0 : 1 ))
}

echo "==> build metadata"
copy_into build-metadata "$ARTIFACTS"/build-metadata/*.json || {
  cargo run -q -p plaudon-release-tools --bin plaudon-release -- \
    build-metadata --out "$STAGING/build-metadata/$(uname -s)-$(uname -m).json"
}

echo "==> tested OS versions"
mkdir -p "$STAGING/tested-os-versions"
{
  echo "{"
  echo "  \"host_os\": \"$(uname -s)\","
  echo "  \"host_arch\": \"$(uname -m)\","
  if [ "$(uname -s)" = "Darwin" ]; then
    echo "  \"os_version\": \"$(sw_vers -productVersion)\","
    echo "  \"os_build\": \"$(sw_vers -buildVersion)\","
  else
    echo "  \"os_version\": \"$(uname -r)\","
    echo "  \"os_build\": \"$(uname -v | tr -d '"')\","
  fi
  echo "  \"commit\": \"$(git rev-parse HEAD)\""
  echo "}"
} > "$STAGING/tested-os-versions/$(uname -s)-$(uname -m).json"

echo "==> test results"
mkdir -p "$STAGING/test-results"
cargo test --workspace --all-features --exclude plaudon-ai-apple --no-fail-fast 2>&1 \
  | tee "$STAGING/test-results/workspace.log" \
  | grep -E "^test result|^error" || true

echo "==> sbom"
copy_into sbom "$ARTIFACTS/sbom/sbom.json" || scripts/sbom.sh --out "$STAGING/sbom/sbom.json"

echo "==> software composition and licence audit"
mkdir -p "$STAGING/sca-license-audit"
if command -v cargo-deny >/dev/null 2>&1; then
  cargo deny --all-features check licenses bans sources advisories \
    > "$STAGING/sca-license-audit/cargo-deny.log" 2>&1 \
    || echo "cargo-deny reported findings; see the log" >&2
else
  echo "reason=missing_tool: cargo-deny is not installed; the licence audit was not run" \
    > "$STAGING/sca-license-audit/cargo-deny.log"
fi
cp docs/licensing/third-party-register.md "$STAGING/sca-license-audit/" 2>/dev/null || true

echo "==> risk exceptions"
mkdir -p "$STAGING/risk-exceptions"
cp docs/release/risk-exceptions.md "$STAGING/risk-exceptions/" 2>/dev/null || \
  echo "no accepted risk exception is open for this release" > "$STAGING/risk-exceptions/none.txt"

echo "==> migration and restore results"
mkdir -p "$STAGING/migration-restore"
cargo test -p plaudon-persistence --all-features 2>&1 \
  | tee "$STAGING/migration-restore/migrations.log" | grep -E "^test result" || true

echo "==> OpenRouter contract fixtures"
mkdir -p "$STAGING/openrouter-contract-fixtures"
cp -R tests/fixtures/openrouter/. "$STAGING/openrouter-contract-fixtures/" 2>/dev/null || true
cp docs/spikes/A1.2-openrouter-contract.md "$STAGING/openrouter-contract-fixtures/" 2>/dev/null || true

echo "==> offline model artifacts"
mkdir -p "$STAGING/offline-model-artifacts"
cp docs/licensing/third-party-register.md "$STAGING/offline-model-artifacts/model-licences.md" 2>/dev/null || true
if [ -n "${PLAUDON_WHISPER_MODEL:-}" ] && [ -f "$PLAUDON_WHISPER_MODEL" ]; then
  printf '{"model_path":"%s","sha256":"%s"}\n' \
    "$(basename "$PLAUDON_WHISPER_MODEL")" \
    "$(shasum -a 256 "$PLAUDON_WHISPER_MODEL" | cut -d' ' -f1)" \
    > "$STAGING/offline-model-artifacts/checksum.json"
else
  echo '{"status":"not_verified","reason":"no offline model artifact was present in this environment"}' \
    > "$STAGING/offline-model-artifacts/checksum.json"
fi

echo "==> security findings"
mkdir -p "$STAGING/security-findings"
cp -R docs/threat-model/. "$STAGING/security-findings/" 2>/dev/null || true

echo "==> consent and local-only egress traces"
mkdir -p "$STAGING/consent-egress-traces"
if [ -x scripts/egress-deny-test.sh ]; then
  scripts/egress-deny-test.sh > "$STAGING/consent-egress-traces/egress-deny.log" 2>&1 || true
fi
cargo test -p plaudon-egress-harness --quiet > "$STAGING/consent-egress-traces/harness.log" 2>&1 || true
cp -R ci-artifacts/egress/. "$STAGING/consent-egress-traces/" 2>/dev/null || true

echo "==> device compatibility records"
mkdir -p "$STAGING/device-compatibility"
cp compatibility.yaml "$STAGING/device-compatibility/"
cargo run -q -p plaudon-release-tools --bin plaudon-release -- compat-matrix \
  --out "$STAGING/device-compatibility/compatibility.md"
# One record per `Supported` cell, derived from the acceptance run that backs
# it. The gate in `plaudon-release evidence gate` fails when a claim has no
# matching record, so this step is what makes a Supported cell publishable.
ACCEPTANCE_LOG="$STAGING/test-results/acceptance.log"
cargo test -p plaudon-e2e --all-features > "$ACCEPTANCE_LOG" 2>&1 || true
cargo run -q -p plaudon-release-tools --bin plaudon-release -- compat-evidence \
  --from "$ACCEPTANCE_LOG" --out "$STAGING/device-compatibility"


echo "==> frozen quality thresholds"
mkdir -p "$STAGING/frozen-thresholds"
cp tests/scale/thresholds.toml "$STAGING/frozen-thresholds/" 2>/dev/null || true
cp coverage-policy.toml "$STAGING/frozen-thresholds/"
cp docs/release/frozen-thresholds.md "$STAGING/frozen-thresholds/" 2>/dev/null || true

echo "==> signing verification"
mkdir -p "$STAGING/signing-verification"
[ -d "$STAGING/signing-verification" ] && [ -z "$(ls -A "$STAGING/signing-verification")" ] && \
  echo '{"status":"not_verified","reason":"no packaging run fed this bundle; run scripts/package-macos-egui.sh --evidence (or scripts/package-macos.sh for the Tauri shell)"}' \
    > "$STAGING/signing-verification/missing.json"

echo "==> scenario coverage"
mkdir -p "$STAGING/scenario-coverage"
cargo run -q -p plaudon-release-tools --bin plaudon-release -- scenarios --json \
  > "$STAGING/scenario-coverage/spec-18.7.json"

echo "==> coverage report"
copy_into coverage-report "$ARTIFACTS"/coverage/* || \
  echo '{"status":"not_verified","reason":"scripts/coverage.sh was not run in this environment"}' \
    > "$STAGING/coverage-report/missing.json"

echo "==> benchmarks"
mkdir -p "$STAGING/benchmarks"
cp -R docs/benchmarks/. "$STAGING/benchmarks/" 2>/dev/null || true

echo "==> quality metrics (optional)"
if [ -x scripts/run-quality-metrics.sh ]; then
  mkdir -p "$STAGING/quality-metrics"
  scripts/run-quality-metrics.sh --out "$STAGING/quality-metrics/metrics.json" || true
fi

echo "==> release gate"
mkdir -p "$STAGING/release-gate"
[ -z "$(ls -A "$STAGING/release-gate")" ] && \
  echo '{"status":"not_verified","reason":"scripts/release-gate.sh was not run against a built artifact"}' \
    > "$STAGING/release-gate/missing.json"

if [ "$SIGN" = "1" ]; then
  echo "==> assembling and signing"
  cargo run -q -p plaudon-release-tools --bin plaudon-release -- evidence assemble --staging "$STAGING"
else
  echo "staging populated at $STAGING"
  echo "sign it with: scripts/collect-evidence.sh --staging $STAGING --sign"
fi
