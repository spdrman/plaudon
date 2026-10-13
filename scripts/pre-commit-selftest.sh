#!/usr/bin/env bash
# Self-test for the pre-commit gate (#257), in the spirit of
# scripts/ci-selftest.sh: a gate nobody has seen fail is a gate nobody knows
# works.
#
# Everything happens in a throwaway clone under target/, never in your working
# copy, and the clone takes the hook, the runner and the installer from the
# working tree so an uncommitted change to them is what gets tested.
#
# Cases:
#   1  an unformatted file blocks `git commit`, and the commit does not happen
#   2  the same commit succeeds once the file is formatted
#   3  `git commit --no-verify` goes through regardless — the bypass is real
#   4  a partial run says, loudly, which CI jobs it did not cover
#   5  scripts/install-hooks.sh is idempotent, and --uninstall undoes it
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT" || exit 1

FAILED=0
pass() { echo "ok    $1"; }
fail() { echo "FAIL  $1"; FAILED=1; }

SCRATCH="$(mktemp -d "$REPO_ROOT/target/precommit-selftest.XXXXXX")"
CLONE="$SCRATCH/clone"
# shellcheck disable=SC2329  # invoked by the EXIT trap below
cleanup() { rm -rf "$SCRATCH"; }
trap cleanup EXIT INT TERM

echo "== preparing a throwaway clone in $CLONE"
git clone --quiet --no-hardlinks "$REPO_ROOT" "$CLONE"
mkdir -p "$CLONE/.githooks"
cp "$REPO_ROOT/.githooks/pre-commit" "$CLONE/.githooks/pre-commit"
cp "$REPO_ROOT/scripts/pre-commit.sh" "$REPO_ROOT/scripts/install-hooks.sh" "$CLONE/scripts/"
cd "$CLONE" || exit 1
git config user.email "selftest@plaudon.invalid"
git config user.name "pre-commit self-test"
# Only the formatting gate: this proves the wiring, not the Rust workspace,
# and it must stay quick enough that people actually run it.
export PLAUDON_PRECOMMIT_ONLY=fmt

echo "== installing the hook"
if out="$(scripts/install-hooks.sh 2>&1)"; then
  pass "install-hooks.sh sets core.hooksPath"
else
  fail "install-hooks.sh exited non-zero"
  printf '%s\n' "$out" | sed 's/^/      /'
fi
[ "$(git config --local --get core.hooksPath)" = ".githooks" ] ||
  fail "core.hooksPath was not set to .githooks"

if out="$(scripts/install-hooks.sh 2>&1)" && printf '%s' "$out" | grep -q "already"; then
  pass "install-hooks.sh is idempotent (case 5)"
else
  fail "a second install did not report the setting as already present"
fi

echo
echo "== case 1: an unformatted file must block the commit"
TARGET="$(git ls-files 'crates/*/src/lib.rs' | head -1)"
[ -n "$TARGET" ] || { echo "no crate lib.rs to mangle" >&2; exit 1; }
printf '\npub fn   plaudon_precommit_selftest( )   ->  ()   { }\n' >>"$TARGET"
git add "$TARGET"
before="$(git rev-parse HEAD)"

if git commit -m "deliberately unformatted" >"$SCRATCH/case1.log" 2>&1; then
  fail "case 1: the hook let an unformatted file through"
else
  pass "case 1: the hook rejected the unformatted file"
fi
sed 's/^/      /' "$SCRATCH/case1.log" | tail -12
grep -q "FAIL" "$SCRATCH/case1.log" ||
  fail "case 1: the failure was not reported as a failing check"
grep -q "fmt" "$SCRATCH/case1.log" ||
  fail "case 1: the output did not name the fmt gate"
grep -q -- "--no-verify" "$SCRATCH/case1.log" ||
  fail "case 1: the output did not say how to bypass deliberately"
if [ "$(git rev-parse HEAD)" = "$before" ]; then
  pass "case 1: no commit was created"
else
  fail "case 1: a commit was created anyway"
fi

echo
echo "== case 2: the same commit must succeed once the file is formatted"
cargo fmt --all >/dev/null 2>&1 || true
git add "$TARGET"
if git commit -m "formatted" >"$SCRATCH/case2.log" 2>&1; then
  pass "case 2: the hook accepted the formatted file"
else
  fail "case 2: a clean tree was rejected"
  sed 's/^/      /' "$SCRATCH/case2.log" | tail -20
fi
[ "$(git rev-parse HEAD)" != "$before" ] ||
  fail "case 2: the commit was not created"

echo
echo "== case 3: --no-verify must still go through"
printf '\npub fn   plaudon_precommit_selftest_two( )  { }\n' >>"$TARGET"
git add "$TARGET"
if git commit --no-verify -m "bypassed on purpose" >"$SCRATCH/case3.log" 2>&1; then
  pass "case 3: --no-verify bypassed the gate, as documented"
else
  fail "case 3: --no-verify did not commit"
  sed 's/^/      /' "$SCRATCH/case3.log" | tail -10
fi
git reset --quiet --hard HEAD~1

echo
echo "== case 4: a partial run must say what it did not check"
scripts/pre-commit.sh --only fmt >"$SCRATCH/case4.log" 2>&1
status=$?
[ "$status" -eq 0 ] || fail "case 4: --only fmt failed on a clean tree (status $status)"
if grep -q "did NOT run" "$SCRATCH/case4.log" &&
  grep -q "Do not call it green" "$SCRATCH/case4.log" &&
  grep -q "clippy" "$SCRATCH/case4.log"; then
  pass "case 4: the partial run named the checks it skipped"
else
  fail "case 4: a partial run did not announce the gap"
  sed 's/^/      /' "$SCRATCH/case4.log" | tail -20
fi

if PLAUDON_PRECOMMIT_ONLY='' PLAUDON_PRECOMMIT_SKIP_SLOW=1 \
  scripts/pre-commit.sh --skip-slow >"$SCRATCH/case4b.log" 2>&1; then
  : # the fast tier is allowed to pass or fail here; the banner is the subject
fi
if grep -q "PARTIAL run" "$SCRATCH/case4b.log"; then
  pass "case 4: --skip-slow announced the slow jobs it left out"
else
  fail "case 4: --skip-slow did not announce the gap"
  sed 's/^/      /' "$SCRATCH/case4b.log" | tail -20
fi

echo
echo "== case 5: --uninstall removes the setting"
scripts/install-hooks.sh --uninstall >/dev/null 2>&1
if [ -z "$(git config --local --get core.hooksPath || true)" ]; then
  pass "case 5: --uninstall removed core.hooksPath"
else
  fail "case 5: core.hooksPath survived --uninstall"
fi

echo
if [ "$FAILED" -eq 0 ]; then
  echo "pre-commit self-test: the gate blocks a deliberate failure and admits its gaps"
  exit 0
fi
echo "pre-commit self-test: at least one case did not hold" >&2
exit 1
