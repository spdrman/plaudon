#!/usr/bin/env bash
# D2.4 — extended (scheduled) fuzzing run. Spec §18.5, §16.2 rows "Arbitrary
# command execution" and "Malicious audio decoder input", §18.6A, App. B.7.
#
# This is the LONG run. It is NOT part of the per-PR job; see scripts/fuzz-ci.sh
# for the per-commit gate. Run this nightly or before a release candidate.
#
#   scripts/fuzz.sh --list
#   scripts/fuzz.sh --duration 600
#   scripts/fuzz.sh --target fuzz_cli_arg_render --duration 1800
#
# Any new reproducer that cargo-fuzz drops into fuzz/artifacts/<target>/ MUST be
# copied into tests/fixtures/fuzz-corpus/<target>/ with a descriptive name, so
# that `cargo test -p plaudon-security --test fuzz_corpus_replay` keeps it as a
# permanent regression case on stable.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FUZZ_DIR="$REPO_ROOT/fuzz"
CORPUS_ROOT="$REPO_ROOT/tests/fixtures/fuzz-corpus"

# Targets wired to real parsers on this branch.
TARGETS=(
  fuzz_cli_arg_render
  fuzz_destination_url
  fuzz_path_validation
  fuzz_archive_entry
  fuzz_redactor
  fuzz_error_envelope
  fuzz_domain_redactor
)

# Targets that cannot be written yet because the parser they would exercise does
# not exist in this repository. Listed here so the gap is visible rather than
# silently absent. Do not add a stand-in parser to make the target compile.
#
# pending: crates/audio (EPIC F) — fuzz_audio_probe over the media
#   sniffing/container-probe code required by §8.1 and the §16.2 "Malicious
#   audio decoder input" row. crates/audio/src currently contains only lib.rs
#   with no parsing functions, so there is nothing to fuzz. When EPIC F lands
#   `sniff`/`probe`, add `fuzz_audio_probe` here with the invariants
#   "never panics, bounded allocation, malformed input yields a typed error",
#   plus an AddressSanitizer pass over real sample media.
PENDING_TARGETS=(
  "fuzz_audio_probe     # pending: crates/audio (EPIC F)"
)

DURATION=300
RSS_LIMIT_MB=2048
ONLY_TARGET=""

usage() {
  cat <<'EOF'
Usage: scripts/fuzz.sh [--list] [--target <name>] [--duration <secs>]

  --list              print the fuzz targets (and documented pending ones) and exit
  --target <name>     run a single target instead of all of them
  --duration <secs>   wall-clock budget per target (default 300)
  -h, --help          this message
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --list)
      printf 'wired targets:\n'
      for t in "${TARGETS[@]}"; do printf '  %s\n' "$t"; done
      printf 'pending targets (no parser in this repo yet):\n'
      for t in "${PENDING_TARGETS[@]}"; do printf '  %s\n' "$t"; done
      exit 0
      ;;
    --target)
      [[ $# -ge 2 ]] || { echo "error: --target needs a value" >&2; exit 2; }
      ONLY_TARGET="$2"; shift 2 ;;
    --duration)
      [[ $# -ge 2 ]] || { echo "error: --duration needs a value" >&2; exit 2; }
      DURATION="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "error: unknown argument '$1'" >&2; usage >&2; exit 2 ;;
  esac
done

if ! [[ "$DURATION" =~ ^[0-9]+$ ]] || [[ "$DURATION" -eq 0 ]]; then
  echo "error: --duration must be a positive integer number of seconds" >&2
  exit 2
fi

if [[ -n "$ONLY_TARGET" ]]; then
  found=0
  for t in "${TARGETS[@]}"; do [[ "$t" == "$ONLY_TARGET" ]] && found=1; done
  if [[ "$found" -eq 0 ]]; then
    echo "error: '$ONLY_TARGET' is not a wired fuzz target." >&2
    echo "       run 'scripts/fuzz.sh --list' to see the available targets." >&2
    exit 2
  fi
  TARGETS=("$ONLY_TARGET")
fi

# --- toolchain preflight: fail loudly, never skip silently --------------------

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is not on PATH. Install Rust from https://rustup.rs and retry." >&2
  exit 1
fi

if ! cargo +nightly --version >/dev/null 2>&1; then
  cat >&2 <<'EOF'
error: the nightly toolchain is required by cargo-fuzz (libFuzzer needs
       -Z build-std / -Zsanitizer, which stable does not expose).

  Install it with:
      rustup toolchain install nightly

  Then re-run:
      scripts/fuzz.sh --duration 300

  The stable per-PR gate does not need nightly; it runs the corpus replay:
      cargo test -p plaudon-security --test fuzz_corpus_replay
EOF
  exit 1
fi

if ! cargo +nightly fuzz --version >/dev/null 2>&1; then
  cat >&2 <<'EOF'
error: cargo-fuzz is not installed for the nightly toolchain.

  Install it with:
      cargo +nightly install cargo-fuzz

  (On macOS you also need the Xcode command line tools for the libFuzzer
   runtime: xcode-select --install)

  Then re-run:
      scripts/fuzz.sh --duration 300
EOF
  exit 1
fi

# --- run ----------------------------------------------------------------------

echo "fuzz: $(cargo +nightly fuzz --version)"
echo "fuzz: ${#TARGETS[@]} target(s), ${DURATION}s each, rss limit ${RSS_LIMIT_MB} MB"

status=0
for target in "${TARGETS[@]}"; do
  seed_dir="$CORPUS_ROOT/${target#fuzz_}"
  echo
  echo "=== $target (seeds: ${seed_dir#"$REPO_ROOT"/})"
  if [[ ! -d "$seed_dir" ]]; then
    echo "error: missing seed corpus directory $seed_dir" >&2
    exit 1
  fi
  # Seed the live corpus from the committed fixtures, then let libFuzzer grow it.
  live_dir="$FUZZ_DIR/corpus/$target"
  mkdir -p "$live_dir"
  cp -R "$seed_dir/." "$live_dir/"

  if ! ( cd "$FUZZ_DIR" && cargo +nightly fuzz run "$target" "$live_dir" -- \
           -max_total_time="$DURATION" -rss_limit_mb="$RSS_LIMIT_MB" ); then
    status=1
    echo "FAIL: $target found a reproducer." >&2
    echo "      Copy it from fuzz/artifacts/$target/ into $seed_dir/ with a" >&2
    echo "      descriptive name and fix the invariant violation." >&2
  fi
done

echo
if [[ "$status" -eq 0 ]]; then
  echo "fuzz: all targets completed their budget with no reproducer."
else
  echo "fuzz: at least one target found a reproducer (see above)." >&2
fi
exit "$status"
