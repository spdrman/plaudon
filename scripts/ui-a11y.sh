#!/usr/bin/env bash
# WCAG 2.2 AA checks: component-level axe assertions plus the Playwright
# keyboard/a11y suite against the dev server with the in-memory backend.
set -euo pipefail
cd "$(dirname "$0")/../apps/desktop"
npm run test -- --run src/features src/app
npx playwright test --reporter=line
