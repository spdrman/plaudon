#!/usr/bin/env bash
# Unit/component test suite for the desktop UI (hermetic: no network, no Tauri).
set -euo pipefail
cd "$(dirname "$0")/../apps/desktop"
npm run test -- "$@"
