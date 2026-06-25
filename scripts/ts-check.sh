#!/usr/bin/env bash
# Strict TypeScript type-check (concert-tracker/frontend's tsconfig + js-tests'
# unit-test tsconfig, which extends it). Catches the same class of bugs the
# frontend/TypeScript conversion exists to prevent — run before pushing.
#
# Called by both CI (.github/workflows/ci.yml) and `just ts-check`.
set -euo pipefail
cd "$(dirname "$0")/.."

(cd concert-tracker/frontend && npx tsc --noEmit)
npx tsc --noEmit -p js-tests/tsconfig.json
