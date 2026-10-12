#!/usr/bin/env bash
# Point this clone's git hooks at .githooks/ (#257).
#
# Idempotent: running it twice changes nothing and says so. `--uninstall`
# removes the setting again, which is the supported way to turn the gate off
# for good; `git commit --no-verify` is the way to turn it off once.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

ACTION=install
case "${1:-}" in
  "") ;;
  --uninstall) ACTION=uninstall ;;
  -h | --help)
    sed -n '2,8p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  *)
    echo "unknown argument: $1 (expected --uninstall)" >&2
    exit 2
    ;;
esac

current="$(git config --local --get core.hooksPath || true)"

if [ "$ACTION" = uninstall ]; then
  if [ -z "$current" ]; then
    echo "core.hooksPath is not set; nothing to remove"
  else
    git config --local --unset core.hooksPath
    echo "removed core.hooksPath (was $current); git uses .git/hooks again"
  fi
  exit 0
fi

# The hook has to be executable in the index as well, or a fresh clone gets a
# file git refuses to run.
chmod +x .githooks/pre-commit scripts/pre-commit.sh

if [ "$current" = ".githooks" ]; then
  echo "core.hooksPath is already .githooks; nothing to do"
else
  git config --local core.hooksPath .githooks
  echo "core.hooksPath = .githooks"
fi

cat <<'TAIL'

The pre-commit gate now runs the checks that stand in for the disabled GitHub
workflows (#257). Useful commands:

  scripts/pre-commit.sh --list            what runs, and which CI job it covers
  scripts/pre-commit.sh --skip-slow       the fast native gates only
  scripts/pre-commit.sh --only fmt,clippy one or two of them
  PLAUDON_PRECOMMIT_SKIP_SLOW=1 git commit -m "…"

A partial run always prints what it did NOT check. docs/PRECOMMIT.md has the
full mapping, the Windows jobs nothing local can reproduce, and how to switch
GitHub Actions back on.
TAIL
