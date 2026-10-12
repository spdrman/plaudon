#!/usr/bin/env bash
# EPIC D security gates, in one entry point.
#
# This is what a CI job should call to enforce the spec §16 controls. Every
# step is hermetic: no network, no real cloud service, no real device, no
# elevated privileges. The two checks that cannot be hermetic — the privileged
# packet capture (scripts/egress-tcpdump.sh) and the extended fuzz run
# (scripts/fuzz.sh) — are deliberately NOT invoked here; they are scheduled or
# release-time steps and say so themselves when run.
#
# Exit status is non-zero if any gate fails. Evidence artifacts for the §24
# release bundle are written under target/security-evidence/.
set -euo pipefail
cd "$(dirname "$0")/.."

failed=()

step() {
    local name="$1"
    shift
    printf '\n\033[1m==> %s\033[0m\n' "$name"
    if "$@"; then
        printf '    PASS: %s\n' "$name"
    else
        local code=$?
        printf '    FAIL (%d): %s\n' "$code" "$name"
        failed+=("$name")
    fi
}

# 1. The code must build and its tests must pass before any gate means anything.
step "unit and integration tests (workspace)" \
    cargo test --workspace --all-features

# 2. Lints are part of the security posture: clippy::all is deny-level and
#    catches, among other things, the byte-slicing and allocation mistakes that
#    the fuzz targets exist to find.
step "clippy (-D warnings)" \
    cargo clippy --workspace --all-targets --all-features -- -D warnings

step "rustfmt" \
    cargo fmt --all --check

# 3. §1.5 release target: 0 secrets in application logs under automated
#    scanning. Also scans snapshots, crash reports and SQLite fixtures.
step "secret and signed-URL scanner (§1.5, §18.5)" \
    ./scripts/secret-scan.sh

# 4. §16.2A / §22(15): zero app-initiated traffic in Local-only Mode, observed
#    at the syscall boundary, with a negative control proving the fixture can
#    fail. Writes target/security-evidence/egress-trace.json.
step "network-deny egress fixture (§16.2A, §22.15)" \
    ./scripts/egress-deny-test.sh

# 5. §18.5 / §16.2: the fuzz targets must build and the committed crash and
#    hostile-input corpora must replay cleanly, on a stable toolchain.
step "fuzz corpus replay and sanitizer pass (§18.5)" \
    ./scripts/fuzz-ci.sh

# 6. §16.2 / §22.8: every threat row maps to an owned control and a named test,
#    or to an explicit open finding.
step "threat-model traceability (§16.2, §22.8)" \
    python3 scripts/check-threat-model.py

printf '\n'
if ((${#failed[@]} > 0)); then
    printf '\033[1;31mSECURITY GATES FAILED (%d):\033[0m\n' "${#failed[@]}"
    printf '  - %s\n' "${failed[@]}"
    exit 1
fi

printf '\033[1;32mAll EPIC D security gates passed.\033[0m\n'
printf 'Evidence artifacts: target/security-evidence/\n'
printf '\n'
printf 'Not covered by this run, by design:\n'
printf '  - privileged packet capture .......... sudo ./scripts/egress-tcpdump.sh <scenario>\n'
printf '  - extended fuzzing (scheduled) ....... ./scripts/fuzz.sh --duration 1800\n'
printf '  - real OS credential store ........... cargo test -p plaudon-security --features real-keychain -- --ignored\n'
printf '  - real hardware device paths ......... out of scope for EPIC D\n'
