# Gap Interlude Tracks (make the source file redundant)

## Problem

The timestamp-adjustment UI lets users detach track boundaries to create gaps (cutting out
talking between songs). Previously that gap audio was **silently discarded** — `process_segments`
in the splitter `continue`d over non-song segments. As a result, once a concert was split the
original source file was still required for whole-concert playback and could not be deleted.

## Goal

Encode each gap as an explicit "interlude" track — a file cut at source fidelity (video+audio
for video concerts). Once song tracks + interlude tracks together cover the entire
`[0, media_duration]` timeline, the source file is **redundant**: no audio is lost and the
concert can be re-split from the individual files. The source can then be **safely deleted**
via a manual gated action.

Full-concert reconstruction playback (queuing song + interlude tracks in sequence when the
source is gone) is **deferred** to a later phase. This change delivers the files and the gate.

## Coverage / state model

```
timeline:  [0 ........................................ media_duration]
covered by: interlude? | song0 | interlude? | song1 | ... | interlude?
              head           inter-song               tail

Interludes = complement of song spans within [0, media_duration],
             each ≥ MIN_INTERLUDE_SECONDS (1.0 s).

source_redundant(concert) ⟺
    media_duration persisted
  ∧ user_split_timestamps present
  ∧ all tracks_present == true         (no song track deleted)
  ∧ every derive_interludes() entry has a file on disk
```

### Keyframe-snap note

The default "smart" video cut mode snaps each track's start back to the nearest preceding
keyframe. This guarantees **no audio hole** at a seam (the safe direction), at the cost of a
small song-over-interlude **overlap** at the seam boundary. Coverage derivation and the
deletability gate both use the **requested** boundaries, so keyframe snapping does not affect
the gate. De-duplicating the overlapping audio is a concern for the **deferred reconstruction
playback phase** only, not this change.

## What Changed

### `concert-types/src/lib.rs`
- `pub const MIN_INTERLUDE_SECONDS: f64` — minimum gap span worth cutting.
- `pub struct Interlude { index, start_time, end_time }` — one uncovered span.
- `pub fn interlude_filename_stem(index: usize) -> String` — the **single** formatter
  shared by the splitter (writer) and concert-tracker (finder), e.g. `"interlude_01"`.
- `pub fn derive_interludes(songs, media_duration) -> Vec<Interlude>` — pure, unit-tested;
  emits head/inter-song/tail spans ≥ `MIN_INTERLUDE_SECONDS` in time order.

### `live-set-song-splitter/src/main.rs`
- New `CutContext` struct groups the per-run cutting parameters (input/output paths, format,
  source_params, cut mode, concert), reducing `extract_track` from 11 to 6 args.
- New `extract_track(ctx, stem, start, end, title, track_number)` — shared helper used by
  both song and interlude cuts. Replaces the duplicate video+audio cut block.
- `remove_stale_interlude_files(output_dir)` — deletes `interlude_NN.(mp4|m4a)` using an
  anchored regex before (re-)emitting interludes; avoids stale orphans on re-split.
- `process_segments` updated to use `CutContext`; builds `song_timestamps` during the run,
  then calls `derive_interludes` + `extract_track` for each interlude when `emit_interludes`.
- New CLI flags: `--emit-interludes` (bool), `--media-duration <secs>` (optional; falls back
  to ffprobe of the input file).

### `concert-tracker/src/jobs/mod.rs`
- `SplitMode::UserTimestamps` is now a struct variant:
  `UserTimestamps { ts: ValidatedTimestamps, media_duration: f64 }`.
- `production` split command appends `--emit-interludes --media-duration {d}` for
  `UserTimestamps` mode only. Analyze/ResetToAuto do not emit interludes (auto boundaries
  chain end→start, so no inter-song gaps exist; auto splits remain non-deletable in v1).

### `concert-tracker/src/jobs/split.rs`
- `remove_stale_interlude_files(output_dir: &Path)` — mirrors the splitter version for
  Analyze/ResetToAuto pre-run cleanup (tracker-side, since those modes don't pass
  `--emit-interludes`).
- Before `run_split`: deletes stale interlude files for Analyze/ResetToAuto so the coverage
  gate stays consistent (a Reset clears coverage).
- On `UserTimestamps` success: persists `media_duration` via `db::set_media_duration`.

### `concert-tracker/src/db.rs`
- New column `media_duration REAL` (idempotent `add_column_if_missing` migration).
- `pub fn set_media_duration(conn, id, duration)` — persists only when `duration` is finite
  and positive; **never overwrites an existing good value** (SQL `WHERE media_duration IS NULL
  OR media_duration <= 0`), so a best-effort GET-path persist cannot clobber an authoritative
  post-split value.

### `concert-tracker/src/model.rs`
- `Concert` gains `pub media_duration: Option<f64>`.
- `pub fn find_interlude_file(working_dir, album, index) -> bool` — probes `interlude_NN.{mp4,m4a}`.
- `pub fn source_redundant(working_dir, album, tracks_present, user_timestamps, media_duration)
  -> bool` — pure gate function; **fails closed** on any absent input; calls `derive_interludes`
  and probes all required interlude files.

### `concert-tracker/src/events.rs`
- New `Event::SourceRedundantDelete` → `"source_redundant_delete"` (distinct audit trail from
  `DownloadDelete`).

### `concert-tracker/src/web/handlers.rs`
- `get_split_timestamps`: falls back to stored `media_duration` when the source file is absent
  (so the timeline editor stays functional after deletion; planned for the deferred phase).
- `set_split_timestamps`: passes `SplitMode::UserTimestamps { ts, media_duration }` with the
  ffprobe'd duration from the POST handler.
- New `delete_redundant_source` handler — re-checks `source_redundant` server-side (gate is
  never trusted from the client), removes the source file, calls `clear_download_state`,
  records `SourceRedundantDelete`, returns a refreshed card.
- `render_card` / `render_detail_card`: read stored user timestamps + compute `source_redundant`
  to thread `source_redundant: bool` into `RowTemplate`.
- `render_row` (listing cards): always passes `source_redundant: false` to avoid the extra DB
  read (button only useful on the detail page).

### `concert-tracker/src/web/mod.rs`
- New route `POST /concerts/:id/delete-redundant-source`.

### `concert-tracker/templates/concert_card.html`
- New button (rendered only when `source_redundant`):
  `🗑️ Source redundant` → `POST /concerts/:id/delete-redundant-source`.
- "Play album" disappears automatically after deletion because `clear_download_state` nulls
  `downloaded_at`, making `can_listen = false`.

## New DB column

| Column | Type | Description |
|---|---|---|
| `media_duration` | `REAL` | Source file duration in seconds (ffprobe); survives source deletion |

## Testing

- **concert-types**: 7 unit tests for `derive_interludes` (head/tail/inter-song/sub-threshold/
  none/full-coverage) and `interlude_filename_stem`.
- **concert-tracker lib**: 3 `set_media_duration` tests (roundtrip, invalid inputs, no-overwrite);
  7 `source_redundant` tests (fails-closed on absent inputs, missing song/interlude, full coverage
  with and without interludes, `find_interlude_file` mp4/m4a).
- Integration tests: 370 lib tests pass; 2 pre-existing web-integration failures unchanged.
- `just lint` clean.

## Files changed

- `concert-types/src/lib.rs` — new interlude types + `derive_interludes` + `interlude_filename_stem`
- `live-set-song-splitter/src/main.rs` — `CutContext`, `extract_track`, `remove_stale_interlude_files`, `process_segments` refactor + interlude emission; new CLI flags
- `live-set-song-splitter/Cargo.toml` — `regex = "1"` dependency
- `concert-tracker/src/jobs/mod.rs` — `SplitMode::UserTimestamps` struct variant, `--emit-interludes` in split command
- `concert-tracker/src/jobs/split.rs` — `remove_stale_interlude_files`, stale cleanup at split start, `media_duration` persistence
- `concert-tracker/src/db.rs` — `media_duration` column, `set_media_duration` accessor
- `concert-tracker/src/model.rs` — `Concert.media_duration`, `find_interlude_file`, `source_redundant`
- `concert-tracker/src/events.rs` — `SourceRedundantDelete` event
- `concert-tracker/src/web/handlers.rs` — GET fallback, UserTimestamps wiring, `delete_redundant_source` handler, `source_redundant` in card rendering
- `concert-tracker/src/web/mod.rs` — new route
- `concert-tracker/templates/concert_card.html` — "Delete redundant source" button
- `concert-tracker/Cargo.toml` — `regex = "1"` dependency
