# OCI Container Images

**Date:** 2026-06-15

## Overview

Added a multi-stage `Containerfile` so newcomers can run the app without
installing Rust, a C++ toolchain, or the native OCR build dependencies.

Three OCI images are produced from a single `Containerfile` (standard OCI syntax,
compatible with Docker, Podman, Buildah, nerdctl):

| Target | Tag | Contents |
|---|---|---|
| `base` | `tiny-desk-base` | Runtime deps: ffmpeg/ffprobe, python3, yt-dlp |
| `dev` | `tiny-desk-dev` | base + Rust + C++ toolchain (development / CI) |
| `release` | `tiny-desk` | Compiled binaries + OCR models on top of base |

## Image layer diagram

```
debian:bookworm-slim
         │
         ▼
    ┌─────────┐   ffmpeg, ffprobe, python3, ca-certificates, yt-dlp (pinned)
    │  base   │
    └────┬────┘   tag: tiny-desk-base
         │ FROM base
         ▼
    ┌─────────┐   + rustup/stable, g++, clang/libclang-dev,
    │   dev   │     pkg-config, curl, git
    └────┬────┘   tag: tiny-desk-dev
         │ FROM dev
         ▼
    ┌─────────┐   cargo build --release  (intermediate, not published)
    │ builder │   produces target/release/* + live-set-song-splitter/models/
    └────┬────┘
         │ COPY --from=builder  (onto a fresh FROM base)
         ▼
    ┌─────────┐   /app/{concert-web,concert-db,live-set-splitter,scraper,
    │ release │     archive_scraper}  +  /app/models/*.mnn
    └─────────┘   tag: tiny-desk  (ENTRYPOINT concert-web)
```

## Build-time requirements

The default `paddle-ocr` backend downloads during `cargo build`:
- A prebuilt static MNN library from `github.com/zibo-chen/MNN-Prebuilds`
  (release tag `dev` — a moving tag; builds are not bit-for-bit reproducible)
- Three `.mnn` OCR model files from `raw.githubusercontent.com`

**Build-time egress to `github.com` and `raw.githubusercontent.com` is
mandatory.** `--network=none` builds will fail at the `cargo build` step.

## Building

```sh
# Build all three (auto-detects docker or podman):
./scripts/build-images.sh

# Or build a single target:
docker build --target base    -t tiny-desk-base .
docker build --target dev     -t tiny-desk-dev  .
docker build --target release -t tiny-desk      .

# Same with podman:
podman build --target release -t tiny-desk .
```

## Running

```sh
# Quick start (all data in a named volume at /data):
docker run --rm -p 3000:3000 -v tiny-desk-data:/data tiny-desk

# Or via Compose:
docker compose up
# (builds the image first if not yet built)

# Open http://localhost:3000
```

Persistent data under `/data`:
- `concerts.db` + WAL sidecars — the SQLite database
- `concerts/` — downloaded videos and split tracks
- `thumbnails/` — listing preview images
- `log/` — job logs

## Runtime volume/port/env contract

| Item | Value |
|---|---|
| Port | `3000` (EXPOSE 3000; mapped to host with `-p 3000:3000`) |
| Volume mount | `/data` (named or bind-mount) |
| DB | `/data/concerts.db` (default `--db`) |
| Workdir | `/data` (default `--workdir`) |
| Listen address | `0.0.0.0` (default `--host` in the image CMD) |
| Open command | `true` (no-op; `open` doesn't exist in a headless container) |
| OCR models | `/app/models` via `$PADDLE_OCR_MODEL_DIR` + sibling placement |

Override any CMD flag by appending it:
```sh
docker run --rm -p 8080:8080 -v tiny-desk-data:/data tiny-desk --port 8080
```

Access the CLI tools via `--entrypoint`:
```sh
docker run --rm --entrypoint /app/concert-db -v tiny-desk-data:/data tiny-desk list
docker run --rm --entrypoint /app/live-set-splitter tiny-desk --help
```

## Code change: `--host` flag

`concert-tracker/src/bin/concert_web.rs` previously hard-coded the bind address
to `127.0.0.1`, making the server unreachable from outside a container.

Added a `--host` clap argument (also read from the `HOST` env var) with a default
of `127.0.0.1` so local development is unchanged.  The release image CMD passes
`--host 0.0.0.0`.  The printed "Listening on http://..." line continues to use
`listener.local_addr()` (not `cli.host`) so the e2e fixture parser in
`e2e/fixtures.js` still matches.

## OCR model placement

`live-set-splitter` (not `concert-web`) runs OCR inference via the spawned
subprocess.  `resolve_model_dir()` in `live-set-song-splitter/src/ocr_paddle.rs`
resolves models in priority order:

1. `$PADDLE_OCR_MODEL_DIR` — set to `/app/models` in the release image
2. `models/` beside `current_exe()` — also `/app/models` since the binary is `/app/live-set-splitter`
3. `CARGO_MANIFEST_DIR/models` — source-tree fallback, not present in the runtime image

Both #1 and #2 point to the same `/app/models` directory; the `ENV` var is
belt-and-suspenders.

## yt-dlp version pinning

yt-dlp is installed from a pinned GitHub release via `ARG YT_DLP_VERSION` in the
`Containerfile`.  When YouTube changes break downloads, rebuild with a bumped version:

```sh
docker build --build-arg YT_DLP_VERSION=2025.12.01 --target release -t tiny-desk .
```

`python3` must remain in the base image — the generic yt-dlp GitHub binary is a
Python zipapp and requires Python at runtime.
