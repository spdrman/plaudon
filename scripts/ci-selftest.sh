#!/usr/bin/env bash
# N1.1 — pipeline self-test: every gate must block its own failing fixture.
#
# A gate nobody has seen fail is a gate nobody knows works. Each case below
# runs a deliberately broken fixture through a real gate and fails this script
# when the gate lets it through.
set -uo pipefail
cd "$(dirname "$0")/.."
REPO_ROOT="$PWD"
FIXTURES="$REPO_ROOT/tests/fixtures/ci-selftest"
FAILED=0

expect_failure() { # name command...
  local name="$1"; shift
  if "$@" > /tmp/selftest-$$.log 2>&1; then
    echo "FAIL  $name: the gate accepted a fixture it must reject"
    sed 's/^/      /' /tmp/selftest-$$.log | tail -15
    FAILED=1
  else
    echo "ok    $name: the gate rejected the fixture"
  fi
  rm -f /tmp/selftest-$$.log
}

expect_success() { # name command...
  local name="$1"; shift
  if "$@" > /tmp/selftest-$$.log 2>&1; then
    echo "ok    $name"
  else
    echo "FAIL  $name: a clean input was rejected"
    sed 's/^/      /' /tmp/selftest-$$.log | tail -15
    FAILED=1
  fi
  rm -f /tmp/selftest-$$.log
}

echo "== clippy gate"
# --target-dir keeps the fixture's build output out of tests/fixtures/: the
# committed-fixture secret scan reads everything under that directory, and a
# stray target/ full of dependency files and cache tags is both not a fixture
# and full of hex strings shaped like device tokens.
expect_failure "clippy blocks a lint violation" \
  cargo clippy --manifest-path "$FIXTURES/clippy-violation/Cargo.toml" --all-targets \
    --target-dir "$REPO_ROOT/target/ci-selftest/clippy-violation" -- -D warnings

echo "== licence gate"
if command -v cargo-deny >/dev/null 2>&1; then
  expect_failure "cargo-deny blocks a forbidden licence" \
    cargo deny --manifest-path "$FIXTURES/forbidden-license/Cargo.toml" check licenses
else
  echo "FAIL  licence gate: cargo-deny is not installed, so the gate could not be proven"
  FAILED=1
fi

echo "== migration gate"
# The drift gate is itself a negative test inside the migration suite: it
# tampers with a committed migration's SQL and asserts the runner refuses it.
# Running the suite therefore proves both directions at once.
expect_success "the migration suite applies the committed schema and rejects a tampered migration" \
  cargo test -p plaudon-persistence --quiet migrations

echo "== documentation gate"
expect_failure "doc-lint blocks a forbidden claim" \
  env PLAUDON_REPO_ROOT="$FIXTURES/forbidden-claim" \
  cargo run -q -p plaudon-release-tools --bin plaudon-release -- doc-lint
expect_success "doc-lint passes on the committed documentation" \
  cargo run -q -p plaudon-release-tools --bin plaudon-release -- doc-lint

echo "== coverage policy gate"
expect_failure "the coverage gate refuses to pass without measurements" \
  cargo run -q -p plaudon-release-tools --bin plaudon-release -- coverage

echo "== scenario registry gate"
expect_failure "a registry missing a required scenario is rejected" \
  env PLAUDON_REPO_ROOT="$FIXTURES/incomplete-registry" \
  cargo run -q -p plaudon-release-tools --bin plaudon-release -- scenarios
expect_success "the committed registry covers all 44 scenarios" \
  cargo run -q -p plaudon-release-tools --bin plaudon-release -- scenarios

echo "== evidence bundle gates"
expect_success "the assembler, secret scanner and claim gate have passing unit tests" \
  cargo test -p plaudon-release-tools --quiet

echo
if [ "$FAILED" -eq 0 ]; then
  echo "pipeline self-test: every gate blocked its fixture"
  exit 0
fi
echo "pipeline self-test: at least one gate did not block" >&2
exit 1
