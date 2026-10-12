#!/usr/bin/env bash
# D1.4 — the secret/signed-URL scan gate. Spec §18.5 ("scan all logs for keys
# and signed URLs"), §1.5 ("Secrets in application logs: 0 in automated
# scanning"), §16.2 ("Signed URL leak — snapshot/log scanner").
#
# Builds `plaudon-secret-scan` and runs it over the conventional locations:
#
#   * build logs under target/ (*.log)
#   * every committed fixture under tests/fixtures/
#   * any SQLite database in the working tree (*.sqlite, *.db, *.sqlite3, *.db3)
#   * a runtime log directory, if one is supplied (argument 1, or $PLAUDON_LOG_DIR)
#
# Extra paths can be appended as further arguments.
#
# EXCLUSIONS — stated here rather than hidden in an allowlist, because they are
# "not a scan target", not "a false positive":
#
#   * tests/fixtures/fuzz-corpus/**  — adversarial *inputs* to the redactor
#     fuzz targets. They contain synthetic secrets on purpose; they are the
#     material the redactor is proven against, not an artefact of running it.
#
# Documented false positives (values that genuinely have a secret's shape but
# are not credentials, e.g. SHA-256 content digests) live in
# tests/fixtures/secret-scan/allowlist.json. An allowlist entry that matches
# nothing is itself reported, so the list cannot rot.
#
# Exit code: 0 clean, 1 findings or a stale allowlist entry, 2 usage/IO error.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

ALLOWLIST="tests/fixtures/secret-scan/allowlist.json"
FORMAT="${PLAUDON_SECRET_SCAN_FORMAT:-text}"

log_dir="${1:-${PLAUDON_LOG_DIR:-}}"
if [[ $# -gt 0 ]]; then
  shift
fi

echo "==> building plaudon-secret-scan"
cargo build --quiet -p plaudon-diagnostics --features scanner --bin plaudon-secret-scan

BIN="target/debug/plaudon-secret-scan"
if [[ ! -x "$BIN" ]]; then
  echo "secret-scan: $BIN was not produced by the build" >&2
  exit 2
fi

targets=()

if [[ -d tests/fixtures ]]; then
  targets+=("tests/fixtures")
fi

if [[ -n "$log_dir" ]]; then
  if [[ -d "$log_dir" ]]; then
    targets+=("$log_dir")
  else
    echo "secret-scan: log directory '$log_dir' does not exist" >&2
    exit 2
  fi
fi

# Build logs and any database in the tree. `find -print0` keeps paths with
# spaces intact; target/ is pruned for databases because build artefacts are
# not application data.
while IFS= read -r -d '' path; do
  targets+=("$path")
done < <(find target -type f -name '*.log' -print0 2>/dev/null || true)

while IFS= read -r -d '' path; do
  targets+=("$path")
done < <(find . -type d -name target -prune -o -type d -name .git -prune -o \
  -type f \( -name '*.sqlite' -o -name '*.sqlite3' -o -name '*.db' -o -name '*.db3' \) \
  -print0 2>/dev/null || true)

# Any extra paths passed after the log directory.
for extra in "$@"; do
  targets+=("$extra")
done

if [[ ${#targets[@]} -eq 0 ]]; then
  echo "secret-scan: nothing to scan (no fixtures, logs or databases found)" >&2
  exit 2
fi

echo "==> scanning ${#targets[@]} path(s)"
set +e
"$BIN" \
  --format "$FORMAT" \
  --allowlist "$ALLOWLIST" \
  --exclude '**/fuzz-corpus/**' \
  "${targets[@]}"
status=$?
set -e

case "$status" in
  0) echo "secret-scan: clean" ;;
  1) echo "secret-scan: FAILED — secrets or stale allowlist entries above" >&2 ;;
  *) echo "secret-scan: error (exit $status)" >&2 ;;
esac

exit "$status"
