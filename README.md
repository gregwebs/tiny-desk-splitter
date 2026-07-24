# Tiny Desk Splitter

Tools and a GUI for downloading and splitting NPR Tiny Desk Concerts into individual tracks with metadata.

## Quick start

```sh
cargo build
cargo run --bin concert-web
# → http://localhost:3000
```

## Development 

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, linting, and
testing.

See [CODING_STANDARDS.md](CODING_STANDARDS.md) for how code should be written and reviewed.

Repository documentation:

- [Architecture decisions](docs/adr/)
- [Change Records](docs/change/)
- [Job execution](docs/jobs.md): backend job execution and runner architecture
- [Concert Split interface](docs/concert-split.md): the synchronous `live-set-splitter` library interface, and `concert-web`'s in-process (default) vs. CLI subprocess adapter selection
  - [Publication and Recoverable Partial Split state diagram](docs/concert-split.md#published-and-recoverable-partial-output)
  - [Interrupted-publication recovery state diagram](docs/concert-split.md#interrupted-publication-recovery)
  - [ADR: availability-first publication with a durable recovery journal](docs/adr/0007-availability-first-concert-split-publication.md)
- [Domain language](CONTEXT.md): canonical vocabulary, including Job Request, Job Run, and Failed Job
- [Hurl tests](hurl/README.md): black-box HTTP tests and Test Control API
- `/health` self-identification endpoint (see `handlers::health` doc comment)
  - [ADR: text and JSON negotiation, why only JSON is in the OpenAPI schema](docs/adr/0008-health-endpoint-content-negotiation.md)
- [Data model](docs/data.md)

### Workspace crates

| Crate | Description |
|---|---|
| `scraper` | Scrape concert metadata from NPR pages; list concerts from the archive |
| `live-set-song-splitter` | Split a concert MP4 into individual tracks using FFmpeg |
| `concert-tracker` | SQLite-backed web UI and CLI to track download/split state across concerts |


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

See [docs/player.md](docs/player.md) for the player state model and the boundary between
event-derived model state and live browser media state.

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

See [./docs/data.md](./docs/data.md) for an overview of the data model, and
[./docs/backend-persistence.md](./docs/backend-persistence.md) for how the
persistence layer's Rust modules are organized.

---

## scraper

Scrapes concert metadata from NPR pages and lists concerts from the archive.

See [./scraper](./scraper/README.md)

---

## live-set-song-splitter

Splits a downloaded concert MP4 into individual tracks.

See [./live-set-song-splitter](./live-set-song-splitter/README.md)
