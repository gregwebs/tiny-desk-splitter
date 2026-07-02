#!/usr/bin/env bash
# Foldkit oxlint pass over concert-tracker/frontend (see .oxlintrc.json):
# Elm-Architecture conventions (Message/Command naming, callable constructors)
# plus a strict TypeScript baseline (no `any`, no type assertions).
#
# Called by both CI (.github/workflows/ci.yml) and `just ts-lint`.
set -euo pipefail
cd "$(dirname "$0")/.."

(cd concert-tracker/frontend && npm run lint)
