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

# The non-trivial gates live in ./scripts/ so CI can run them without installing
# `just`; these recipes are thin wrappers (see each script for the rationale).
shellcheck:
    ./scripts/shellcheck.sh

ts-check:
    ./scripts/ts-check.sh

ts-lint:
    ./scripts/ts-lint.sh

ts-build:
    ./scripts/ts-build.sh

# Rebuild and watch frontend/src for changes (used by `just dev`).
ts-watch:
    ./scripts/ts-build.sh --watch

# All TypeScript/JS tests: the pure node:test unit suites (js-tests/) plus the
# Foldkit Story/Scene tests for the widgets (vitest + happy-dom, since they need
# a DOM). The Playwright e2e suite (e2e/) is separate — run it with `npx playwright test`.
test-ts:
    ./scripts/ts-test.sh

# This is faster than "cargo test"
test-rs:
	cargo nextest run --tests

# Run fmt-check + clippy + shellcheck + ts-check + ts-lint (the full standard lint suite).
lint: fmt-check clippy shellcheck ts-check ts-lint

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
    ./scripts/ts-build.sh --watch &
    cargo watch -w concert-tracker/src -w concert-tracker/templates -w concert-tracker/static \
        -x 'run --bin concert-web -- --dev {{ARGS}}'
