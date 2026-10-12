#!/usr/bin/env bash
# Regenerate the malicious .recording-archive corpus under tests/fixtures/archives/.
#
# The corpus is committed so the restore path is tested against hostile bytes
# produced by an earlier build, not only against freshly generated ones. Run
# this when the container format changes, and commit the result.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo run -q -p plaudon-integration-tests --bin make-archive-fixtures "$@"
