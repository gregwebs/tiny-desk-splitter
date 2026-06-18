# Backfill `media_duration` for existing concerts

## Motivation

`media_duration` was added after many concerts had already been split, so older
rows have it `NULL`. Two features stay dark for those concerts as a result:
- `model::source_redundant` — the "Delete redundant source" button never appears
  without a `media_duration`.
- `model::build_reconstruction` — falls back to songs-only (no interludes) without
  a `media_duration`.

This change adds a one-time CLI backfill, eligible for concerts that have a
present source file, or stored/on-disk timestamps, or a complete set of song
tracks.

## Safety rule

`source_redundant` derives a **tail** interlude `[max_song_end, media_duration]`
(`concert_types::derive_interludes`). If `media_duration` is set *below* the true
source length, that tail gap shrinks or vanishes, so `source_redundant` can return
`true` while real audio past `media_duration` is still uncovered — authorizing
deletion of a source file that's still needed.

**Load-bearing invariant: a persisted `media_duration` for a *present* source must
never be less than the true source length.** Concretely:

```
if source file present on disk:
    duration = ffprobe(source_file)       # accurate — the true length
    on ffprobe error: SKIP (leave NULL)   # never fall through to an undercount
                                           # while the source is still deletable
else (source file absent — nothing left to delete, so an undercount is safe):
    duration = max(end_time) over timestamps   # user → auto (DB) → timestamps.json
    elif every set-list track present & probeable:
        duration = sum(ffprobe(track_i))       # undercounts inter-song gaps
    else: SKIP
```

`db::set_media_duration` is already fail-closed (rejects NaN/Inf/≤0; only writes
`WHERE media_duration IS NULL OR <= 0`), so the backfill can never clobber a good
value, and re-running it is a no-op for already-backfilled concerts.

## What changed

### `model::decide_backfill_duration` (`src/model.rs`)

Pure decision function encoding the rule above. Illegal states are unrepresentable
by construction:
- `SourceState::Present(Result<f64>)` couples "source exists" to its ffprobe
  outcome, so `Present(Err(_))` is a single explicit arm that returns `None` — it
  can never fall through to `Timestamps`/`TrackSum`.
- `track_durations: Option<&[f64]>` is `Some` **only** when every set-list entry
  resolved to a file and probed successfully (all-or-nothing — never a partial
  sum that would undercount further than already accepted).

Returns `Option<(f64, DurationSource)>`; `DurationSource` (`Ffprobe` / `Timestamps`
/ `TrackSum`) records provenance for the dry-run report. 9 unit tests, including
the load-bearing case (`Present(Err)` ⇒ `None`) and a guard that `TrackSum`/
`Timestamps` are unreachable while the source is present.

### `scan::backfill_media_duration` (`src/scan.rs`)

Orchestrator: gathers inputs per concert (source file via
`jobs::find_downloaded_file` + the new `scan::ffprobe_duration_sync`; timestamps
via `db::get_split_timestamps` then on-disk `jobs::split::read_analysis_timestamps`;
track durations via `model::find_track_file` + ffprobe), feeds them to
`decide_backfill_duration`, and — when `apply` is `true` — calls
`db::set_media_duration`. `apply = false` is fully read-only, so dry-run and
confirm share one code path. Every per-concert failure (ffprobe error, a DB read
error on `get_split_timestamps`, a `set_media_duration` write error, missing
album) degrades to a `skipped` row and `continue`s — one bad row never aborts
the batch. Returns a `MediaDurationBackfillReport { planned, skipped }` for
printing. 6 integration tests with real temp DBs/workdirs and real
ffmpeg-generated audio files (Ffprobe / Timestamps / TrackSum rows, incomplete
track set skip, dry-run writes nothing, idempotent re-run).

`scan::ffprobe_duration_sync` is the synchronous counterpart of the existing async
`web::handlers::ffprobe_duration` (needed because the CLI binary has no tokio
runtime); the handler now delegates to it via `spawn_blocking` instead of
duplicating the subprocess logic.

### `db::list_concerts_missing_media_duration` (`src/db.rs`)

`SELECT * FROM concerts WHERE media_duration IS NULL` — mirrors
`list_concerts_needing_tracks_backfill`.

### New CLI subcommand (`src/bin/concert_db.rs`)

```
concert-db [--db concerts.db] [--workdir .] backfill-media-duration [--dry-run] [--confirm]
```

**Behaviour:**
1. Always builds the report (`apply = false`) and prints each planned row
   (`[id] title -> 123.4s (Ffprobe)`) and skip reasons.
2. `--dry-run` stops there — no changes.
3. Without `--confirm`, prints a warning with the count and `--db` path, and stops.
4. `--confirm` first runs `PRAGMA wal_checkpoint(TRUNCATE)` on the open connection
   (the DB runs in WAL mode per `db::open`, so a raw file copy alone could miss
   writes still sitting in `<db>-wal`), then backs up the database file — copies
   it alongside the original as `<db>.bak-<UTC timestamp>` and aborts if the
   checkpoint or copy fails — then re-runs with `apply = true` and reports the
   write count.

No `--force`/re-correct flag: `set_media_duration`'s fail-closed `WHERE` clause
means a wrong value can't be overwritten by re-running; the dry-run report and the
automatic backup are the safety net, and fix-ups are manual SQL.

## Usage

```sh
# Preview what would be backfilled
concert-db --workdir /path/to/media backfill-media-duration --dry-run

# Apply (backs up the database automatically)
concert-db --workdir /path/to/media backfill-media-duration --confirm
```

## Files changed

- `concert-tracker/src/model.rs` — `DurationSource`, `SourceState`,
  `decide_backfill_duration`; 9 new unit tests
- `concert-tracker/src/scan.rs` — `ffprobe_duration_sync`,
  `backfill_media_duration`, `MediaDurationBackfillReport`; 6 new integration tests
- `concert-tracker/src/web/handlers.rs` — `ffprobe_duration` now delegates to
  `scan::ffprobe_duration_sync` via `spawn_blocking`
- `concert-tracker/src/db.rs` — `list_concerts_missing_media_duration`
- `concert-tracker/src/bin/concert_db.rs` — `backfill-media-duration` subcommand,
  `backup_db_path`
