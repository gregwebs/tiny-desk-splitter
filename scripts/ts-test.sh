#!/usr/bin/env bash
# All TypeScript/JS tests: the pure node:test unit suites (js-tests/) plus the
# Foldkit Story/Scene tests for the widgets (vitest + happy-dom, since they need
# a DOM). The Playwright e2e suite (e2e/) is separate — run it with `npx playwright test`.
#
# Called by both CI (.github/workflows/ci.yml) and `just test-ts`.
set -euo pipefail
cd "$(dirname "$0")/.."

npm run test:unit
(cd concert-tracker/frontend && npm run test:story)
