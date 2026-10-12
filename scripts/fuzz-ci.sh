#!/usr/bin/env bash
# D2.4 — the PER-PR fuzzing gate. Spec §18.5, §18.8 (CI pipeline).
#
# Scope split, stated explicitly so nobody mistakes this for a real fuzzing run:
#
#   * THIS script runs on every commit/PR. It is fast and deterministic:
#       1. the stable-toolchain corpus replay (always, no nightly needed) —
#          every committed hostile input is pushed through the exact same
#          check functions the libFuzzer targets use, and the same invariants
#          are asserted;
#       2. `cargo +nightly fuzz build` to prove every fuzz target still
#          compiles, when a nightly toolchain and cargo-fuzz are present;
#       3. an AddressSanitizer pass over the replay test, when nightly is
#          present.
#     Steps 2 and 3 are reported as "not available" (not "skipped quietly")
#     when the toolchain is missing; step 1 is mandatory and never skipped.
#
#   * The EXTENDED, time-boxed fuzzing run is SCHEDULED (nightly job /
#     pre-release), not per-commit: scripts/fuzz.sh --duration 1800
#
# Exit code is non-zero if the mandatory replay fails, or if an available
# optional step fails. A missing nightly toolchain is reported but does not fail
# the PR gate, because the replay already enforces the invariants on stable.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

status=0
nightly_available=0
cargo_fuzz_available=0

if cargo +nightly --version >/dev/null 2>&1; then
  nightly_available=1
  if ( cd fuzz && cargo +nightly fuzz --version >/dev/null 2>&1 ); then
    cargo_fuzz_available=1
  fi
fi

echo "== [1/3] mandatory: stable corpus replay =="
echo "   cargo test -p plaudon-security --test fuzz_corpus_replay"
echo "   cargo test -p plaudon-export --test voiceprofile_corpus_replay"
if cargo test -p plaudon-security --test fuzz_corpus_replay \
   && cargo test -p plaudon-export --test voiceprofile_corpus_replay; then
  echo "   OK: every committed corpus entry satisfied its invariants."
else
  echo "   FAIL: corpus replay failed." >&2
  status=1
fi

echo
echo "== [2/3] optional: fuzz targets compile (cargo +nightly fuzz build) =="
if [[ "$cargo_fuzz_available" -eq 1 ]]; then
  if ( cd fuzz && cargo +nightly fuzz build ); then
    echo "   OK: all fuzz targets build."
  else
    echo "   FAIL: a fuzz target no longer builds." >&2
    status=1
  fi
elif [[ "$nightly_available" -eq 1 ]]; then
  echo "   NOT AVAILABLE: nightly is installed but cargo-fuzz is not."
  echo "   Install with: cargo +nightly install cargo-fuzz"
  echo "   The mandatory replay above already enforced the invariants."
else
  echo "   NOT AVAILABLE: no nightly toolchain on this machine."
  echo "   Install with: rustup toolchain install nightly && cargo +nightly install cargo-fuzz"
  echo "   The mandatory replay above already enforced the invariants."
fi

echo
echo "== [3/3] optional: AddressSanitizer pass over the corpus replay =="
if [[ "$nightly_available" -eq 1 ]]; then
  # -Zsanitizer=address only gives clean results when std is rebuilt with the
  # sanitizer, which needs the `rust-src` component and -Z build-std.
  host_triple="$(cargo +nightly -vV | awk '/^host:/ {print $2}')"
  rust_src="$(rustc +nightly --print sysroot)/lib/rustlib/src/rust/library"
  if [[ -d "$rust_src" ]]; then
    echo "   running with RUSTFLAGS=-Zsanitizer=address (target $host_triple)"
    if RUSTFLAGS="-Zsanitizer=address" RUSTDOCFLAGS="-Zsanitizer=address" \
         cargo +nightly test -p plaudon-security --test fuzz_corpus_replay \
         -Z build-std --target "$host_triple"; then
      echo "   OK: no ASan report."
    else
      echo "   FAIL: AddressSanitizer reported an error." >&2
      status=1
    fi
  else
    echo "   NOT AVAILABLE: the 'rust-src' component is missing, so -Z build-std"
    echo "   cannot rebuild std with the sanitizer. Install with:"
    echo "       rustup component add rust-src --toolchain nightly"
  fi
else
  echo "   NOT AVAILABLE: no nightly toolchain, so -Zsanitizer=address cannot run."
  echo "   Install with: rustup toolchain install nightly"
fi

echo
echo "Reminder: the extended fuzzing run is scheduled, not per-commit."
echo "          scripts/fuzz.sh --duration 1800"
exit "$status"
