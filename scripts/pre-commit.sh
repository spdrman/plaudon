#!/usr/bin/env bash
# Local pre-commit gate: the checks GitHub Actions used to run (#257).
#
# The three workflows (CI, Apple bridge, Extraction Evaluation) are disabled
# with `gh workflow disable`; their files are still in .github/workflows/ so
# they can be switched back on. Until then this script is the gate, so every
# job step that can run on a developer machine is reproduced here, named after
# the CI job it stands in for. docs/PRECOMMIT.md holds the mapping, including
# the four Windows-runner jobs that cannot be reproduced on macOS or Linux and
# are therefore NOT covered by anything here.
#
# Usage:
#   scripts/pre-commit.sh                 every check, fail-fast
#   scripts/pre-commit.sh --list          the registry: name, tier, runner, CI job
#   scripts/pre-commit.sh --only fmt,clippy
#   scripts/pre-commit.sh --skip coverage
#   scripts/pre-commit.sh --skip-slow     the seconds-long native gates only
#   scripts/pre-commit.sh --keep-going    run everything, report at the end
#
# Environment (how the git hook is configured, since `git commit` passes no
# flags):
#   PLAUDON_PRECOMMIT_ONLY=fmt,clippy
#   PLAUDON_PRECOMMIT_SKIP=coverage,package-macos
#   PLAUDON_PRECOMMIT_SKIP_SLOW=1
#   PLAUDON_PRECOMMIT_KEEP_GOING=1
#
# Anything skipped is printed, loudly, before and after the run: a partial run
# must never read like a full one. `git commit --no-verify` is always yours to
# use — it is not disabled, and it is not hidden either.
#
# Where things run: the workstation budget is 5 cores / 20 GB across all
# agents, so native cargo work is crate-scoped at CARGO_BUILD_JOBS=2 and every
# workspace-wide build, every renderer suite and the coverage run go through
# scripts/e2e-remote.sh (MARS Docker, falling back to a local arm64 container).
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT" || exit 1

# ci.yml's env block. PLAUDON_LOCAL_ONLY makes an accidental outbound call from
# application code fail loudly (spec 22(15)).
export PLAUDON_LOCAL_ONLY="${PLAUDON_LOCAL_ONLY:-1}"
export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"
export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"
# One workstation cargo process at -j 2 (docs/EGUI_BUILD.md §1, resource policy).
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"

# whisper-rs compiles C++ against libc++'s headers, which a GitHub macos-15
# image has on the default include path and a workstation with a separate
# Command Line Tools install does not (docs/EGUI_BUILD.md §1). Supplying them
# when the developer has not is the difference between a real failure and a
# confusing one; an existing value is never overwritten.
if [ "$(uname -s)" = "Darwin" ] && command -v xcrun >/dev/null 2>&1; then
  SDKROOT="${SDKROOT:-$(xcrun --show-sdk-path)}"
  export SDKROOT
  export CXXFLAGS="${CXXFLAGS:--isystem $SDKROOT/usr/include/c++/v1}"
fi

ARTIFACTS="$REPO_ROOT/target/precommit"
RELEASE_BIN=(cargo run -q -p plaudon-release-tools --bin plaudon-release --)

# --- the registry -----------------------------------------------------------
#
# name | tier | runner | CI workflow › job › step
#
# tier    fast  = seconds to a minute on a warm checkout; always runs
#         slow  = compiles or links something large, or goes to the runner
# runner  native       this macOS arm64 host
#         native-macos this host, and only a macOS host can do it at all
#         remote       scripts/e2e-remote.sh (Linux container, MARS or local)
#         remote-linux same, and Linux is the point of the check
#         both         split: the part that builds on Linux goes to the runner,
#                      the Apple-tainted crates stay on this Mac
CHECKS=$(
  cat <<'REGISTRY'
fmt|fast|native|CI › Rust (macos-15) › Formatting
compat-matrix|fast|native|CI › release-gates › Compatibility matrix is generated, not written
doc-lint|fast|native|CI › release-gates › Public documentation lint
scenarios|fast|native|CI › release-gates + acceptance › Spec 18.7 scenario registry
deny|fast|native|CI › supply-chain › Advisories, licences, bans and sources
licence-register|fast|native|CI › supply-chain › Model and third-party licence register
sbom|fast|native|CI › supply-chain › Software bill of materials
build-metadata|fast|native|CI › Rust (macos-15) › Reproducible build metadata
clippy|slow|both|CI › Rust (macos-15) › Clippy
tests|slow|both|CI › Rust (macos-15) › Unit, integration and contract tests
persistence|slow|remote|CI › Rust (macos-15) › Database migrations apply to a fresh database and reject drift
chaos|slow|remote|CI › Rust (macos-15) › Crash-recovery suite
egui-suites|slow|remote|CI › egui-desktop (macos-15) › UI, shell and packaging suites
acceptance|slow|remote|CI › acceptance (macos-15) › Acceptance scenarios
apple-hermetic|slow|remote-linux|Apple bridge › hermetic (all six steps)
apple-no-symbols|slow|remote-linux|Apple bridge › no-apple-symbols
apple-macos|slow|native-macos|Apple bridge › macos (Swift bridge, link probe, sanitizers, opt-in gate)
extraction-eval|slow|remote|Extraction Evaluation › evaluate › Run extraction evaluation
package-macos|slow|native-macos|CI › egui-desktop (macos-15) › Package the macOS bundle
onnx-embedder|slow|native-macos|no CI job: the ADR-0081 ONNX engine, opt-in (PLAUDON_ONNX_EMBEDDER=1)
ci-selftest|slow|native|CI › pipeline-selftest › Deliberately failing fixtures must fail their gates
coverage|slow|native-macos|CI › coverage › Branch coverage + Enforce the critical-module policy
REGISTRY
)

check_names() { printf '%s\n' "$CHECKS" | cut -d'|' -f1; }
check_field() { # name field-index
  printf '%s\n' "$CHECKS" | awk -F'|' -v n="$1" -v f="$2" '$1 == n { print $f }'
}

# --- runners ----------------------------------------------------------------

E2E="$REPO_ROOT/scripts/e2e-remote.sh"
# AccessKit's AT-SPI adapter and the keyring backend need libdbus at build time
# in the container image (docs/EGUI_TESTING.md).
export PLAUDON_E2E_APT_PACKAGES="${PLAUDON_E2E_APT_PACKAGES:-libdbus-1-dev}"

remote_cargo() { "$E2E" cargo "$@"; }
remote_shell() { "$E2E" shell "$1"; }

# scripts/e2e-remote.sh excludes .git/ from the rsync, so suites that inspect
# the committed tree (tests/integration/tests/workspace_layout.rs,
# tests/scale/src/results.rs, crates/ai-diarization quality-metrics schema)
# have no repository to look at and fail for the wrong reason. A throwaway
# snapshot commit inside the container gives them one.
GIT_SNAPSHOT='export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory GIT_CONFIG_VALUE_0=/work
if [ ! -d .git ]; then
  git init -q .
  git add -A
  git -c user.email=precommit@plaudon.invalid -c user.name=pre-commit \
      commit -q -m "pre-commit snapshot of the working tree"
fi'

require_tool() { # tool install-hint
  if ! command -v "$1" >/dev/null 2>&1; then
    printf 'the %s gate needs %s, which is not installed: %s\n' "$CURRENT_CHECK" "$1" "$2" >&2
    return 127
  fi
}

run_fmt() {
  cargo fmt --all --check
}

run_compat_matrix() {
  "${RELEASE_BIN[@]}" compat-matrix --check
}

run_doc_lint() {
  "${RELEASE_BIN[@]}" doc-lint
}

run_scenarios() {
  "${RELEASE_BIN[@]}" scenarios
}

run_deny() {
  require_tool cargo-deny "cargo install cargo-deny --locked --version ^0.18" || return 127
  cargo deny --all-features check licenses bans sources advisories
}

run_licence_register() {
  # Same invocation as the CI step. scripts/license-audit.sh currently ignores
  # its arguments and re-runs the cargo-deny gate; see docs/PRECOMMIT.md.
  scripts/license-audit.sh --register-only
}

run_sbom() {
  mkdir -p "$ARTIFACTS/sbom"
  scripts/sbom.sh --out "$ARTIFACTS/sbom/sbom.json"
}

run_build_metadata() {
  local arch
  arch="$(rustc -vV | sed -n 's/^host: //p')"
  mkdir -p "$ARTIFACTS/build-metadata"
  "${RELEASE_BIN[@]}" build-metadata --out "$ARTIFACTS/build-metadata/$arch.json"
  cat "$ARTIFACTS/build-metadata/$arch.json"
}

# `--all-features` turns on `apple-bridge`, whose build script refuses any
# non-macOS target on purpose (crates/ai-apple/build.rs) — and the feature is
# re-exported by plaudon-app, plaudon-egui and plaudon-hardware-tests, so
# `--exclude plaudon-ai-apple` does not switch it off. The whole
# `--all-features` workspace therefore cannot be built on the Linux runner at
# all. CI ran it on macos-15; here it is split in two so that nothing is left
# unchecked: everything that builds on Linux goes to the runner, and the four
# Apple-tainted crates are built on this Mac, crate-scoped at
# CARGO_BUILD_JOBS=2, which is what the workstation resource policy allows.
APPLE_TAINTED=(plaudon-ai-apple plaudon-app plaudon-egui plaudon-hardware-tests)
exclude_feature_tainted() {
  local p
  for p in "${APPLE_TAINTED[@]}"; do printf -- '--exclude %s ' "$p"; done
  # `--all-features` would also turn on plaudon-ai-speaker's `onnx` feature,
  # whose build script downloads a prebuilt ONNX Runtime over the network
  # (ADR-0081 §3). A gate that silently needs a CDN is not a gate, so the
  # crate is excluded here and checked on its own by `run_onnx_embedder`,
  # which is opt-in. Nothing depends on this crate with the feature on, so
  # one `--exclude` really does switch it off.
  printf -- '--exclude plaudon-ai-speaker '
}

# The off-by-default inference runtime (ADR-0081 §4). Opt-in, like the
# hardware suites: it needs the network on a cold cache and it is not part of
# any shipped configuration. Without the variable this check says what it
# skipped and why, rather than passing quietly.
run_onnx_embedder() {
  if [ "${PLAUDON_ONNX_EMBEDDER:-0}" != "1" ]; then
    echo "skipped: set PLAUDON_ONNX_EMBEDDER=1 to build and test the ADR-0081 ONNX engine"
    echo "  (off by default; its build downloads a prebuilt ONNX Runtime)"
    return 0
  fi
  cargo clippy -p plaudon-ai-speaker --all-targets --features onnx -- -D warnings || return $?
  cargo test -p plaudon-ai-speaker --all-targets --features onnx
}

run_clippy() {
  remote_shell "$GIT_SNAPSHOT
cargo clippy --workspace --all-targets --all-features $(exclude_feature_tainted) -- -D warnings" ||
    return $?
  echo "--- the four crates whose --all-features needs macOS, on this host at -j${CARGO_BUILD_JOBS}"
  cargo clippy -p plaudon-ai-apple -p plaudon-app -p plaudon-egui -p plaudon-hardware-tests \
    --all-targets --all-features -- -D warnings
}

# Suites that assert "this must NOT be writable" — tests/chaos' unwritable
# archive is the one that bites — only mean anything when the command runs as
# an ordinary user. scripts/e2e-remote.sh drops to this host's uid in local
# mode, but keeps the container's root on MARS, and root writes straight
# through a read-only directory: observed here as
# `an_unwritable_archive_fails_the_commit_without_registering_an_asset`
# passing locally and failing on MARS. An explicit PLAUDON_E2E_HOST is always
# obeyed; only the default is overridden.
unprivileged_host() {
  case "${PLAUDON_E2E_HOST:-auto}" in
    auto)
      echo "--- forcing PLAUDON_E2E_HOST=local: the permission assertions need an unprivileged uid" >&2
      echo local
      ;;
    *) echo "${PLAUDON_E2E_HOST}" ;;
  esac
}

run_tests() {
  # The Apple bridge links a macOS-only Swift static library; its probe suite
  # is opt-in (spec 18.3), so CI excludes plaudon-ai-apple here and the
  # apple-macos check covers it instead.
  PLAUDON_E2E_HOST="$(unprivileged_host)" \
    remote_shell "$GIT_SNAPSHOT
cargo test --workspace --all-features $(exclude_feature_tainted)" || return $?
  echo "--- plaudon-app and plaudon-hardware-tests with --all-features, on this host"
  cargo test -p plaudon-app -p plaudon-hardware-tests --all-features || return $?
  # plaudon-egui's suites drive a real renderer, which docs/EGUI_TESTING.md
  # keeps off workstations: they are *run* by the egui-suites check on the
  # runner, so all that is owed here is that they still build with every
  # feature on, including apple-bridge.
  echo "--- plaudon-egui --all-features builds (run by egui-suites on the runner)"
  cargo test -p plaudon-egui --all-features --no-run
}

run_persistence() {
  remote_cargo test -p plaudon-persistence --all-features
}

run_chaos() {
  PLAUDON_E2E_HOST="$(unprivileged_host)" \
    remote_cargo test -p plaudon-chaos-tests --all-features
}

run_egui_suites() {
  remote_cargo test \
    -p plaudon-egui -p plaudon-ui -p plaudon-ui-testkit \
    -p plaudon-platform -p plaudon-craft-ui
}

run_acceptance() {
  remote_shell "$GIT_SNAPSHOT
cargo run -q -p plaudon-release-tools --bin plaudon-release -- scenarios
cargo test -p plaudon-e2e --all-features -- --nocapture"
}

run_apple_hermetic() {
  # All six steps of the hermetic job, including the one that only means
  # anything off macOS: enabling the apple-bridge feature on a non-macOS target
  # must fail the build with an actionable message, not produce a
  # library-shaped nothing.
  remote_shell "$GIT_SNAPSHOT
export RUSTFLAGS='-D warnings'
cargo test -p plaudon-ai-apple --all-targets
cargo test -p plaudon-app-core --all-targets
cargo test -p plaudon-integration-tests --test h_apple_bridge
cargo test -p plaudon-chaos-tests --test apple_bridge_stress
cargo clippy -p plaudon-ai-apple -p plaudon-app-core --all-targets
if cargo check -p plaudon-ai-apple --features apple-bridge 2>build.log; then
  echo 'the apple-bridge feature built on Linux, which it must not' >&2
  exit 1
fi
grep -q 'can only be enabled for a macOS target' build.log"
}

run_apple_no_symbols() {
  remote_shell "export RUSTFLAGS='-D warnings'
cargo build -p plaudon-ai-apple --release
shopt -s nullglob
artifacts=(\"\$CARGO_TARGET_DIR\"/release/libplaudon_ai_apple.rlib \"\$CARGO_TARGET_DIR\"/release/deps/libplaudon_ai_apple-*.rlib)
if [ \"\${#artifacts[@]}\" -eq 0 ]; then
  echo 'no plaudon-ai-apple artifact was produced' >&2
  exit 1
fi
scripts/check-no-apple-symbols.sh \"\${artifacts[@]}\""
}

run_apple_macos() {
  [ "$(uname -s)" = "Darwin" ] || {
    echo "the macOS Swift bridge job needs a macOS host" >&2
    return 127
  }
  require_tool swiftc "install Xcode or the Command Line Tools" || return 127
  swiftc --version
  scripts/build-apple-bridge.sh build

  # The product supports Macs older than Apple's AI floor, so the binary must
  # load on them: a hard framework reference would make dyld refuse to launch
  # the whole application (task H1.2).
  local probe="$ARTIFACTS/link-probe"
  mkdir -p "$ARTIFACTS"
  clang -O1 -mmacosx-version-min=13.0 \
    -Inative/apple-bridge/include \
    -o "$probe" native/apple-bridge/tests/abi_stress.c \
    native/apple-bridge/build/libPlaudonAppleBridge.a \
    -L/usr/lib/swift -Wl,-rpath,/usr/lib/swift \
    -framework Foundation -framework AVFoundation -framework CoreMedia \
    -weak_framework Speech -weak_framework FoundationModels
  otool -l "$probe" | grep -A3 LC_BUILD_VERSION | grep -q "minos 13.0"
  otool -l "$probe" | grep -A2 LC_LOAD_WEAK_DYLIB | grep -q "Speech.framework"
  otool -l "$probe" | grep -A2 LC_LOAD_WEAK_DYLIB | grep -q "FoundationModels.framework"

  PLAUDON_APPLE_BRIDGE_LIB_DIR="$REPO_ROOT/native/apple-bridge/build" \
    RUSTFLAGS="-D warnings" \
    cargo test -p plaudon-ai-apple --features apple-bridge --all-targets

  # Hermetic: no audio fixture, so the transcription paths are skipped and no
  # Apple Intelligence or speech model is needed.
  scripts/apple-bridge-sanitizers.sh

  # Proves the gate rather than the hardware: without the opt-in variable every
  # hardware test must report itself as skipped.
  cargo test -p plaudon-hardware-tests --test apple_compatibility -- --nocapture \
    | tee "$ARTIFACTS/hardware.log"
  grep -q "PLAUDON_APPLE_HARDWARE_TESTS" "$ARTIFACTS/hardware.log"
}

run_extraction_eval() {
  remote_shell "$GIT_SNAPSHOT
bash scripts/run-extraction-eval.sh"
}

run_package_macos() {
  [ "$(uname -s)" = "Darwin" ] || {
    echo "the macOS packaging job needs a macOS host" >&2
    return 127
  }
  # Packaging without a credential is expected to exit 3 — "built and
  # verifiable, NOT publishable". Any other failure is a real one, and the
  # report must agree that it is unsigned.
  local status=0
  scripts/package-macos-egui.sh --evidence "$ARTIFACTS/evidence" || status=$?
  if [ "$status" != "3" ] && [ "$status" != "0" ]; then
    echo "packaging failed with $status" >&2
    return "$status"
  fi
  local report=dist/macos-egui/macos-signing-report.json
  grep -q '"publishable": false' "$report" || {
    echo "a workstation holds no Developer ID; a report claiming publishable is wrong" >&2
    return 1
  }
  grep -q '"notarization_status": "not_verified"' "$report"
}

run_ci_selftest() {
  require_tool cargo-deny "cargo install cargo-deny --locked --version ^0.18" || return 127
  scripts/ci-selftest.sh
}

run_coverage() {
  # CI runs this job on macos-15, and so does this check — for the same reason
  # clippy and tests are split: scripts/coverage.sh measures the workspace with
  # `--all-features`, which turns on apple-bridge through plaudon-app and
  # plaudon-egui, and that cannot be built on Linux. Instrumenting the
  # workspace is the single heaviest thing the gate does; CARGO_BUILD_JOBS=2
  # keeps it inside the workstation budget and makes it slow.
  require_tool cargo-llvm-cov "cargo install cargo-llvm-cov --locked --version ^0.6" || return 127
  scripts/coverage.sh --json "$ARTIFACTS/coverage/llvm-cov.json" || return $?
  "${RELEASE_BIN[@]}" coverage --llvm-cov "$ARTIFACTS/coverage/llvm-cov.json"
}

run_check() { # name
  case "$1" in
    fmt) run_fmt ;;
    compat-matrix) run_compat_matrix ;;
    doc-lint) run_doc_lint ;;
    scenarios) run_scenarios ;;
    deny) run_deny ;;
    licence-register) run_licence_register ;;
    sbom) run_sbom ;;
    build-metadata) run_build_metadata ;;
    clippy) run_clippy ;;
    tests) run_tests ;;
    persistence) run_persistence ;;
    chaos) run_chaos ;;
    egui-suites) run_egui_suites ;;
    acceptance) run_acceptance ;;
    apple-hermetic) run_apple_hermetic ;;
    apple-no-symbols) run_apple_no_symbols ;;
    apple-macos) run_apple_macos ;;
    extraction-eval) run_extraction_eval ;;
    package-macos) run_package_macos ;;
    onnx-embedder) run_onnx_embedder ;;
    ci-selftest) run_ci_selftest ;;
    coverage) run_coverage ;;
    *) echo "unknown check: $1" >&2; return 2 ;;
  esac
}

# --- argument handling ------------------------------------------------------

ONLY="${PLAUDON_PRECOMMIT_ONLY:-}"
SKIP="${PLAUDON_PRECOMMIT_SKIP:-}"
SKIP_SLOW="${PLAUDON_PRECOMMIT_SKIP_SLOW:-0}"
KEEP_GOING="${PLAUDON_PRECOMMIT_KEEP_GOING:-0}"
LIST=0
HOOK=0

usage() {
  sed -n '2,45p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//; s/^#$//'
}

while [ $# -gt 0 ]; do
  case "$1" in
    --list) LIST=1; shift ;;
    --only) ONLY="${ONLY:+$ONLY,}${2:-}"; shift 2 ;;
    --only=*) ONLY="${ONLY:+$ONLY,}${1#--only=}"; shift ;;
    --skip) SKIP="${SKIP:+$SKIP,}${2:-}"; shift 2 ;;
    --skip=*) SKIP="${SKIP:+$SKIP,}${1#--skip=}"; shift ;;
    --skip-slow) SKIP_SLOW=1; shift ;;
    --keep-going) KEEP_GOING=1; shift ;;
    --hook) HOOK=1; shift ;;
    -h | --help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

in_list() { # needle comma-list
  case ",$2," in *",$1,"*) return 0 ;; *) return 1 ;; esac
}

# Reject a typo rather than silently running a different set of checks.
for requested in $(printf '%s' "$ONLY,$SKIP" | tr ',' ' '); do
  [ -n "$requested" ] || continue
  if ! check_names | grep -qx -- "$requested"; then
    echo "unknown check: $requested (scripts/pre-commit.sh --list)" >&2
    exit 2
  fi
done

if [ "$LIST" = 1 ]; then
  printf '%-18s %-5s %-13s %s\n' CHECK TIER RUNNER "STANDS IN FOR"
  printf '%s\n' "$CHECKS" | while IFS='|' read -r name tier runner job; do
    [ -n "$name" ] || continue
    printf '%-18s %-5s %-13s %s\n' "$name" "$tier" "$runner" "$job"
  done
  cat <<'TAIL'

Not reproducible on this host, and not attempted by any check above:
  CI › Rust (windows-2025)             needs a native Windows x86-64 runner (spec 18.4)
  CI › egui-desktop (windows-2025)     rc.exe, NSIS, package-windows-egui.ps1
  CI › acceptance (windows-2025)       the acceptance suite on Windows
  Apple bridge › macos on a non-Mac    the Swift bridge needs a real macOS host
See docs/PRECOMMIT.md for the full mapping and how to re-enable GitHub CI.
TAIL
  exit 0
fi

# --- selection --------------------------------------------------------------

SELECTED=""
SKIPPED=""
for name in $(check_names); do
  [ -n "$name" ] || continue
  tier="$(check_field "$name" 2)"
  reason=""
  if [ -n "$ONLY" ] && ! in_list "$name" "$ONLY"; then
    reason="not in --only"
  elif in_list "$name" "$SKIP"; then
    reason="--skip"
  elif [ "$SKIP_SLOW" = 1 ] && [ "$tier" = slow ]; then
    reason="--skip-slow"
  fi
  if [ -n "$reason" ]; then
    SKIPPED="$SKIPPED$name|$reason
"
  else
    SELECTED="$SELECTED $name"
  fi
done

skipped_count() { printf '%s' "$SKIPPED" | grep -c '|' ; }

announce_skips() { # headline
  [ -n "$SKIPPED" ] || return 0
  local n
  n="$(skipped_count)"
  {
    echo
    echo "################################################################################"
    echo "##  $1"
    echo "##  $n of $(check_names | grep -c .) checks did NOT run. This commit is NOT"
    echo "##  covered by the CI jobs below. Do not call it green."
    echo "##"
    printf '%s' "$SKIPPED" | while IFS='|' read -r name reason; do
      [ -n "$name" ] || continue
      printf '##    %-18s (%s)  stands in for: %s\n' "$name" "$reason" "$(check_field "$name" 4)"
    done
    echo "##"
    echo "##  Run the full gate before pushing:  scripts/pre-commit.sh"
    echo "################################################################################"
    echo
  } >&2
}

# --- run --------------------------------------------------------------------

if [ "$HOOK" = 1 ]; then
  echo "pre-commit: running the gates that stand in for the disabled GitHub workflows (#257)."
  echo "pre-commit: bypass is yours to take — git commit --no-verify — and it is recorded nowhere."
fi

announce_skips "WARNING: this is a PARTIAL run."

RESULTS=""
FAILED=0
RUN_START=$(date +%s)

for name in $SELECTED; do
  CURRENT_CHECK="$name"
  job="$(check_field "$name" 4)"
  echo
  echo "==> [$name] $job"
  start=$(date +%s)
  status=0
  run_check "$name" || status=$?
  elapsed=$(( $(date +%s) - start ))
  if [ "$status" -eq 0 ]; then
    verdict=pass
  elif [ "$status" -eq 127 ]; then
    verdict="blocked (missing tool)"
    FAILED=1
  else
    verdict="FAIL ($status)"
    FAILED=1
  fi
  printf '<== [%s] %s in %dm%02ds\n' "$name" "$verdict" "$((elapsed / 60))" "$((elapsed % 60))"
  RESULTS="$RESULTS$name|$verdict|$elapsed
"
  if [ "$FAILED" = 1 ] && [ "$KEEP_GOING" != 1 ]; then
    break
  fi
done

TOTAL=$(( $(date +%s) - RUN_START ))

echo
echo "pre-commit summary ($(printf '%dm%02ds' "$((TOTAL / 60))" "$((TOTAL % 60))") total)"
printf '%s' "$RESULTS" | while IFS='|' read -r name verdict elapsed; do
  [ -n "$name" ] || continue
  printf '  %-18s %-22s %dm%02ds\n' "$name" "$verdict" "$((elapsed / 60))" "$((elapsed % 60))"
done

if [ "$FAILED" = 1 ] && [ "$KEEP_GOING" != 1 ]; then
  not_reached=""
  reached=0
  for name in $SELECTED; do
    if printf '%s' "$RESULTS" | grep -q "^$name|"; then
      reached=1
    else
      not_reached="$not_reached $name"
    fi
  done
  [ "$reached" = 1 ] || true
  if [ -n "$not_reached" ]; then
    echo "  not reached (fail-fast):$not_reached"
  fi
fi

announce_skips "REMINDER: that was a PARTIAL run."

if [ "$FAILED" = 1 ]; then
  echo
  echo "pre-commit: FAILED. Fix the check above, or decide deliberately to bypass it:" >&2
  echo "            git commit --no-verify" >&2
  exit 1
fi

if [ -z "$SKIPPED" ]; then
  echo
  echo "pre-commit: every reproducible CI check passed. The Windows runner jobs"
  echo "            (docs/PRECOMMIT.md) are still unchecked — nothing here can run them."
fi
exit 0
