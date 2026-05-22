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
# 1. Sync the archive and import any existing JSON files
concert-db sync --from 2024-01 --to 2024-12
concert-db import .

# 2. Open the web UI
concert-web --port 3000
# → http://localhost:3000
```

## Dependencies

- **yt-dlp** — download concert videos
- **ffmpeg** — split video into tracks
- **Rust** — build the project (`cargo build --release`)

---

## concert-tracker

A SQLite-backed tool for tracking the full lifecycle of Tiny Desk Concerts: discovery → download → split.

### Web UI

```sh
concert-web [--db concerts.db] [--workdir .] [--port 3000]
```

Opens a local web UI at `http://localhost:<port>`. Features:

- Filter by status: All / Wanted / Available / Ignored / Downloaded / Split
- Per-row actions: Want, Ignore, Download, Split, Re-scrape
- In-progress rows auto-refresh every 3 seconds
- Sync button fetches the current month's archive listings

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

# Clear stale in-progress flags after an unclean shutdown
concert-db reset-in-progress
```

### Database schema

Key columns in the `concerts` table:

| Column | Type | Description |
|---|---|---|
| `source_url` | TEXT UNIQUE | NPR concert page URL (primary key) |
| `ignored` / `wanted` | INTEGER | User intent flags (mutually exclusive) |
| `notes` | TEXT | Free-form user notes |
| `download_started_at` / `downloaded_at` | TEXT | Download lifecycle timestamps |
| `split_started_at` / `split_at` | TEXT | Split lifecycle timestamps |
| `download_errors_json` / `split_errors_json` | TEXT | Accumulating JSON error arrays |
| `set_list_json` | TEXT | `["Song Title", ...]` |
| `musicians_json` | TEXT | `[{"name": "...", "instruments": [...]}]` |

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
cargo test -p concert-tracker # just the tracker (53 tests)
cargo test -p tiny-desk-scraper
```
