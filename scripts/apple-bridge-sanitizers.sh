#!/usr/bin/env bash
# Fuzz, stress and sanitize the Apple bridge C ABI (task H1.5, spec 6.6, 18.4).
#
# Spec 6.6: "we will fuzz and stress the bridge before shipping". This is that
# gate. It builds `native/apple-bridge/tests/abi_stress.c` against the real
# Swift library under AddressSanitizer + UndefinedBehaviorSanitizer and then
# again under ThreadSanitizer, and runs both.
#
# Written in C rather than Rust on purpose: the failures worth catching here
# are allocator and threading failures *across* the language boundary, clang
# instruments both sides of a C-to-Swift call, and none of it needs a nightly
# Rust toolchain.
#
#   scripts/apple-bridge-sanitizers.sh [audio-fixture]
#
# The audio fixture is optional. Without it the transcription paths are
# skipped and everything else still runs, which is what lets this gate pass on
# a plain macOS runner with no Apple Intelligence and no installed models.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ "$(uname -s)" != "Darwin" ]; then
  echo "the Apple bridge sanitizer suite runs on macOS only" >&2
  exit 1
fi

AUDIO="${1:-}"
CORPUS="$(pwd)/tests/fixtures/apple-bridge"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

unset DYLD_FALLBACK_LIBRARY_PATH DYLD_LIBRARY_PATH

echo "== building the bridge =="
scripts/build-apple-bridge.sh "$WORK/lib" >/dev/null
LIB="$WORK/lib/libPlaudonAppleBridge.a"

FRAMEWORKS=(
  -framework Foundation
  -framework AVFoundation
  -framework CoreMedia
  -weak_framework Speech
  -weak_framework FoundationModels
)

build_and_run() {
  local name="$1"
  shift
  echo
  echo "== $name =="
  clang -O1 -g -fno-omit-frame-pointer "$@" \
    -Inative/apple-bridge/include \
    -o "$WORK/abi_stress_$name" \
    native/apple-bridge/tests/abi_stress.c \
    "$LIB" \
    -L/usr/lib/swift -Wl,-rpath,/usr/lib/swift \
    "${FRAMEWORKS[@]}"
  "$WORK/abi_stress_$name" "$CORPUS" "$AUDIO"
}

# `detect_leaks` is unsupported by Apple's AddressSanitizer, so leaks are
# checked separately below with `leaks`, which is. What ASan does catch here
# is `detect_stack_use_after_return`: a progress-relay context that outlives
# its frame is the specific mistake the callback contract is designed to
# prevent, and it is invisible without this option.
export ASAN_OPTIONS="detect_stack_use_after_return=1:abort_on_error=1"
export UBSAN_OPTIONS="print_stacktrace=1:halt_on_error=1"
export TSAN_OPTIONS="halt_on_error=1:second_deadlock_stack=1"

build_and_run "asan_ubsan" -fsanitize=address,undefined

# ThreadSanitizer is a separate binary: it cannot be combined with ASan, and
# the race it is looking for is between the cancel storm and the progress
# relay.
build_and_run "tsan" -fsanitize=thread

# Leak check. The suite allocates and releases hundreds of thousands of
# cancellation handles and response documents, so a per-cycle leak is large
# enough to be unmistakable by the time it finishes. Built without a
# sanitizer because `leaks` and ASan do not coexist.
echo
echo "== leaks =="
clang -O1 -g -Inative/apple-bridge/include \
  -o "$WORK/abi_stress_leaks" \
  native/apple-bridge/tests/abi_stress.c \
  "$LIB" \
  -L/usr/lib/swift -Wl,-rpath,/usr/lib/swift \
  "${FRAMEWORKS[@]}"
MallocStackLogging=1 leaks --atExit --groupByType -- \
  "$WORK/abi_stress_leaks" "$CORPUS" "$AUDIO" | tail -20

echo
echo "sanitizer suite passed"
