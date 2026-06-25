#!/usr/bin/env bash
# Drift guard: static/player.js must be exactly what ts-build produces from the
# current frontend/src. A diff here means someone hand-edited the generated .js
# (forbidden — see its "@generated" banner) or forgot to rebuild after a source
# change. Blocking: wired into `just lint`, the pre-push hook, and CI.
#
# static/splitter.js and static/playlists.js are deliberately NOT diff-guarded
# here: they're Foldkit (Effect-TS) bundles, built minified (unlike player.js,
# which stays unminified and reviewable) because the bundled Effect-TS runtime
# is too large to review as a plain-text diff. They're still committed build
# artifacts (cargo build stays Node-free via include_str!) — just not ones a
# human is expected to read. See docs/change/2026-06-19-foldkit-eval.md.
#
# Called by both CI (.github/workflows/ci.yml) and `just ts-verify`.
set -euo pipefail
cd "$(dirname "$0")/.."

scripts/ts-build.sh
git diff --exit-code -- concert-tracker/static/player.js
