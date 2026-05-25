## Concert Files

Each concert lives in `concerts/<album>/` with these metadata files:

| File | Description |
|---|---|
| `concert.json` | Scraped metadata (artist, set list, musicians, source URL) |
| `timestamps.json` | Splitter output with detected song timestamps |
| `preview.jpg` | Thumbnail image from NPR |

The directory also contains the full concert video (`<album>.mp4`) and, after
splitting, per-song tracks (`<Song Title>.mp4` / `.m4a`).

After archiving, the directory is moved to `<archive_location>/<album>/` and
a symlink replaces the original path. If the archive is unreachable (e.g. NAS
offline), track info is reconstructed from `split` and `track_delete` events.

## Sync

NPR concerts are scraped and synced to a local SQLite DB in a concerts table.
This is initiated by the user for an individual month with a synced_months table.
Detailed scraping is also initiated by going to the concert detail page or clicking download.

## Concert State

The concert has a download state, a split state, and an archive state.
The download can be deleted and individual tracks or all tracks can be deleted.
Archiving moves the concert directory to a configurable location and creates a symlink.

Key columns in the `concerts` table:

| Column | Type | Description |
|---|---|---|
| `source_url` | TEXT UNIQUE | NPR concert page URL (primary key) |
| `ignored` / `wanted` | INTEGER | User intent flags (mutually exclusive) |
| `notes` | TEXT | Free-form user notes |
| `download_started_at` / `downloaded_at` | TEXT | Download lifecycle timestamps |
| `split_started_at` / `split_at` | TEXT | Split lifecycle timestamps |
| `archive_started_at` / `archived_at` | TEXT | Archive lifecycle timestamps |
| `download_errors_json` / `split_errors_json` / `archive_errors_json` | TEXT | Accumulating JSON error arrays |
| `set_list_json` | TEXT | `["Song Title", ...]` |
| `musicians_json` | TEXT | `[{"name": "...", "instruments": [...]}]` |

## Events

Events are recorded in an immutable events table.
There are `inserted_at` and `updates` columns that are only useful for tracking our data migrations.

If the events table is deleted, that should not affect the functionality of the app.
The concerts table needs to maintain the download/split state.
It is okay to duplicate data between the 2 tables.

Events recorded:

* import
* scraped
* download_started
* download_error
* downloaded: downloaded_at
* download_delete
* split_started
* split: JSON contains the track names generated
* split_error
* split_delete
* track_delete: JSON contains the track number and name
* listen
* wanted
* wanted_delete
* ignored
* ignored_delete
* archive_started
* archived
* archive_error

## Settings

Settings are stored in a singleton `settings` table (single row with `id = 1`).

| Column | Type | Description |
|---|---|---|
| `archive_location` | TEXT | Directory path for archived concerts (e.g. `/nas/media/music`) |
