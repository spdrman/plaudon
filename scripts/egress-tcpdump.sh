#!/usr/bin/env bash
# Opt-in, privileged packet-level verification of Local-only Mode
# (§16.2A, §1.5, §24 release evidence bundle).
#
# This is the independent check that sits beside the in-process monitor:
# tcpdump watches the host's interfaces while one scenario runs, and the
# capture is then re-read to assert that the scenario sent no packet to a
# non-loopback, non-link-local destination.
#
# By default the in-process monitor is NOT injected, so what is measured is the
# application's own `plaudon_security::outbound` gate, observed from outside
# the process. Pass --with-monitor to capture with the monitor injected as well.
#
# This script is never run by `cargo test`: it needs root, it writes a pcap,
# and (for the negative control) it deliberately emits a packet.
#
# Usage:
#   sudo scripts/egress-tcpdump.sh [scenario] [--with-monitor] [--keep-running]
#
# Scenarios: see `plaudon-egress-scenario --list`. Default: openrouter-dispatch.
# Running `negative-control` is the self-test of this script: it is expected to
# produce packets, and the script fails if it does not.
#
# Exit codes:
#   0 pass · 1 unexpected packets (or none from the negative control)
#   2 usage · 3 insufficient privileges · 4 tooling or capture failure
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-${repo_root}/target}"
evidence_dir="${target_dir}/security-evidence"

scenario="openrouter-dispatch"
with_monitor=0
for arg in "$@"; do
  case "${arg}" in
    --with-monitor) with_monitor=1 ;;
    --help|-h)
      sed -n '2,25p' "${BASH_SOURCE[0]}"
      exit 0
      ;;
    -*)
      echo "unknown option: ${arg}" >&2
      exit 2
      ;;
    *) scenario="${arg}" ;;
  esac
done

# ---------------------------------------------------------------------------
# Preconditions. Every failure below is loud: this script never skips quietly,
# because "we did not look" must never be recorded as "nothing happened".
# ---------------------------------------------------------------------------

if ! command -v tcpdump >/dev/null 2>&1; then
  echo "egress tcpdump: ERROR — tcpdump is not installed." >&2
  echo "Install it and re-run; a missing capture tool is not a pass." >&2
  exit 4
fi

if [[ "$(id -u)" -ne 0 ]]; then
  echo "egress tcpdump: ERROR — packet capture needs elevated privileges." >&2
  echo "This check did NOT run, and nothing about app egress was verified by it." >&2
  echo "Re-run as:  sudo ${BASH_SOURCE[0]} ${scenario}" >&2
  echo "(The unprivileged, hermetic check is scripts/egress-deny-test.sh.)" >&2
  exit 3
fi

os="$(uname -s)"
case "${os}" in
  Darwin)
    capture_iface="pktap,all"
    # pcapng keeps the per-packet process metadata that makes attribution real.
    capture_format=(-P)
    pcap_ext="pcapng"
    ;;
  Linux)
    capture_iface="any"
    capture_format=()
    pcap_ext="pcap"
    ;;
  *)
    echo "egress tcpdump: ERROR — unsupported host OS '${os}'." >&2
    echo "No capture was performed; do not record this run as evidence." >&2
    exit 4
    ;;
esac

mkdir -p "${evidence_dir}"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
pcap="${evidence_dir}/egress-${scenario}-${stamp}.${pcap_ext}"
report="${evidence_dir}/egress-tcpdump-${scenario}.json"
run_dir="${target_dir}/egress-tcpdump/${scenario}"
rm -rf "${run_dir}"
mkdir -p "${run_dir}/work"

echo "egress tcpdump: building the scenario binary"
cargo build --quiet --manifest-path "${repo_root}/Cargo.toml" \
  -p plaudon-egress-scenario -p plaudon-egress-monitor \
  --features plaudon-egress-monitor/interpose

profile_dir="${target_dir}/debug"
scenario_bin="${profile_dir}/plaudon-egress-scenario"
if [[ ! -x "${scenario_bin}" ]]; then
  echo "egress tcpdump: ERROR — ${scenario_bin} was not produced." >&2
  exit 4
fi
if [[ "${os}" == "Darwin" ]]; then
  monitor_lib="${profile_dir}/libplaudon_egress_monitor.dylib"
  inject_var="DYLD_INSERT_LIBRARIES"
else
  monitor_lib="${profile_dir}/libplaudon_egress_monitor.so"
  inject_var="LD_PRELOAD"
fi

# Destinations that are not "the internet": loopback, link-local, multicast
# and broadcast. Anything else leaving the host is a finding.
exclude_filter='not (host 127.0.0.1 or host ::1 or net 169.254.0.0/16 or net 224.0.0.0/4 or host 255.255.255.255 or ip6 multicast or net fe80::/10)'

tcpdump_pid=""
cleanup() {
  if [[ -n "${tcpdump_pid}" ]] && kill -0 "${tcpdump_pid}" 2>/dev/null; then
    kill -INT "${tcpdump_pid}" 2>/dev/null || true
    wait "${tcpdump_pid}" 2>/dev/null || true
  fi
}
trap cleanup EXIT

echo "egress tcpdump: capturing on ${capture_iface} -> ${pcap}"
tcpdump -i "${capture_iface}" -n -U "${capture_format[@]}" -w "${pcap}" 'ip or ip6' \
  >"${run_dir}/tcpdump.log" 2>&1 &
tcpdump_pid=$!

# Wait for the capture to be live rather than guessing with a fixed sleep.
for _ in $(seq 1 50); do
  if [[ -s "${pcap}" ]]; then break; fi
  if ! kill -0 "${tcpdump_pid}" 2>/dev/null; then
    echo "egress tcpdump: ERROR — tcpdump exited during startup:" >&2
    cat "${run_dir}/tcpdump.log" >&2
    exit 4
  fi
  sleep 0.2
done
if [[ ! -s "${pcap}" ]]; then
  echo "egress tcpdump: ERROR — the capture file never opened; nothing was observed." >&2
  cat "${run_dir}/tcpdump.log" >&2
  exit 4
fi

echo "egress tcpdump: running scenario '${scenario}'"
scenario_env=(
  "PLAUDON_EGRESS_DECISIONS=${run_dir}/decisions.json"
  "PLAUDON_SCENARIO_WORKDIR=${run_dir}/work"
)
if [[ "${with_monitor}" -eq 1 ]]; then
  if [[ ! -f "${monitor_lib}" ]]; then
    echo "egress tcpdump: ERROR — ${monitor_lib} is missing; --with-monitor cannot run." >&2
    exit 4
  fi
  scenario_env+=("PLAUDON_EGRESS_LOG=${run_dir}/trace.jsonl" "${inject_var}=${monitor_lib}")
fi

set +e
env "${scenario_env[@]}" "${scenario_bin}" "${scenario}" \
  >"${run_dir}/scenario.out" 2>"${run_dir}/scenario.err" &
scenario_pid=$!
wait "${scenario_pid}"
scenario_status=$?
set -e
cat "${run_dir}/scenario.out"

# Let the kernel hand the last packets to the capture before stopping it.
sleep 1
cleanup
trap - EXIT
tcpdump_pid=""

# ---------------------------------------------------------------------------
# Post-process the capture.
# ---------------------------------------------------------------------------

packets_file="${run_dir}/packets.txt"
attribution="time-window"
if [[ "${os}" == "Darwin" ]]; then
  attribution="process-id (pktap metadata)"
  if ! tcpdump -r "${pcap}" -n -k NP -Q "pid = ${scenario_pid}" "${exclude_filter}" \
      >"${packets_file}" 2>"${run_dir}/read.log"; then
    echo "egress tcpdump: ERROR — could not re-read ${pcap}:" >&2
    cat "${run_dir}/read.log" >&2
    exit 4
  fi
else
  if ! tcpdump -r "${pcap}" -n "${exclude_filter}" \
      >"${packets_file}" 2>"${run_dir}/read.log"; then
    echo "egress tcpdump: ERROR — could not re-read ${pcap}:" >&2
    cat "${run_dir}/read.log" >&2
    exit 4
  fi
fi

packet_count="$(grep -c . "${packets_file}" || true)"
packet_count="${packet_count:-0}"

cat >"${report}" <<JSON
{
  "scenario": "${scenario}",
  "platform": "${os}",
  "capture_interface": "${capture_iface}",
  "pcap": "${pcap}",
  "monitor_injected": $([[ "${with_monitor}" -eq 1 ]] && echo true || echo false),
  "scenario_pid": ${scenario_pid},
  "scenario_exit_code": ${scenario_status},
  "attribution": "${attribution}",
  "non_local_packet_count": ${packet_count},
  "excluded_destinations": "loopback, link-local, multicast, broadcast",
  "limits": [
    "Observes packets on this host during the capture window only.",
    "Says nothing about the operating system, other applications, platform stores, or a cloud-sync client acting on a user-selected folder.",
    "On Linux the capture cannot be attributed to a process, so unrelated host traffic in the window appears here; on macOS pktap process metadata is used."
  ]
}
JSON

echo
echo "pcap:   ${pcap}"
echo "report: ${report}"
echo "attribution: ${attribution}"
echo "non-loopback, non-local packets attributed to the scenario: ${packet_count}"
if [[ "${packet_count}" -gt 0 ]]; then
  head -20 "${packets_file}"
fi

if [[ "${scenario}" == "negative-control" ]]; then
  if [[ "${packet_count}" -gt 0 ]]; then
    echo "egress tcpdump: PASS — the negative control produced packets, so this capture can detect traffic."
    exit 0
  fi
  echo "egress tcpdump: FAIL — the negative control produced no packets." >&2
  echo "The capture pipeline cannot see this host's traffic, so no other result from it is trustworthy." >&2
  exit 1
fi

if [[ "${scenario_status}" -ne 0 ]]; then
  echo "egress tcpdump: FAIL — the scenario itself exited ${scenario_status}; see ${run_dir}/scenario.err" >&2
  exit 1
fi

if [[ "${packet_count}" -gt 0 ]]; then
  echo "egress tcpdump: FAIL — ${packet_count} packet(s) left this host for a non-local destination." >&2
  exit 1
fi

echo "egress tcpdump: PASS — zero app-initiated non-local packets for '${scenario}'."
echo "Scope: this host, this capture window, this process. Nothing is claimed about"
echo "the operating system, other applications, platform stores, or a cloud-sync"
echo "client acting on a user-selected folder."
