#!/usr/bin/env bash
# Local-only Mode network-deny acceptance (§16.2A, §1.5, §18.6A, §22(15), §24).
#
# Runs the D2.3 egress harness, which:
#   * injects tests/egress/monitor into the scenario process
#     (DYLD_INSERT_LIBRARIES on macOS, LD_PRELOAD on Linux),
#   * records and refuses every non-loopback connect/resolve,
#   * records every process execution,
#   * asserts zero app-initiated traffic for every Local-only scenario,
#   * asserts the deliberate negative control IS detected, and
#   * writes target/security-evidence/egress-trace.json.
#
# Hermetic: no network access is required or used. Every destination is in a
# reserved documentation range (RFC 5737) or an .invalid domain (RFC 6761).
#
# Usage:
#   scripts/egress-deny-test.sh [extra cargo test args…]
#
# Exit codes:
#   0 pass · 1 acceptance failure · 2 evidence artifact missing
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-${repo_root}/target}"
evidence_dir="${target_dir}/security-evidence"
artifact="${evidence_dir}/egress-trace.json"

mkdir -p "${evidence_dir}"

# The monitor's own unit tests run with the hooks compiled in. They are a
# separate cargo invocation on purpose: within one invocation cargo unifies
# features, which would also link the hooks into the harness test binary and
# let it interpose its own libc calls.
echo "egress deny test: monitor unit tests"
set +e
cargo test --manifest-path "${repo_root}/Cargo.toml" \
  -p plaudon-egress-monitor --features interpose
monitor_status=$?
set -e

echo "egress deny test: building and running plaudon-egress-harness"
set +e
cargo test --manifest-path "${repo_root}/Cargo.toml" \
  -p plaudon-egress-scenario -p plaudon-egress-harness \
  -- --nocapture "$@"
status=$?
set -e
if [[ ${monitor_status} -ne 0 ]]; then
  status=${monitor_status}
fi

if [[ ! -f "${artifact}" ]]; then
  echo "egress deny test: ERROR — no evidence artifact at ${artifact}" >&2
  echo "The harness writes it on every run, pass or fail; its absence means the" >&2
  echo "harness did not execute, which is a failure and not a skip." >&2
  exit 2
fi

echo "egress evidence artifact: ${artifact}"

if command -v python3 >/dev/null 2>&1; then
  python3 - "${artifact}" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    bundle = json.load(handle)

print(f"monitor: {bundle['monitor']}  platform: {bundle['platform']}")
for scenario in bundle["scenarios"]:
    verdict = "FAIL" if scenario["failure"] else "ok"
    print(
        f"  {scenario['scenario']:<20} {verdict:<4} "
        f"injected={scenario['monitor_injected']} "
        f"non_loopback_attempts={scenario['non_loopback_attempt_count']} "
        f"exec_records={len(scenario['exec_records'])}"
    )
PY
fi

if [[ ${status} -ne 0 ]]; then
  echo "egress deny test: FAIL" >&2
  exit 1
fi

echo "egress deny test: PASS"
echo
echo "Scope of this result: it covers traffic initiated by the scenario process."
echo "It says nothing about the operating system, other applications, platform"
echo "stores, or a cloud-sync client acting on a user-selected folder."
echo "For an independent packet-level check, run scripts/egress-tcpdump.sh"
echo "(privileged, opt-in)."
