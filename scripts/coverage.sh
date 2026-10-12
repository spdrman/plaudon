#!/usr/bin/env bash
# Branch-coverage measurement for the spec 18.9 policy gate.
#
# This script only MEASURES. The policy — which modules are critical, which
# are exempt, and the threshold — lives in coverage-policy.toml and is
# enforced by `plaudon-release coverage`, so a threshold change is a reviewable
# diff rather than a flag buried in a CI script.
#
#   scripts/coverage.sh [--json <path>]
#
# Requires cargo-llvm-cov: cargo install cargo-llvm-cov --locked
set -euo pipefail
cd "$(dirname "$0")/.."

OUT="ci-artifacts/coverage/llvm-cov.json"
while [ $# -gt 0 ]; do
  case "$1" in
    --json) OUT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if ! cargo llvm-cov --version >/dev/null 2>&1; then
  echo "cargo-llvm-cov is not installed: cargo install cargo-llvm-cov --locked" >&2
  exit 127
fi

mkdir -p "$(dirname "$OUT")"

# The Apple bridge links a Swift static library that is not built in a default
# checkout, so it is excluded here exactly as it is in the test job.
cargo llvm-cov --workspace --all-features --branch \
  --exclude plaudon-ai-apple \
  --json --output-path "$OUT"

# A summary for the log; the gate reads the JSON.
cargo llvm-cov report --branch --summary-only || true

echo "branch coverage written to $OUT"
echo "enforce the policy with: plaudon-release coverage --llvm-cov $OUT"
