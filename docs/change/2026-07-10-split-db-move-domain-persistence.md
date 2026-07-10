# Split SQLite Persistence: Move Domain Persistence Behind the Facade

Implements [#64](https://github.com/gregwebs/tiny-desk-splitter/issues/64), the
first *migrate* step of the wider `db.rs` domain split ([#69](https://github.com/gregwebs/tiny-desk-splitter/issues/69)),
following [#63](https://github.com/gregwebs/tiny-desk-splitter/issues/63) (the
*expand* step, PR #72). Behavior-preserving code motion only: no schema, SQL,
transaction-scope, timestamp-format, event-name/payload, or error-behavior
changes.

## Scope

Moved five domains out of `concert-tracker/src/db/mod.rs` (3214 lines before
this change) into their own modules, in the checkpoint order the issue
specifies:

- `db::concerts` — `NewListing`, `MetadataUpdate`, `concert_from_row` (row
  mapping), listing/metadata/notes reads and writes, and the wanted/ignored
  intent toggles.
- `db::lifecycle` — download/split/archive state transitions, stale
  in-progress cleanup (`fail_in_progress_jobs`, `reset_in_progress`), and
  `list_resplit_candidates`.
- `db::split_timestamps` — `StoredSplitTimestamps`, auto/user timestamp
  persistence, track presence/liked state, and media-duration persistence.
- `db::sync` — synced-month persistence, `earliest_concert_date`, and the
  month-completeness predicate (`MONTH_END_SYNC_GRACE` / `list_fully_synced_months`).
- `db::failed_jobs` — `FailedJob` and failed-job insert/list/read.

Playlists (`create_playlist`, `add_playlist_item`, `PlaylistMembership`,
etc.) intentionally stay in `db/mod.rs` for now — that's #65.

The temporary compatibility facade added in #63 was extended with `pub use`
re-exports for every item moved in this step, so all existing `db::...` call
sites (handlers, jobs, scan, sync, scrape, CLI binaries) keep compiling
unchanged. The facade is still marked temporary and slated for removal in #68.

## Module map after this step

```text
db/
├── mod.rs            facade + playlists (moves in #65)
├── connection.rs      migrations, open/open_in_memory, pragmas   (#63)
├── settings.rs        Theme, Settings, settings reads/writes     (#63)
├── time.rs            now_string                                 (#63)
├── concerts.rs         NewListing, MetadataUpdate, concert reads/writes  (#64)
├── lifecycle.rs        download/split/archive transitions               (#64)
├── split_timestamps.rs StoredSplitTimestamps, tracks, media duration     (#64)
├── sync.rs             synced-month persistence                         (#64)
└── failed_jobs.rs       FailedJob                                       (#64)
```

## Cross-module dependencies introduced

Pure code motion surfaced a few intra-`db` call sites that now cross module
boundaries. Each resolves via a direct `use super::<module>::<item>;` (or
`use crate::db::<module>::<item>;` in test modules) rather than the facade,
so #68's facade removal won't silently break anything:

- `db::lifecycle::split_tracks_json` and `db::split_timestamps::toggle_track_liked`
  both call `db::concerts::get_concert`.
- `db::lifecycle` test helpers/tests reuse `db::split_timestamps`'s
  `set_auto_split_timestamps`/`set_user_split_timestamps`/`get_split_timestamps`
  and its `pub(crate)` `make_timestamps()` helper (for the intentionally
  cross-domain `clear_split_state_preserves_timestamp_columns` test).
- `db::sync` calls `db::time::now_string`.

One exception is **not** resolved this way, and is called out explicitly so
#68 doesn't miss it: `track_durations` (staying in `db/mod.rs` until #65)
calls `get_split_timestamps` unqualified, resolving it through the facade's
`pub use split_timestamps::{...}` rather than a direct import. #68 must add
`use split_timestamps::get_split_timestamps;` to `mod.rs` when the facade
block is deleted.

## Test moves

Existing `db.rs` unit tests moved into their owning domain module's
`#[cfg(test)] mod tests`, per the issue's acceptance criterion. Two
trigger-characterization tests (`insert_sets_updated_at_via_trigger`,
`update_bumps_updated_at_via_trigger`) moved into `db::connection::tests`
instead of `db::concerts`, since they characterize the audit-timestamp
triggers `db::connection::run_migrations` installs, not concert-domain logic.

Shared test helpers `listing`, `seed`, and `seed_with_album` stay
`pub(crate)` in `db::tests` (`mod.rs`) — each is used by three or more
sibling domain test modules, so the parent module is the smallest module that
reaches every user. `make_timestamps` became `pub(crate)` inside
`db::split_timestamps::tests` for the same reason, scoped to just the two
modules that need it. `concert_ts`, previously `pub(crate)` in `db::tests`
for #63's `db::connection` tests, is now private again inside
`db::connection::tests` — its only remaining users moved there with it.

`concert_from_row` became `pub(super)` in `db::concerts` (i.e.
`pub(in crate::db)`), the one sanctioned non-test visibility change, so
sibling modules `db::lifecycle` and `db::split_timestamps` can map query rows
without going through the facade.

## New event characterization coverage

The issue requires characterization coverage for "emitted event type,
emitted JSON payload, and guarded updates that should not emit events" —
this was previously untested for the download/split/archive lifecycle and
for concert intent/metadata writes. Added a shared `pub(crate) fn events_for`
helper in `db::tests` and 18 new tests:

- `db::lifecycle` (13 tests): event type and payload for each of the
  download/split/archive started/succeeded/failed/delete operations,
  including that `mark_split_succeeded`'s payload is the tracks JSON when the
  concert has a set list and `None` when it doesn't (two tests), and that the
  four guarded no-op paths (`try_mark_download_started` on a second call,
  `try_mark_split_started` without `downloaded_at`, `try_mark_archive_started`
  on an already-archived concert, `clear_archive_state` while archive is
  in-flight) emit nothing.
- `db::concerts` (4 tests): `upsert_listing` emits `import` only on insert,
  not on conflict-update; `update_metadata` emits `scraped`; `toggle_ignored`/
  `toggle_wanted` emit their add/delete event pairs.
- `db::split_timestamps` (1 test): `set_user_split_timestamps` emits
  `split_timestamps_user` with the timestamps JSON payload (the reset-guard
  case was already covered by an existing test).

These assert current behavior only (characterization), not new requirements.

## State changes

None. This is a pure module reorganization; all SQL, transaction scopes,
event emission, and timestamp formats are unchanged.

## Verification

- Baseline before starting: `cargo test -p concert-tracker db::` — 109
  tests pass; `cargo test -p concert-tracker` — 461 lib + 76 integration
  tests pass.
- After code motion (checkpoints 1–5, before adding new tests):
  `cargo test -p concert-tracker db::` — still 109 tests, now reporting
  under `db::concerts::tests::*`, `db::lifecycle::tests::*`,
  `db::split_timestamps::tests::*`, `db::sync::tests::*`,
  `db::failed_jobs::tests::*`, and `db::connection::tests::*`.
- After adding the 18 characterization tests: `cargo test -p concert-tracker`
  — 479 lib tests (461 + 18) + 76 integration tests, all passing.
- `cargo check --workspace` — passes.
- `just lint` — passes with no warnings.
- Manual: started `concert-web` on a spare port with a fresh `--db` and
  `--workdir`; confirmed startup migrations + event backfill (exercising the
  `db::connection` → `events` → `db::concerts::list_concerts` path), settings
  read/write round-trip for both `theme` (verified via the `data-theme`
  attribute) and `archive_location`, and that `/`, `/jobs`, and `/playlists`
  all return 200.
- engineering-lead Agent Review: plan review returned approve-with-changes
  (three test-helper domain-assignment errors — `make_timestamps` cross-domain
  placement, `seed_url` misassigned to `db::sync` instead of `db::lifecycle`,
  and `seed_with_album` incorrectly scoped to a single domain instead of the
  shared `db::tests` module — all corrected in the plan before implementation);
  implementation review returned Approve, confirming byte-for-byte code
  motion (all 54 moved public items plus both private helpers), facade
  completeness, that all 18 new characterization tests assert real behavior,
  minimal visibility changes, and matching test/lint verification counts.
