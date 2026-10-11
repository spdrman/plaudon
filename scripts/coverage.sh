#!/usr/bin/env bash
# Branch-coverage gate for the persistence, archive-accounting and policy
# modules (spec 18.9: at least 90% meaningful branch coverage there).
#
# Requires cargo-llvm-cov: cargo install cargo-llvm-cov
set -euo pipefail
cd "$(dirname "$0")/.."

THRESHOLD="${COVERAGE_THRESHOLD:-90}"
PACKAGES=(plaudon-persistence plaudon-app-core plaudon-domain)

ARGS=()
for package in "${PACKAGES[@]}"; do
  ARGS+=(-p "$package")
done

cargo llvm-cov --workspace --all-targets --branch --summary-only \
  --fail-under-functions "$THRESHOLD" "${ARGS[@]}"
