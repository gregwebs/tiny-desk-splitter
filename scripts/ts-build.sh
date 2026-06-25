#!/usr/bin/env bash
# Bundle frontend/src/*.ts -> concert-tracker/static/*.js (committed build
# artifacts — see concert-tracker/frontend/build.mjs's header comment).
# Extra args pass through to build.mjs (e.g. --watch for the dev server).
#
# Single source of the esbuild invocation: called by `just ts-build`,
# `just ts-watch`, `just dev`, and scripts/ts-verify.sh.
set -euo pipefail
cd "$(dirname "$0")/.."

node concert-tracker/frontend/build.mjs "$@"
