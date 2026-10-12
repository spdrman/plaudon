#!/usr/bin/env bash
# Opt-in capture of a real, user-installed official PLAUD Cloud CLI (K1.1).
#
# Never run in CI. It launches a vendor binary that talks to PLAUD's servers,
# so it requires a PLAUD account you are already signed in to with
# `plaud login`, run by you, outside this repository.
#
#   scripts/probe-plaud-cli.sh [output-directory]
#
# Output is a capture bundle for `tests/integration/fixtures/plaud-cli/`.
# Read ADR-0040 before promoting a release into the compatibility table.

set -euo pipefail

out="${1:-}"
if [[ -z "${out}" ]]; then
    stamp="$(date +%Y%m%d-%H%M%S)"
    out="target/plaud-cli-capture/${stamp}"
fi

echo "This launches the vendor CLI you installed and reads your PLAUD account."
echo "It runs only 'plaud --version' and 'plaud files'. It never signs in for"
echo "you, never deletes anything and never writes to your account."
echo
echo "The capture will contain your recording titles. Review it for personal"
echo "data before committing anything."
echo
read -r -p "Continue? [y/N] " reply
case "${reply}" in
    y | Y | yes | YES) ;;
    *)
        echo "aborted"
        exit 1
        ;;
esac

cargo run \
    -p plaudon-source-plaud-cli \
    --features vendor-cli-probe \
    --bin plaud-cli-probe \
    -- "${out}"
