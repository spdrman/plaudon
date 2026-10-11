#!/usr/bin/env bash
# Software composition and licence audit (spec 18.8, A1.5).
#
# Fails the build when a dependency carries an unreviewed or denied licence,
# when an advisory applies, or when a source other than crates.io is used.
# Requires cargo-deny: cargo install cargo-deny --locked
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v cargo-deny >/dev/null 2>&1; then
  echo "cargo-deny is not installed: cargo install cargo-deny --locked" >&2
  exit 127
fi

cargo deny --all-features check licenses bans sources advisories
