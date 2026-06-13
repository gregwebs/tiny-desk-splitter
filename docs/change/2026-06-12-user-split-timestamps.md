# User-Supplied Track Split Timestamps

## Problem

Automated track splitting works well but users often want to nudge track
boundaries or cut out talking between songs. `live-set-splitter` already accepts
`--timestamps-file` (skipping the slow OCR/audio-analysis phase), but concert-web
had no way to invoke it with user-supplied timestamps and no place to store them.

## What Changed

### New API endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/concerts/:id/split-timestamps` | Returns set_list, auto timestamps, and user timestamps |
| POST | `/concerts/:id/split-timestamps` | Submit user timestamps; re-cuts tracks without re-analysis |
| POST | `/concerts/:id/split-timestamps/reset` | Reset to automated timestamps (no re-analysis) |

#### GET response shape

```json
{
  "set_list": ["Song A", "Song B"],
  "auto": [{"title": "Song A", "start_time": 0.0, "end_time": 180.5, "duration": 180.5}, ...],
  "user": null
}
```

`auto` and `user` are `null` when not available.

#### POST body shape

```json
{
  "songs": [
    {"title": "Song A", "start_time": 0.0, "end_time": 179.0},
    {"title": "Song B", "start_time": 195.0, "end_time": 400.0}
  ]
}
```

Gaps between songs are allowed (cutting out talking). Overlaps are rejected.
`duration` is computed server-side.

#### Status codes

- **202** — split job spawned
- **200** (reset only) — `{"status": "already-auto"}` when user column already NULL
- **422** — validation error (count mismatch, overlap, title mismatch, out-of-bounds, etc.)
- **409** — source file missing or a split job already running
- **404** — concert not found

### New DB columns (additive migration)

| Column | Description |
|--------|-------------|
| `auto_split_timestamps_json` | Timestamps from automated Analyze split; lazy-backfilled from `timestamps.json` on disk for pre-feature concerts |
| `user_split_timestamps_json` | User-submitted timestamps; non-NULL iff tracks on disk were cut by the user |

### State diagram

```
(auto, user)         action                   → (auto', user')      tracks on disk
─────────────────────────────────────────────────────────────────────────────────
(NULL, NULL)  ── analyze ok ───────────────── → (auto, NULL)        auto
(NULL, NULL)  ── lazy backfill ─────────────── → (auto, NULL)        auto (disk read)
(NULL, NULL)  ── user POST ok ──────────────── → (NULL, user)        user
(auto, NULL)  ── user POST ok ──────────────── → (auto, user)        user
(auto, user)  ── reset ok ──────────────────── → (auto, NULL)        auto (fast, no OCR)
(auto, user)  ── analyze ok ────────────────── → (auto', NULL)       auto' (new analysis)
(auto, user)  ── delete-split ──────────────── → (auto, user)        unchanged (files not deleted)
any           ── split job fails ────────────── → unchanged
```

Invariant: `user_split_timestamps_json` non-NULL ⟺ tracks on disk were cut with
those user timestamps. `delete-split` does not delete track files so the columns
survive it; the next successful Analyze split clears the user column.

### New events

- `split_timestamps_user` — recorded when user-submitted timestamps are stored
- `split_timestamps_reset` — recorded when user column is cleared back to auto
  (only when it was non-NULL; no spurious events)

### Shared wire type

`TimestampsFile { songs: Vec<SongTimestamp> }` moved from a private struct inside
`live-set-splitter` into `concert-types` so both crates share the
`--timestamps-file` format.

### Validation (`split_timestamps.rs`)

`ValidatedTimestamps` enforces:
- Exactly one timestamp per set-list song (positional, titles must match)
- `start_time >= 0`, `end_time - start_time >= 1.0s`
- No overlaps (gaps allowed)
- Every `end_time <= media_duration` (user POST; obtained via `ffprobe`)
- Reset path uses `validate_for_reset` (no duration needed; set-list mismatch
  returns a distinct error telling the user to re-run analysis)

### Known race

A user split followed immediately by a prepare-chained download/analyze will have
the queued Analyze split overwrite the track cut and clear the user column. The
invariant still holds afterward. This is documented but not prevented.

## Files changed

- `concert-types/src/lib.rs` — added `TimestampsFile`
- `live-set-song-splitter/src/main.rs` — switched to shared `TimestampsFile`
- `concert-tracker/Cargo.toml` — added `concert-types` dependency
- `concert-tracker/src/events.rs` — two new event variants
- `concert-tracker/src/db.rs` — two new columns, four new accessors
- `concert-tracker/src/split_timestamps.rs` — new validation module
- `concert-tracker/src/jobs/mod.rs` — `SplitMode` enum, `SplitJob` fields, `split_cmd` flag
- `concert-tracker/src/jobs/split.rs` — `start_split` mode handling, persistence, helpers
- `concert-tracker/src/jobs/prepare.rs` — updated call site
- `concert-tracker/src/web/handlers.rs` — three new handlers, `ffprobe_duration` helper
- `concert-tracker/src/web/mod.rs` — two new routes
- `concert-tracker/src/lib.rs` — `pub mod split_timestamps`
