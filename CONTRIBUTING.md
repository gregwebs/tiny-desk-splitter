# Contributing

This repo is a Rust workspace with three crates: `scraper`,
`live-set-song-splitter`, and `concert-tracker`. `concert-tracker`
additionally has a TypeScript frontend (`concert-tracker/frontend`), compiled
with [esbuild](https://esbuild.github.io/) into `concert-tracker/static/*.js`
bundles that the Rust backend embeds at compile time — that's why some
commands below are `cargo`/`just <rust-thing>` and others are `npm`/`just
ts-*`. See [README.md](README.md) and [./docs/](docs/) for what the project
does and how it's built; this document only covers setup and day-to-day
development commands.

## Prerequisites

- **Rust** — the workspace uses `rust-toolchain.toml` to pin the toolchain to
  Rust **1.92**, which matches the `Containerfile`. `rustup` installs it
  automatically on first use.
- **yt-dlp** — download concert videos.
- **ffmpeg** — frame analysis and cutting tracks; also required to run the
  Playwright e2e suite (it generates tiny fixture media).
- **OCR Engine** — used when splitting video into tracks:
  - *(default)* a C/C++ toolchain — only to build the **PaddleOCR** backend
    (`--features paddle-ocr`), a more accurate OCR option selectable at
    runtime with `--ocr-engine paddle`. See
    [docs/change/2026-06-04-adopt-paddle-ocr.md](docs/change/2026-06-04-adopt-paddle-ocr.md).
  - *(alternative)* **leptonica** and **tesseract** (`--features leptess-ocr`)
- **Node.js / npm** — needed for `concert-tracker/frontend` (TypeScript
  build/lint) and the Playwright e2e suite.

## Setup (one-time per clone)

```sh
cargo build
cd concert-tracker/frontend && npm install && cd -
just install-hooks
```

`just install-hooks` sets `core.hooksPath = .githooks` so that:
- **pre-commit** runs `cargo fmt --check` (fast)
- **pre-push** runs `just clippy`, `just ts-check`, and `just ts-lint`
  (gates what leaves the machine)

## Building

```sh
cargo build --release
# Binaries: target/release/concert-db, concert-web, scraper, archive_scraper, live-set-splitter
```

## Linting

A root `justfile` provides the standard lint targets:

```sh
just fmt          # auto-format
just lint         # fmt --check + clippy + shellcheck + ts-check + ts-lint (the full standard suite)
just clippy-all   # also lint the leptess-ocr code path (needs Tesseract/leptonica)
```

`just ts-lint` runs [oxlint](https://oxc.rs/docs/guide/usage/linter.html) with the
[Foldkit oxlint plugin](https://foldkit.dev/tooling/oxlint-plugin) over
`concert-tracker/frontend` (config: `concert-tracker/frontend/.oxlintrc.json`) — Elm
Architecture naming/shape conventions plus a strict TypeScript baseline (no `any`, no
type assertions). See `docs/change/2026-07-01-oxlint-foldkit.md`.

## Testing

```sh
just test-rs                  # cargo nextest run --tests — faster than `cargo test`
cargo test -p concert-tracker # just the tracker
cargo test -p tiny-desk-scraper
just test-ts                  # pure node:test suites + Foldkit Story/Scene (vitest) tests
```

### End-to-end (Playwright)

```sh
npm install        # first time only
npx playwright test
```

The e2e suite is fully self-contained — it never touches the real `concerts.db`.
A global setup builds the `concert-web` binary and a deterministic fixture (an
isolated SQLite DB plus tiny, genuinely-playable media generated with **ffmpeg**)
under `e2e/.fixture/`. Each test then copies that fixture into a temp dir and
runs its own `concert-web` on an ephemeral port, driving the **real** endpoints
(no request mocking). Requirements: `ffmpeg` on `PATH` and the Playwright
browser (`npx playwright install chromium`).

The full suite also runs in the `playwright` GitHub Actions job on every pull
request and every push to `main`. When Playwright starts, its HTML report is
attached to the non-cancelled workflow run.

More documentation is in [./docs/playwright.md](docs/playwright.md).

## Frontend (TypeScript)

`concert-tracker/static/{player,playlists,splitter}.js` are **generated build
artifacts** — edit `concert-tracker/frontend/src/*.ts` instead. They're
compiled with esbuild into standalone IIFE bundles with the same filenames
(see `concert-tracker/frontend/build.mjs`'s header comment), committed to the
repo so `cargo build` stays Node-free, and guarded by a drift check
(`scripts/ts-verify.sh`, run in CI) that fails if the committed `.js` doesn't
match a fresh build.

```sh
cd concert-tracker/frontend && npm install   # one-time
just ts-build     # rebuild static/*.js from frontend/src
just ts-check     # strict tsc --noEmit (frontend + js-tests)
just ts-lint      # oxlint + Foldkit oxlint plugin (frontend)
just openapi-types  # regenerate frontend/src/generated/openapi.d.ts from the
                     # backend's live OpenAPI spec, after changing a
                     # #[utoipa::path]/ToSchema in concert-tracker/src/web
```

See `docs/change/2026-06-19-frontend-typescript.md` for the full design.

## Hot-reload dev server

```sh
cargo install cargo-watch   # one-time
cd concert-tracker/frontend && npm install   # one-time
just dev --db test.db --workdir /tmp/tds-dev --port 3001
```

Runs two watchers together: esbuild rebuilds `static/*.js` from
`frontend/src` on every TypeScript edit (milliseconds), and `cargo-watch`
recompiles/restarts `concert-web --dev` on any `src`/`templates`/`static`
change, with the browser auto-refreshing via
[`tower-livereload`](https://docs.rs/tower-livereload) whenever the process
restarts:

| Edit                          | Recompile? |
|-------------------------------|------------|
| `templates/*.html` (incl. inline CSS) | yes — askama compiles templates in |
| `src/**/*.rs`                 | yes |
| `frontend/src/*.ts`           | esbuild rebuilds `static/*.js` (~ms), then a fast (~4s) incremental `cargo-watch` recompile — `include_str!` embeds `static/*.js` at compile time in both the dev and prod code paths, so editing it invalidates the build even though `--dev` itself serves the file from disk |

Use a scratch `--db`/`--workdir` (never the real `concerts.db`) — copy data
from `concerts.db` into the test db first if you need real data to work
against. Without `--dev`, `concert-web` behaves exactly as before: JS is
compiled in and no livereload script is injected.
