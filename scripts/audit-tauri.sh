#!/usr/bin/env bash
# Tauri least-privilege audit (§2.4, §6.3A, §15.1, §18.5).
#
# Builds and runs `plaudon-tauri-audit`, writing the §24 release-evidence
# artifact to target/security-evidence/tauri-audit.json and exiting non-zero on
# any finding.
#
# Usage:
#   scripts/audit-tauri.sh [--allow-missing-app] [extra auditor args…]
#
# Exit codes are the auditor's own:
#   0 pass · 1 findings · 2 usage · 3 apps/desktop/src-tauri absent · 4 internal
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
evidence_dir="${repo_root}/target/security-evidence"
report="${evidence_dir}/tauri-audit.json"

mkdir -p "${evidence_dir}"

cargo build --quiet --manifest-path "${repo_root}/Cargo.toml" \
  -p plaudon-security --bin plaudon-tauri-audit

set +e
cargo run --quiet --manifest-path "${repo_root}/Cargo.toml" \
  -p plaudon-security --bin plaudon-tauri-audit -- \
  --root "${repo_root}" --format json --output "${report}" "$@"
status=$?
set -e

echo "tauri audit report: ${report}"

case "${status}" in
  0) echo "tauri least-privilege audit: PASS" ;;
  1) echo "tauri least-privilege audit: FAIL (findings above)" >&2 ;;
  3) echo "tauri least-privilege audit: NOT VERIFIABLE (apps/desktop/src-tauri has no tauri.conf.json yet; pass --allow-missing-app to audit the manifest alone)" >&2 ;;
  *) echo "tauri least-privilege audit: ERROR (exit ${status})" >&2 ;;
esac

exit "${status}"
