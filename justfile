# Lint targets
fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

# Default-features clippy (paddle-ocr on; leptess-ocr skipped — needs Tesseract system libs).
# This is the standard lint gate used by the pre-push hook and CI.
clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# Opt-in: also lints the leptess-ocr code path (ocr_leptess.rs + #[cfg(...leptess-ocr)] arms).
# Run this before touching any OCR / leptess backend code.
# Requires Tesseract/leptonica system libraries: brew install tesseract leptonica
clippy-all:
    cargo clippy --workspace --all-targets --features leptess-ocr -- -D warnings

# Strict TypeScript type-check (concert-tracker/frontend's tsconfig + js-tests'
# unit-test tsconfig, which extends it). Catches the same class of bugs the
# frontend/TypeScript conversion exists to prevent — run before pushing.
ts-check:
    cd concert-tracker/frontend && npx tsc --noEmit
    npx tsc --noEmit -p js-tests/tsconfig.json

# Bundle frontend/src/*.ts -> concert-tracker/static/*.js (committed build
# artifacts — see concert-tracker/frontend/build.mjs's header comment).
ts-build:
    node concert-tracker/frontend/build.mjs

# Rebuild and watch frontend/src for changes (used by `just dev`).
ts-watch:
    node concert-tracker/frontend/build.mjs --watch

# Drift guard: static/player.js and static/playlists.js must be exactly what
# `ts-build` produces from the current frontend/src. A diff here means someone
# hand-edited the generated .js (forbidden — see its "@generated" banner) or
# forgot to rebuild after a source change. Blocking: wired into `lint` and the
# pre-push hook.
#
# static/splitter.js is deliberately NOT diff-guarded here: it's a Foldkit
# (Effect-TS) bundle, built minified (unlike the other two, which stay
# unminified and reviewable) because the bundled Effect-TS runtime is too
# large to review as a plain-text diff. It's still a committed build artifact
# (cargo build stays Node-free via include_str!) — just not one a human is
# expected to read. See docs/change/2026-06-19-foldkit-eval.md.
ts-verify: ts-build
    git diff --exit-code -- concert-tracker/static/player.js concert-tracker/static/playlists.js

# Run fmt-check + clippy + ts-check + ts-verify (the full standard lint suite).
lint: fmt-check clippy ts-check ts-verify

# Wire up the version-controlled git hooks (one-time per clone).
install-hooks:
    git config core.hooksPath .githooks
    @echo "Git hooks installed from .githooks/"

# Regenerate frontend/src/generated/openapi.d.ts from the backend's live OpenAPI
# spec (concert-tracker/src/bin/openapi_dump.rs prints exactly what's served at
# /api-docs/openapi.json — see web::built_api_doc). Run this after changing any
# #[utoipa::path]/ToSchema in concert-tracker/src/web, then re-run the relevant
# `just ts-*` recipe / `cargo build` for the frontend ports that consume it.
openapi-types:
    cargo run -q --bin openapi-dump > /tmp/concert-tracker-openapi.json
    cd concert-tracker/frontend && npm run openapi-types

# Hot-reload dev server. Requires: cargo install cargo-watch
# Runs two watchers together: esbuild rebuilds static/*.js from frontend/src on
# every TS edit (~ms); cargo-watch recompiles + restarts concert-web on any
# src/templates/static change. The static/*.js case is still a real (if fast,
# ~4s incremental) recompile — include_str! embeds those files at compile time
# in both the dev and prod code paths, so editing them invalidates the build
# regardless of which path actually serves them at runtime; dev mode itself
# serves static/ straight from disk (see --dev in concert_web.rs), it just
# doesn't skip the recompile that --dev's own embedding code path still incurs.
# tower-livereload's reload signal changes on each process restart, which is
# what actually triggers the browser refresh — see RouterOpts::dev.
dev *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    trap 'kill 0' EXIT
    node concert-tracker/frontend/build.mjs --watch &
    cargo watch -w concert-tracker/src -w concert-tracker/templates -w concert-tracker/static \
        -x 'run --bin concert-web -- --dev {{ARGS}}'
