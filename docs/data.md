## Concert Files

Each concert lives in `concerts/<album>/` with these metadata files:

| File | Description |
|---|---|
| `concert.json` | Scraped metadata (artist, set list, musicians, source URL) |
| `timestamps.json` | Splitter output with detected song timestamps |
| `preview.jpg` | Thumbnail image from NPR |
| `.concert-split-published.json` | Exact filenames owned by the current Published Concert Split |
| `.concert-split-partial.json` | Exact titles, timing, and filenames owned by a Recoverable Partial Split |
| `.concert-split-backup/` | One retained previous Published Concert Split used for ordinary publication rollback |

The directory also contains the full concert video (`<album>.mp4`) and, after
splitting, per-song tracks (`<Song Title>.mp4` / `.m4a`).

After archiving, the directory is moved to `<archive_location>/<album>/` and
a symlink replaces the original path. If the archive is unreachable (e.g. NAS
offline), track info is reconstructed from `split` and `track_delete` events.

## Sync

NPR concerts are scraped and synced to a local SQLite DB in a concerts table.
This is initiated by the user for an individual month, via a Sync button shown
on the index page's month divider. Each sync is recorded as a `(year, month,
synced_at)` row in `synced_months`.

A month's Sync button is hidden once that month counts as *fully synced* —
which requires a sync recorded **after the month has ended** (plus a small
grace window for the UTC/US-Eastern offset), not merely a sync at any point
during the month. Otherwise a sync run mid-month would hide the button before
NPR published that month's remaining concerts, with no way to re-sync. The
current month's button is therefore always shown. See
`concert-tracker/src/db.rs::list_fully_synced_months` and
`./change/2026-07-01-month-sync-completeness.md` for the implementation.

Detailed scraping is also initiated by going to the concert detail page or clicking download.

## Concert State

The concert has a download state, a split state, and an archive state.
The download can be deleted and individual tracks or all tracks can be deleted.
Archiving moves the concert directory to a configurable location and creates a symlink.
State-transition policy for destructive lifecycle operations lives in
`concert-tracker/src/lifecycle.rs`; HTTP handlers keep request/response behavior
such as confirmation prompts and HTMX rendering.
Concert filesystem media facts (downloaded source lookup, split-track lookup,
interlude lookup, all-tracks-present, reconstruction-item construction, source
redundancy, and track detail media facts) live in the **Concert Media
Inventory**, `concert-tracker/src/concert_media.rs`, whose primary test seam is
`ConcertMediaInventory`. Playback policy — plan selection (source vs.
reconstruction), playback-facing response structs/errors, and next/prev
playable-track policy — lives in `concert-tracker/src/playback.rs` and calls
into `concert_media` for the underlying facts. HTTP handlers translate those
facts into JSON, templates, and status codes. See
`docs/change/2026-07-09-concert-media-inventory.md` for the module boundary
and migration notes.
Split timestamp editing workflow lives in `concert-tracker/src/split_timestamps.rs`:
it validates timestamp payloads, reads/backfills stored automatic and user
timestamps, probes source duration for the editor, and starts user/reset split
jobs. HTTP handlers translate those outcomes into route status codes.

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
| `tracks_present` | TEXT | JSON `[bool, ...]` parallel to `set_list_json` — whether the track file is on disk. NULL when never set. |
| `tracks_liked` | TEXT | JSON `[bool, ...]` parallel to `set_list_json` — user "like" state per track. NULL when none liked. |
| `auto_split_timestamps_json` | TEXT | JSON `[SongTimestamp, ...]` — timestamps written by the automated Analyze split. Populated after a successful analysis, also lazy-backfilled from `timestamps.json` on disk for concerts split before this column existed. |
| `user_split_timestamps_json` | TEXT | JSON `[SongTimestamp, ...]` — user-submitted timestamps. Non-NULL iff the tracks on disk were cut by a user-supplied split; cleared by a successful Analyze split or a successful reset. |

`tracks_present` and `tracks_liked` index by position in `set_list_json`. This
assumes `set_list_json` is **append-only** after first scrape. If a re-scrape
reorders or replaces titles, both arrays will mis-attribute against the new
positions. The current scraper overwrites `set_list_json` wholesale, so this
is a known limitation.

A Recoverable Partial Split stores `tracks_present` for its exact salvaged
songs while leaving `split_at` NULL and appending a split error. Individual
tracks remain playable. Concert reconstruction and redundant-source deletion
require `split_at IS NOT NULL`, so a partial set cannot be mistaken for a
Published complete timeline.

### Lifecycle Transitions

Download deletion requires `downloaded_at` to be set. When the source file is
present, the file is removed, download state is cleared, download errors are
reset, and a `download_delete` event is recorded. Split state is preserved so
existing tracks can continue to play. When the source file is missing, the web
handler asks for confirmation unless `force=true`; forced deletion clears the
same download state without probing the file.

Redundant source deletion is a stricter download deletion path. The server
re-checks that every second of the source is covered by present song tracks and
interlude files using `tracks_present`, `user_split_timestamps_json`, and
`media_duration`. On success it removes the source file if present, clears
download state, and records both `download_delete` and
`source_redundant_delete`.

Split deletion is allowed for successful split state (`split_at IS NOT NULL`)
or split-error state (`split_errors_json` non-empty). It clears `split_at`,
`split_started_at`, `tracks_present`, and `split_errors_json`, then records
`split_delete`. Stored timestamp columns are not cleared by deleting split
state.

Track deletion validates the track index against `set_list_json`, removes known
track file extensions when possible, and ignores individual file deletion
errors after logging them. It records `track_delete`, marks that track absent
in `tracks_present`, and clears split state only when no tracks remain present.

Cancellation distinguishes four outcomes: a running task was aborted and marked
failed, a queued dependent was dropped before a task was spawned, a stale
database in-progress flag was marked failed, or no active job existed. Running
and stale cancellations append the relevant `*_error` with `cancelled by user`;
dropping a queued dependent records no lifecycle event because no task started.

Restart recovery marks stale in-progress rows failed instead of only clearing
flags: `download_started_at` becomes `download_error`, `split_started_at`
becomes `split_error`, and `archive_started_at` becomes `archive_error`, using
`server restarted` as the error text at web startup. The CLI
`reset-in-progress` command remains a manual escape hatch that only clears the
started flags.

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
* track_liked: JSON contains the track index and title
* track_liked_delete: JSON contains the track index and title
* listen
* wanted
* wanted_delete
* ignored
* ignored_delete
* archive_started
* archived
* archive_error
* archive_delete
* source_redundant_delete
* interlude_delete
* split_timestamps_user: JSON contains the user-submitted timestamps
* split_timestamps_reset: recorded when user column is cleared back to auto (only when it was non-NULL)

## Settings

Settings are stored in a singleton `settings` table (single row with `id = 1`).

| Column | Type | Description |
|---|---|---|
| `archive_location` | TEXT | Directory path for archived concerts (e.g. `/nas/media/music`) |
