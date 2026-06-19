# Tiny Desk Splitter

Tools for downloading and splitting NPR Tiny Desk Concerts into individual tracks with metadata.

## Workspace crates

| Crate | Description |
|---|---|
| `scraper` | Scrape concert metadata from NPR pages; list concerts from the archive |
| `live-set-song-splitter` | Split a concert MP4 into individual tracks using FFmpeg |
| `concert-tracker` | SQLite-backed web UI and CLI to track download/split state across concerts |

## Quick start

```sh
cargo build
cargo run --bin concert-web
# → http://localhost:3000
```

## Running with containers

OCI images (Docker / Podman / Buildah compatible) are the easiest way to run
the app without installing Rust, a C++ toolchain, or the OCR build dependencies.

```sh
# 1. Build the release image (requires network: downloads MNN + OCR models)
docker build --target release -t tiny-desk .
# same with: podman build --target release -t tiny-desk .

# 2. Run (all data persisted in the named volume tiny-desk-data at /data)
docker run --rm -p 3000:3000 -v tiny-desk-data:/data tiny-desk
# → http://localhost:3000

# Or via Compose:
docker compose up
```

Three image targets are available:

| Target | Tag | Use |
|---|---|---|
| `base` | `tiny-desk-base` | Runtime only: ffmpeg, yt-dlp |
| `dev` | `tiny-desk-dev` | base + Rust + C++ toolchain (development) |
| `release` | `tiny-desk` | Compiled binaries + OCR models on top of base |

Build all three with `./scripts/build-images.sh` (auto-detects Docker or Podman).

See [docs/change/2026-06-15-containerization.md](docs/change/2026-06-15-containerization.md)
for full details: volume/port contract, yt-dlp version bumps, CLI tool access via
`--entrypoint`, and the `--host` flag added to `concert-web`.

## Dependencies

- **Rust** — run the project (`cargo build && cargo run --bin concert-web`)
- **yt-dlp** — download concert videos
Splitting video into tracks
- **ffmpeg** — frame analysis
- OCR Engine
  - *(default)* a C/C++ toolchain — only to build the **PaddleOCR** backend (`--features paddle-ocr`),
    a more accurate OCR option selectable at runtime with `--ocr-engine paddle`. See
    [docs/change/2026-06-04-adopt-paddle-ocr.md](docs/change/2026-06-04-adopt-paddle-ocr.md).
  - *(alternative)* **leptonica** and **tesseract** — (`--features leptess-ocr`)

---

## concert-tracker

A SQLite-backed tool for tracking the full lifecycle of Tiny Desk Concerts: discovery → download → split.

### Web UI

```sh
concert-web [--db concerts.db] [--workdir .] [--port 3000]
```

Opens a local web UI at `http://localhost:<port>` built with axum, htmx, and askama templates.

#### Concert list

- **Card grid** of all concerts, grouped by month with divider headers
- **Per-card thumbnail**: small resized preview image (once metadata is scraped), served from
  the always-local `workdir/thumbnails/` dir so listing images keep working even after a
  concert is archived to a NAS (the full preview moves with the concert; the thumbnail stays)
- **Filter chips**: Wanted / Available / Ignored / Downloaded / Tracks
- **Per-card status badges** with color-coded left borders (blue = wanted, green = split, cyan = downloaded, purple = archived)
- **Per-card actions**: Want, Ignore, Download, Split, Archive, Delete download/split
- **In-progress auto-refresh**: cards with active jobs poll every 3 seconds
- **Month sync buttons**: fetch new listings from the NPR archive for any month

#### Concert detail

- **Auto-scrape**: automatically fetches full metadata (artist, description, set list, musicians, preview image) on first view
- **Re-scrape** button to refresh metadata from NPR
- **Preview image** display: the full-size preview is shown in the concert card (the listing
  uses the smaller thumbnail instead)
- **Track list** with per-track playback, watch, like (★), and delete buttons
- **Track splitter**: an inline, collapsible timeline editor for downloaded concerts.
  Drag a handle per track boundary, detach a shared split point into two handles to
  open a gap that belongs to no track (e.g. cut out talking), audition cut points
  against the album audio, then submit to re-cut the tracks (or reset to the
  automatic split). See
  [docs/change/2026-06-13-splitter-timeline-ui.md](docs/change/2026-06-13-splitter-timeline-ui.md).
- **Set list** display for concerts that haven't been split yet
- **Musicians** listing with instruments
- **Notes** field with save (persisted to DB)
- **Error history** for download, split, and archive failures
- **Event log** table showing all lifecycle events (listen, download, split, archive, etc.)
- **Link to NPR source page**

#### Media player

- **Persistent player bar** fixed to the bottom of the page
- **Album playback**: play the full downloaded concert file (audio or video)
- **Track playback**: play individual split tracks, with auto-advance to the next track
- **Back / Next buttons** to step to the previous or next playable track (each disables itself when there is nothing to go to)
- **Seek bar** and time display
- **Spacebar play/pause**: pressing Space toggles active playback when focus is outside ordinary page controls
- **Watch button**: plays video files inline in the player — a panel folds up from the player bar showing the video (a separate button opens the file in the system player, macOS `open`)
- **Like star** before the track title to like/unlike the currently-playing track, kept in sync with the track-list star
- **Delete button** (trash icon) removes the currently-playing track's files and advances to the next track (stops if nothing is next)
- **Now-playing indicator** on the currently playing track button

#### Jobs dashboard

- **Active jobs table** with concert, artist, job type (Download/Split/Archive), and start time
- **Cancel** button for running jobs
- **Failed jobs table** with error messages, filterable by job type (Download/Split/Archive)
- **Job log viewer** with full output for failed jobs
- **Live badge count** in the header nav, polling every 5 seconds

#### Settings

- **Archive location**: configure a directory for archiving concerts (e.g. NAS path)

#### Static file serving

- Concert files (downloads, split tracks) served from `workdir/concerts/` via `/concert-files/`

### CLI (`concert-db`)

```sh
# Sync listings from the NPR archive
concert-db sync                          # current month
concert-db sync --from 2024-01           # from month to current
concert-db sync --from 2024-01 --to 2024-12

# Scrape full metadata for a single concert URL
concert-db scrape <URL>

# Import metadata from existing *.json files (skips listing_* files)
concert-db import <DIR>

# Scan a directory for existing downloads and split dirs
concert-db scan <DIR>

# One-time backfill: import JSON + scan
concert-db init-from-files <DIR>

# Browse concerts
concert-db list
concert-db list --filter wanted

# Update intent
concert-db ignore <ID>
concert-db want <ID>

# Backfill listing thumbnails from existing preview images on disk
concert-db backfill-thumbnails

# Clear stale in-progress flags after an unclean shutdown
concert-db reset-in-progress

# Reset stale download errors on downloads that were deleted after erroring
# (one-time cleanup for concerts deleted before the error-on-delete fix)
concert-db clear-stale-download-errors
```

### Database schema

See [./doc/data.md](./doc/data.md) for an overview of the data model.

---

## scraper

Scrapes concert metadata from NPR pages and lists concerts from the archive.

```sh
# Scrape a single concert
cargo run --bin scraper -- <URL>

# List concerts from a month's archive
cargo run --bin archive_scraper -- 2024 01
```

---

## live-set-song-splitter

Splits a downloaded concert MP4 into individual tracks.

```sh
cargo run --bin live-set-splitter -- <json_file> [output_dir]

# Optional: choose the OCR backend (default tesseract; paddle needs --features paddle-ocr)
cargo run --features paddle-ocr --bin live-set-splitter -- <json_file> --ocr-engine paddle

# Optional: frame-accurate video cuts (slower, re-encodes video). Default is `copy`.
cargo run --bin live-set-splitter -- <json_file> --video-cut-mode reencode
```

The JSON file uses the same format produced by the `scraper` crate.

### Video cut mode

`--video-cut-mode` controls how each track's video is cut from the source. Both modes
keep audio and video in sync:

| Mode | Speed | Cut precision | Notes |
|---|---|---|---|
| `copy` *(default)* | Fast, lossless | Snaps the start back to the nearest preceding keyframe (up to one GOP — a few seconds — early) | Stream copy; no re-encode |
| `reencode` | Slow | Frame-accurate at the detected start | Re-encodes video with x264; audio is still copied |

Both modes seek on the **input** side (`-ss` before `-i`). An earlier version placed
`-ss` *after* `-i` with `-c copy`, which let the video start at the first keyframe
*after* the cut while the audio started exactly at the cut — desyncing every track not
cut on a keyframe by up to one GOP. See
[docs/change/2026-06-06-video-audio-sync-fix.md](docs/change/2026-06-06-video-audio-sync-fix.md).

---

## Shell scripts

These aren't deleted anymore.

| Script | Description |
|---|---|
| `download.sh <URL>` | Download a concert with yt-dlp |
| `extract.sh <URL>` | Download + scrape + split in one step |

---

## Development

The workspace uses `rust-toolchain.toml` to pin the toolchain to Rust **1.87**, which matches the `Containerfile`. `rustup` will install it automatically on first use.

### Linting

A root `justfile` provides the standard lint targets:

```sh
just fmt          # auto-format
just lint         # fmt --check + clippy + ts-check + ts-verify (the full standard suite)
just clippy-all   # also lint the leptess-ocr code path (needs Tesseract/leptonica)
```

#### One-time hook setup (per clone)

```sh
just install-hooks
```

This sets `core.hooksPath = .githooks` so that:
- **pre-commit** runs `cargo fmt --check` (fast)
- **pre-push** runs `just clippy`, `just ts-check`, and `just ts-verify`
  (gates what leaves the machine)

### Frontend (TypeScript)

`concert-tracker/static/{player,playlists,splitter}.js` are **generated build
artifacts** — edit `concert-tracker/frontend/src/*.ts` instead. They're
compiled with [esbuild](https://esbuild.github.io/) into standalone IIFE
bundles with the same filenames (see `concert-tracker/frontend/build.mjs`'s
header comment), committed to the repo so `cargo build` stays Node-free, and
guarded by a drift check (`just ts-verify`, wired into `just lint` and the
pre-push hook) that fails if the committed `.js` doesn't match a fresh build.

```sh
cd concert-tracker/frontend && npm install   # one-time
just ts-build     # rebuild static/*.js from frontend/src
just ts-check     # strict tsc --noEmit (frontend + js-tests)
just openapi-types  # regenerate frontend/src/generated/openapi.d.ts from the
                     # backend's live OpenAPI spec, after changing a
                     # #[utoipa::path]/ToSchema in concert-tracker/src/web
```

See `docs/change/2026-06-19-frontend-typescript.md` for the full design.

### Hot-reload dev server

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

## Building

```sh
cargo build --release
# Binaries: target/release/concert-db, concert-web, scraper, archive_scraper, live-set-splitter
```

## Testing

```sh
cargo test                    # all crates
cargo test -p concert-tracker # just the tracker
cargo test -p tiny-desk-scraper
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

#### Running in a restricted / proxied sandbox

In a sandboxed environment (e.g. the Claude Code sandbox) `npx playwright
test` fails out of the box with **Chromium failing to launch**. This is a
host/egress-proxy problem, not a test bug, and it requires fixes at two
independent layers: the Chromium *browser* process, and (if a test makes
outbound HTTP) the `concert-web` *server* process.

**1. One-time install — override `no_proxy` for the download:**

```sh
# The default no_proxy excludes *.googleapis.com, which breaks the
# cdn.playwright.dev → storage.googleapis.com redirect the installer follows.
# Lowercase no_proxy specifically — undici/proxy-from-env prefer it over
# NO_PROXY, and an empty string is falsy in JS and falls through to the
# unhelpful default, so "localhost" is the minimal value that works.
no_proxy="localhost" npx playwright install chromium
```

**2. Already wired in — Chromium launch args.** `playwright.config.js` and
`e2e/fixtures.js` already pass the args below to every browser launch, so
`npx playwright test` should work once the browser is installed. They're
documented here so the reasons are visible if they ever need to be touched:

| Flag | Why |
| --- | --- |
| `--single-process` | The sandbox blocks the Mach-port IPC Chromium normally uses between its processes; without this every launch dies immediately with `bootstrap_check_in org.chromium.Chromium.MachPortRendezvousServer: Permission denied (1100)`. Runs browser + renderer + GPU in one process instead. |
| `--no-proxy-server` | Chromium connects directly to the test's `127.0.0.1` server instead of routing through the egress proxy. |
| `--autoplay-policy=no-user-gesture-required` | Lets the player start tracks programmatically (auto-advance, back/next) without a real user gesture. |

`--single-process` has two structural consequences elsewhere in the suite,
so they aren't mistaken for bugs:
- `playwright.config.js` sets `workers: 1` — parallel single-process Chromium
  instances crash under CPU contention, so the suite is serialized.
- `e2e/fixtures.js` launches a **per-test** browser via a private
  `_ownBrowser` fixture instead of Playwright's worker-scoped `browser`
  fixture, because `--single-process` Chromium can crash during
  `browserContext` cleanup — isolating each test to its own browser keeps
  one crash from failing every subsequent test in the worker.

**3. Server-side proxy flags (only needed for manual/outbound runs).** The
`concert-web` binary (`concert-tracker/src/bin/concert_web.rs`) has its own
proxy flags, separate from Chromium's:

- `--no-proxy` — build HTTP clients with no proxy (direct egress). Skips
  reqwest's macOS SystemConfiguration proxy lookup, which is blocked (and
  panics) in some sandboxes.
- `--proxy-from-env` — build HTTP clients using `HTTPS_PROXY`/`HTTP_PROXY`/
  `ALL_PROXY` from the environment while still skipping the SystemConfiguration
  lookup. Mutually exclusive with `--no-proxy`.

The e2e fixtures do **not** pass either flag — the test server never makes
outbound HTTP calls, so there's nothing to proxy. They matter when running
`concert-web` by hand in a sandbox (e.g. for manual verification), or for any
future test that exercises scraping:

```sh
target/debug/concert-web --db test.db --workdir /tmp/tds --port 0 --no-proxy
```

**4. Troubleshooting, if it still fails:**

- **`bootstrap_check_in … MachPortRendezvousServer: Permission denied
  (1100)`** — `--single-process` is missing or was stripped from the launch
  args.
- **Install fails with `EAI_AGAIN` / DNS error** — use the
  `no_proxy="localhost"` override above; an empty or unset `no_proxy` isn't
  enough (see the note in step 1).
- **First test in a worker passes, the next fails almost instantly with
  `Target page, context or browser has been closed`** — the shared
  worker-scoped browser died; check that `e2e/fixtures.js`'s `_ownBrowser` /
  `context` / `page` fixtures (step 2) haven't been reverted to the
  Playwright defaults.
- **Flaky failures that pass solo but fail in a full run** — usually one of:
  media ending mid-test and auto-advance reacting (set
  `document.getElementById("player-audio").loop = true` for tests that need
  playback to persist, or don't assert on real video decode); or a real
  pointer move crossing a hover-reactive card crashing single-process
  Chromium (use `locator.evaluate(el => el.click())` / `dispatchEvent`
  instead of pointer movement for tests whose subject is event *logic*).
- **Every launch dies within seconds with `SIGTRAP` (exit code 133), even
  running `chrome-headless-shell` directly with no Playwright involved** —
  this is a host/container-level regression below all the workarounds above,
  not something fixable from inside a test run. Confirm with a direct-binary
  repro (run `chrome-headless-shell --headless --no-sandbox --single-process
  --no-proxy-server --disable-gpu about:blank` outside Playwright) before
  spending time on fixture changes; if confirmed, fall back to API-level
  (`curl`) verification for anything that doesn't strictly require
  pixel/interaction checks.
