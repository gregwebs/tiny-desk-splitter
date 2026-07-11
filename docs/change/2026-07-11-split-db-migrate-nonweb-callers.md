# Split SQLite Persistence: Migrate Non-Web Callers to Domain Paths

Implements [#67](https://github.com/gregwebs/tiny-desk-splitter/issues/67), a
*migrate* step of the wider `db.rs` domain split
([#69](https://github.com/gregwebs/tiny-desk-splitter/issues/69)), parallel to
[#66](https://github.com/gregwebs/tiny-desk-splitter/issues/66) (the web
migration batch) and following [#63](https://github.com/gregwebs/tiny-desk-splitter/issues/63)–[#65](https://github.com/gregwebs/tiny-desk-splitter/issues/65)
(expand + move domain persistence behind the facade). Call-path-only change:
no persistence logic, error propagation, logging, event emission, or
transaction-scope changes; no facade export removed.

## Scope

Pointed every non-web caller at its owning `db::<domain>::...` module instead
of the temporary top-level facade in `concert-tracker/src/db/mod.rs`. Before
this change, all non-web code (100% of it) called through the facade; after,
zero non-web call sites, type paths, or `use` imports reference the top-level
facade.

18 files migrated:

- **jobs**: `src/jobs/{mod,archive,download,prepare,scrape_queue,split}.rs`
- **lifecycle/scan/sync/scrape/import/normalize**: `src/lifecycle.rs`,
  `src/scan.rs`, `src/sync.rs`, `src/scrape.rs`, `src/archive_import.rs`,
  `src/normalize.rs`
- **events/split timestamps/playlist expansion**: `src/events.rs` (including
  the three concert reads named in #67's acceptance criteria — `list_concerts`
  at the two backfill call sites and `get_concert` in the split-tracks event
  backfill, all now `db::concerts::...`), `src/split_timestamps.rs`,
  `src/playlist.rs`
- **CLI binaries + fixture example**: `src/bin/concert_db.rs`,
  `src/bin/organize_concerts.rs`, `examples/make_test_fixture.rs`

Left untouched, as designed (owned by #66/#68): `src/web/*`,
`src/bin/concert_web.rs`, `tests/web_integration.rs`, and `src/db/mod.rs`
itself (both the facade block and its shared `#[cfg(test)]` helpers, which
resolve facade names via `use super::*`).

## Scope decisions

- **Inline `#[cfg(test)]` modules migrated with their files.** #67's text
  keeps the facade "available for web code and tests", but #66 only owns
  *web* tests and #68 (contract) is a delete-only ticket — leaving non-web
  inline tests on the facade would strand them with no owning ticket. This
  matches #65's precedent of importing test helpers directly from
  `db::concerts` rather than through the facade. Several inline tests
  construct facade types directly (e.g. `db::NewListing { .. }` in
  `events.rs`, `jobs/split.rs`, `archive_import.rs`), so the migration
  covered type paths and `use` imports, not just calls.
- **`examples/make_test_fixture.rs` migrated too** — a non-web fixture
  generator nothing else owns before #68.

## Migration mechanics

All non-web call sites used the bare facade name (`db::get_concert`,
`crate::db::NewListing`, etc.) with a plain `use crate::db;` — no
domain-qualified paths existed anywhere outside `src/db/` before this change.
A scripted regex substitution (word-boundary matched, so `get_concert` never
matched `get_concert_opt`/`get_concert_by_url`) qualified every facade symbol
with its owning domain module in one pass per file group, followed by manual
review of every diff. Four files had grouped imports
(`use crate::db::{self, MetadataUpdate, NewListing};`) that needed splitting
by hand into a bare `use crate::db;` plus a domain-qualified type import
(`use crate::db::concerts::{MetadataUpdate, NewListing};`), since a mixed
group would have imported the wrong path depth.

`cargo fmt` re-wrapped a handful of lines that exceeded the column width once
domain module segments were added to call chains (e.g.
`db::concerts::get_concert_by_url(...).unwrap().unwrap().id` in `sync.rs`) —
purely cosmetic, no logic touched.

## State changes

None. This is a pure call-path reorganization; all SQL, transaction scopes,
event emission, and timestamp formats are unchanged — verified by identical
test counts before and after.

## Verification

- Baseline: `cargo test -p concert-tracker` — 479 lib tests + 76 integration
  tests, all passing (unchanged from #65's post-migration baseline).
- After migrating each of the four file groups: `cargo check -p
  concert-tracker` plus focused module tests, all passing (68 tests for
  jobs/lifecycle overlap, 126 for lifecycle/scan/sync/scrape group, 76 for
  events/split_timestamps/playlist group).
- Completeness check — grep for every facade symbol name (calls, type paths,
  and grouped imports) across `src` and `examples`, excluding `src/web/`,
  `src/bin/concert_web.rs`, and `src/db/`: zero matches.
- Final: `cargo test -p concert-tracker` — still 479 lib + 76 integration
  tests passing; `cargo check --workspace` passes; `just lint` passes with no
  warnings (after `cargo fmt` reflowed lines lengthened by domain
  qualification).
- Codex adversarial review of the plan: found the verification grep would
  have missed facade type paths and grouped imports, wrong repo-root-relative
  paths, an incomplete ownership table, an unstated precondition about #66
  not being merged yet, and an over-scoped manual smoke check. All folded
  into the plan before implementation.
- Manual: ran `cargo run --example make_test_fixture` (migrated) against a
  scratch workdir/db, producing a 6-concert fixture — exercises
  `connection::open`, `concerts::{upsert_listing, update_metadata,
  get_concert_by_url}`, `lifecycle::{try_mark_download_started,
  mark_download_succeeded, try_mark_split_started, mark_split_succeeded}`,
  `split_timestamps::{set_tracks_present, set_auto_split_timestamps,
  set_tracks_liked}`, and `failed_jobs::insert_failed_job`. Ran
  `concert-db list` (migrated) against that fixture DB and got the expected
  6-row listing — exercises `connection::open` + `concerts::list_concerts`.
  Started `concert-web` on a spare port with that fixture `--db` and a fresh
  `--workdir`: startup log showed `events` backfill running (0 events
  generated, since the fixture's own inserts already emit events — exercises
  the migrated `events` → `db::concerts` read path) and `/`, `/jobs`,
  `/playlists` all returned 200.

## Next steps

#66 migrates web handlers, `concert_web`'s app-state setup, and
`tests/web_integration.rs` to domain paths. #68 then deletes the facade block
in `src/db/mod.rs`, fixes the shared test helpers' now-unqualified imports
(noted since #64: `track_durations`'s facade-resolved
`get_split_timestamps` call — already resolved, since `track_durations`
moved into `db::split_timestamps` in #65), and documents the final module map
in `docs/backend-persistence.md`.
