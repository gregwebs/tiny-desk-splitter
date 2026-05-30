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

## Dependencies

- **yt-dlp** — download concert videos
- **Rust** — build the project (`cargo build --release`)
Splitting video into tracks
- **ffmpeg** — frame analysis
- **leptonica** and **tesseract** — OCR
- **imagemagick** - create black and white image to help with OCR

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
```

The JSON file uses the same format produced by the `scraper` crate.

---

## Shell scripts

These aren't deleted anymore.

| Script | Description |
|---|---|
| `download.sh <URL>` | Download a concert with yt-dlp |
| `extract.sh <URL>` | Download + scrape + split in one step |

---

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