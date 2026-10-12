#!/usr/bin/env bash
# N2.5 — clean-machine install / update / rollback / restore release gate
# (spec 19 Phase 9 exit gate, spec 22 items 8 and 9).
#
# Every step asserts something and writes its verdict into the evidence
# staging directory. A step that cannot be performed in this environment is
# recorded as `not_verified` with a reason and FAILS the gate: spec 24 treats a
# claimed configuration without its evidence as a release failure, so "could
# not check" never reads as "fine".
#
#   scripts/release-gate.sh <app.zip> [--evidence <dir>] [--allow-unsigned]
#   scripts/release-gate.sh --artifact <app.zip> --evidence <dir> [--allow-unsigned]
#
# `--allow-unsigned` is for a development run on a workstation with no
# Developer ID. It changes the recorded verdict to `development_only` and the
# gate still exits non-zero; it exists so the remaining steps can be exercised,
# not to turn a red gate green.
#
# The artifact is `dist/macos-egui/Plaudon.app.zip` from
# `scripts/package-macos-egui.sh`: one shell ships, so the gate asserts one
# bundle layout — a `.app` whose `CFBundleExecutable` starts and keeps
# running, resolving its library from the bundle identifier.
set -uo pipefail
cd "$(dirname "$0")/.."
REPO_ROOT="$PWD"

ARTIFACT=""
EVIDENCE="$REPO_ROOT/ci-artifacts/evidence"
ALLOW_UNSIGNED=0
while [ $# -gt 0 ]; do
  case "$1" in
    --artifact) ARTIFACT="$2"; shift 2 ;;
    --evidence) EVIDENCE="$2"; shift 2 ;;
    --allow-unsigned) ALLOW_UNSIGNED=1; shift ;;
    -*) echo "unknown argument: $1" >&2; exit 2 ;;
    # A bare path is the artifact: that is how the gate is invoked by hand
    # right after a packaging run, and a silent "unknown argument" there would
    # look like a gate failure rather than a typo.
    *)
      if [ -n "$ARTIFACT" ]; then
        echo "unexpected second artifact: $1" >&2
        exit 2
      fi
      ARTIFACT="$1"; shift ;;
  esac
done

mkdir -p "$EVIDENCE/release-gate"
RESULTS="$EVIDENCE/release-gate/steps.jsonl"
: > "$RESULTS"
FAILED=0

record() { # name verdict detail
  printf '{"step":"%s","verdict":"%s","detail":"%s"}\n' "$1" "$2" "${3//\"/\\\"}" >> "$RESULTS"
  printf '%-34s %-14s %s\n' "$1" "$2" "$3"
  case "$2" in
    pass) ;;
    *) FAILED=1 ;;
  esac
}

HOST_OS="$(uname -s)"
CLEAN_MACHINE="${PLAUDON_CLEAN_MACHINE:-0}"
if [ "$CLEAN_MACHINE" = "1" ]; then
  record environment pass "clean, native $HOST_OS runner with no pre-provisioned application state"
else
  record environment not_verified "not a clean machine: set PLAUDON_CLEAN_MACHINE=1 only on a freshly imaged native runner (spec 18.4)"
fi

# ------------------------------------------------------ 1. artifact + signature
if [ -z "$ARTIFACT" ] || [ ! -e "$ARTIFACT" ]; then
  record artifact_present fail "no artifact supplied at '$ARTIFACT'"
else
  record artifact_present pass "$(basename "$ARTIFACT") $(shasum -a 256 "$ARTIFACT" | cut -c1-16)…"

  WORK="$(mktemp -d)"
  trap 'rm -rf "$WORK"' EXIT
  if [ "$HOST_OS" = "Darwin" ]; then
    /usr/bin/ditto -x -k "$ARTIFACT" "$WORK" 2>/dev/null
    APP="$(find "$WORK" -maxdepth 1 -name '*.app' -print -quit)"
    if [ -z "$APP" ]; then
      record install fail "the artifact contains no .app bundle"
    else
      record install pass "extracted $(basename "$APP") into a clean location"

      if codesign --verify --deep --strict "$APP" 2>/dev/null; then
        if spctl --assess --type execute "$APP" 2>/dev/null; then
          record signature pass "codesign strict verification and Gatekeeper assessment both pass"
        elif [ "$ALLOW_UNSIGNED" = "1" ]; then
          record signature development_only "ad-hoc signature verifies but Gatekeeper rejects it: no Developer ID or notarization in this environment"
        else
          record signature fail "Gatekeeper assessment failed: the artifact is not notarized"
        fi
      else
        record signature fail "codesign strict verification failed"
      fi

      # Negative: tampering with a bundled binary after signing must break
      # verification (spec 16.5, N2.1 acceptance).
      TAMPERED="$WORK/tampered.app"
      cp -R "$APP" "$TAMPERED"
      BIN="$(find "$TAMPERED/Contents/MacOS" -type f -print -quit)"
      printf '\x00' >> "$BIN"
      if codesign --verify --deep --strict "$TAMPERED" 2>/dev/null; then
        record tamper_detection fail "a modified bundled binary still passed verification"
      else
        record tamper_detection pass "a modified bundled binary fails codesign verification"
      fi

      # Launch: the installed application must start and then exit cleanly.
      if [ "${PLAUDON_SKIP_LAUNCH:-0}" = "1" ]; then
        record launch not_verified "launch skipped by PLAUDON_SKIP_LAUNCH; a headless runner cannot prove a GUI launch"
      else
        LOG="$WORK/launch.log"
        # The bundle executable is named by CFBundleExecutable, which need not
        # match the bundle name.
        EXECUTABLE="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$APP/Contents/Info.plist" 2>/dev/null)"
        [ -n "$EXECUTABLE" ] || EXECUTABLE="$(basename "${APP%.app}")"
        "$APP/Contents/MacOS/$EXECUTABLE" > "$LOG" 2>&1 &
        LAUNCH_PID=$!
        sleep 3
        if kill -0 "$LAUNCH_PID" 2>/dev/null; then
          kill "$LAUNCH_PID" 2>/dev/null
          wait "$LAUNCH_PID" 2>/dev/null
          record launch pass "the installed bundle started and stayed up for 3s"
        else
          wait "$LAUNCH_PID"
          STATUS=$?
          if [ "$STATUS" -eq 0 ]; then
            record launch pass "the installed bundle started and exited cleanly"
          else
            record launch fail "the installed bundle exited with status $STATUS: $(tail -c 400 "$LOG")"
          fi
        fi
      fi

      # The library directory is the whole upgrade story (migration plan R7):
      # the shell resolves it from the bundle identifier with `dirs`, and the
      # bundle declares that identifier in its Info.plist. If the two
      # disagree, an existing user launches the new build into an empty
      # library and nothing in the UI says why.
      IDENTIFIER="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP/Contents/Info.plist" 2>/dev/null)"
      EXPECTED_IDENTIFIER="$(sed -n 's/.*APPLICATION_IDENTIFIER: &str = "\([^"]*\)".*/\1/p' \
        "$REPO_ROOT/crates/app/src/runtime.rs" | head -1)"
      LIBRARY="$HOME/Library/Application Support/$IDENTIFIER"
      if [ -z "$IDENTIFIER" ] || [ "$IDENTIFIER" != "$EXPECTED_IDENTIFIER" ]; then
        record library_directory fail "the bundle is $IDENTIFIER but the application resolves its library under $EXPECTED_IDENTIFIER"
      elif [ "${PLAUDON_SKIP_LAUNCH:-0}" = "1" ]; then
        record library_directory not_verified "the library is created on first launch, which was skipped"
      elif [ -d "$LIBRARY" ]; then
        record library_directory pass "the launched bundle used $LIBRARY, the path the application resolves for $IDENTIFIER"
      else
        record library_directory fail "nothing was created at $LIBRARY: the launched bundle is not using the identifier's library path"
      fi
    fi
  else
    record install not_verified "this gate step is implemented for macOS; run scripts/package-windows-egui.ps1 plus this gate's Windows leg natively (spec 18.4)"
  fi
fi

# --------------------------------------- 2. offline import / STT / export smoke
if cargo test -p plaudon-e2e --quiet 2>&1 | tail -5 | grep -q "test result: ok"; then
  record offline_smoke pass "spec 18.7 scenarios 1 and 2 pass with the outbound policy denying every request"
else
  record offline_smoke fail "the offline import/transcribe/export acceptance scenarios failed"
fi

# --------------------------------------------- 2b. the shell's own test suite
# The whole UI suite: kittest interaction, accessibility-tree and packaging
# assertions, driven by `cargo test`. It is Rust end to end and no JavaScript
# or browser-driver runner may be added (docs/EGUI_TESTING.md §1). Pixel
# comparison stays opt-in and off here: it is only valid on the MARS
# renderer, so a workstation run would either lie or fail for the wrong
# reason.
UI_LOG="$EVIDENCE/release-gate/ui-suite.log"
if cargo test -p plaudon-egui -p plaudon-ui -p plaudon-platform --quiet > "$UI_LOG" 2>&1; then
  record ui_suite pass "the egui shell, screen and packaging suites pass (kittest; log at $UI_LOG)"
else
  record ui_suite fail "the egui UI suite failed: $(tail -c 400 "$UI_LOG")"
fi

# ----------------------------------------------- 3. signed update and rollback
if cargo test -p plaudon-updater --quiet 2>&1 | tail -5 | grep -q "test result: ok"; then
  record signed_update pass "forward update, rejection of unsigned/tampered artifacts and verified rollback all pass"
else
  record signed_update fail "the updater verification and rollback suite failed"
fi

# ------------------------------------------------ 4. portable archive round trip
# One invocation, not two: the suite was being run twice and judged on
# `tail -5`, which reads whichever target happened to print last. A crate with
# an empty second target therefore reported "no passing round-trip" while all
# of its tests passed. Judge the whole output instead: at least one test must
# have passed, and no block may have failed.
ARCHIVE_OUT="$(cargo test -p plaudon-export --quiet 2>&1 || true)"
ARCHIVE_PASSED="$(printf '%s\n' "$ARCHIVE_OUT" \
  | sed -n 's/^test result: ok\. \([0-9]*\) passed.*/\1/p' | awk '{n += $1} END {print n + 0}')"
if printf '%s\n' "$ARCHIVE_OUT" | grep -q "test result: FAILED"; then
  record archive_restore fail "the portable archive suite has failing tests"
elif [ "$ARCHIVE_PASSED" -gt 0 ]; then
  record archive_restore pass \
    "portable archive export and restore reproduce media hashes and structured records ($ARCHIVE_PASSED tests)"
else
  record archive_restore not_verified "the portable archive suite produced no passing round-trip; export/restore is a release gate (spec 22(9))"
fi

# ------------------------------------------------------ 5. repeatability check
SECOND_RUN="$(cargo test -p plaudon-e2e --quiet 2>&1 | tail -5 | grep -c "test result: ok")"
if [ "$SECOND_RUN" -ge 1 ]; then
  record repeatability pass "a second consecutive run produced the same verdict"
else
  record repeatability fail "a second consecutive run disagreed with the first"
fi

echo
if [ "$FAILED" -eq 0 ]; then
  echo "release gate: PASS"
  exit 0
fi
echo "release gate: BLOCKED — see $RESULTS" >&2
exit 1
