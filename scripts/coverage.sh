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
# Prerequisites:
#
#   * cargo-llvm-cov:  cargo install cargo-llvm-cov --locked --version ^0.6
#   * a nightly toolchain with llvm-tools:
#         rustup toolchain install nightly --component llvm-tools-preview
#
# WHY NIGHTLY, when rust-toolchain.toml pins stable. Spec 18.9 is a *branch*
# coverage policy, and LLVM branch instrumentation is still gated behind
# `-Z coverage-options=branch`, which stable rustc refuses. Measuring lines
# instead would be worse than useless here: `cargo llvm-cov` without --branch
# reports every file as having zero branches, and `plaudon-release coverage`
# scores a file with no branches as fully covered — the gate would pass
# vacuously while measuring nothing. So the measurement, and only the
# measurement, runs on nightly; everything the project ships is still built
# and tested by the pinned stable toolchain. Override the toolchain name with
# PLAUDON_COVERAGE_TOOLCHAIN (e.g. a dated nightly) if `nightly` is broken.
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
  echo "cargo-llvm-cov is not installed: cargo install cargo-llvm-cov --locked --version ^0.6" >&2
  exit 127
fi

TOOLCHAIN="${PLAUDON_COVERAGE_TOOLCHAIN:-nightly}"
if ! cargo "+$TOOLCHAIN" --version >/dev/null 2>&1; then
  cat >&2 <<EOF
branch coverage needs the \`$TOOLCHAIN\` toolchain, which is not installed:
    rustup toolchain install $TOOLCHAIN --component llvm-tools-preview
(-Z coverage-options=branch is nightly-only; see the comment at the top of
this script for why line coverage is not an acceptable substitute.)
EOF
  exit 127
fi
if ! rustup component list --toolchain "$TOOLCHAIN" 2>/dev/null |
  grep -q '^llvm-tools.*(installed)'; then
  echo "the $TOOLCHAIN toolchain has no llvm-tools: rustup component add llvm-tools-preview --toolchain $TOOLCHAIN" >&2
  exit 127
fi

mkdir -p "$(dirname "$OUT")"

# cargo-llvm-cov 0.6 collects the object files it hands to `llvm-cov export`
# from <target>/<profile>/deps. Nightly cargo now writes test binaries to
# <profile>/build/<package>/<hash>/out instead, and the mismatch does not
# fail loudly: the few artifacts still hard-linked at the top of <profile>
# are found, every other object is missed, and the files their tests cover
# are reported as 0% — measured here as crates/security/src/local_only.rs
# scoring 0.00% while all 213 of plaudon-security's unit tests passed. A
# coverage gate that reports zeroes for code that is tested is worse than no
# gate, so the measurement asks for the layout its tooling understands.
export CARGO_UNSTABLE_BUILD_DIR_NEW_LAYOUT=false

echo "==> measuring branch coverage with $(cargo "+$TOOLCHAIN" --version)"

# The Apple bridge links a Swift static library that is not built in a default
# checkout, so it is excluded here exactly as it is in the test job.
#
# `--all-features` also turns on plaudon-ai-speaker's `onnx` feature, which
# links a vendored ONNX Runtime through ORT_LIB_LOCATION (ADR-0081 §3). That
# archive is 83 MB and is not in the repository, so a machine that has not
# fetched it cannot link the crate and `ort-sys` fails the link with its own
# "could not link to ONNX Runtime" message. The test and clippy checks already
# exclude it (scripts/pre-commit.sh, `exclude_feature_tainted`); this run
# excludes it for the same reason, and the crate keeps its own opt-in check.
cargo "+$TOOLCHAIN" llvm-cov --workspace --all-features --branch \
  --exclude plaudon-ai-apple \
  --exclude plaudon-ai-speaker \
  --json --output-path "$OUT"

# A summary for the log; the gate reads the JSON.
cargo "+$TOOLCHAIN" llvm-cov report --branch --summary-only || true

echo "branch coverage written to $OUT"
echo "enforce the policy with: plaudon-release coverage --llvm-cov $OUT"
