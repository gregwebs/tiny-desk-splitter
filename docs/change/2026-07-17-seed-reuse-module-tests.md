# Reuse Database Seeds across persistence-backed module tests

Ticket: #122 — Spec: #121

## Context

The Database Seed API (`concert-tracker/src/db/seeds.rs`, `SeedContext`) was
already the canonical fixture vocabulary for Hurl/Test Control and for
`db::tests::seed`/`seed_with_album` (see
`docs/change/2026-07-13-db-seed-api-design.md`). Many co-located Rust module
tests still hand-rolled concert fixtures with local `upsert_listing` +
`update_metadata` helpers that duplicated a canonical seed's semantics. This
change migrates every helper/inline arrangement that exactly duplicates an
existing seed to `SeedContext`, keeps semantically distinct helpers local
(delegating canonical concert creation where exact equivalence holds), and
documents the convention canonically in `docs/backend-persistence.md`.

Test-setup and documentation refactor only. No schema, production API, or
runtime behavior change. The `cfg(any(test, feature = "test-control"))` gate
on `db::seeds` is unchanged.

## Migration inventory

Inventory taken via `rg` for every test-code `upsert_listing`/`update_metadata`
call site (named helper or inline), covering playlist, split-timestamp,
scanning, archive-import, job-oriented, sync, events, lifecycle, web-handler,
and test-control-driver test areas.

### A — Migrated to a canonical `SeedContext` call

| # | Location | Migration |
|---|---|---|
| 1 | `src/events.rs` `seed()` | Delegates to `db::tests::seed` (identical values) |
| 2 | `src/web/handlers.rs` `seed_listing()` | `SeedContext::seed_listing`, all fields explicit |
| 3 | `src/db/lifecycle.rs` `seed_url()` | `SeedContext::seed_listing`, `listing()`'s values explicit |
| 4 | `src/db/sync.rs` `earliest_concert_date_returns_min` | `SeedContext::seed_listing` × 2, explicit fields |
| 5 | `src/sync.rs` `import_listings_skips_existing_scraped_without_overwriting`, `import_listings_requeues_existing_unscraped_without_overwriting` | `SeedContext::seed_listing`; local `scrape()` transition helper unchanged |
| 6 | `src/playlist.rs` `seed_concert()` | `SeedContext::seed_scraped_concert`, explicit `concert_date: None` |
| 7 | `src/lifecycle.rs` `insert_concert()` | `SeedContext::seed_scraped_concert`, explicit `concert_date: None` |
| 8 | `src/scan.rs` `seed_concert_with_album()` | `SeedContext::seed_scraped_concert`, `set_list: Some(vec![])` |
| 9 | `src/split_timestamps.rs` `seed_ts_concert()` | `SeedContext::seed_scraped_concert`, explicit fields |
| 10 | `src/archive_import.rs` `setup_db_with_concert()` | `SeedContext::seed_scraped_concert`; signature changed to return `(Connection, i64)` |
| 11 | `src/jobs/download.rs` `seeded_db_with_set_list()`/`seeded_db()` | `SeedContext::seed_scraped_concert`; signature changed to return `(Arc<Mutex<Connection>>, i64)` |
| 12 | `src/jobs/prepare.rs` `seeded_db()` | Same as #11 |
| 13 | `src/jobs/scrape_queue.rs` `scrape_item_skips_already_scraped_without_recording_failure` | `SeedContext::seed_scraped_concert` (already consumed the returned id) |
| 14 | `src/scan.rs` `scan_skips_concerts_without_album` | `SeedContext::seed_listing`, explicit `None`s |
| 15 | `src/test_control/scrape_driver.rs` `seeded_concert()` | `SeedContext::seed_listing`, explicit fields |
| 16 | `src/test_control.rs` `reset_clears_concerts_and_settings_but_leaves_the_settings_row`, `reset_leaves_db_rows_intact_when_filesystem_cleanup_fails` | `SeedContext::seed_listing` |
| 17 | `src/db/lifecycle.rs:474,498` (inline duplicates of `seed_url`'s body) | Call the migrated `seed_url` |

### B — Stayed local, canonical part delegated where exact equivalence holds

| Helper | Reason |
|---|---|
| `src/db/playlists.rs` `seed_concert` | Row carries `teaser: Some("Great show")` **and** metadata; `seed_scraped_concert` always writes `teaser: None`, so full delegation would change fixture semantics. Delegates the listing to `seed_listing` (preserving the teaser), keeps the local `update_metadata` call. |
| `src/db/lifecycle.rs` `seed_downloaded` | Composes `try_mark_download_started`/`mark_download_succeeded` — the module's own functions under test. Delegates listing creation to the migrated `seed_url`. |
| `src/jobs/split.rs` `seeded_db` | Uses `lifecycle::set_downloaded_at_if_missing` (the scan-backfill path: no `downloaded_extension`, no Download event) — not equivalent to `seed_lifecycle_concert { downloaded: true }`. Delegates the scraped-concert part to `seed_scraped_concert`; the backfill call stays local. Also returns `(Arc<Mutex<Connection>>, i64)` now (previously assumed id `1`). |
| `db::tests::seed_with_album` | Sets `description: Some(...)` and a musician, a shape `seed_scraped_concert` cannot produce. Unchanged — already delegates via `seed()`. |
| `src/scan.rs` `seed_media_duration_concert` | Calls the migrated `seed_concert_with_album`, then a second `update_metadata` — kept as-is so the two-`metadata_scrape`-event history is preserved exactly. |
| `src/sync.rs` `scrape()` | A meaningful metadata transition on an already-seeded row, not a listing duplicate. |
| File/timestamp helpers (`scan.rs` `make_concert_dir`/`create_test_audio_sync`, `lifecycle.rs` `downloaded_file`, `split_timestamps.rs` `sample_timestamps`/`payload_for`, etc.) | Filesystem/domain-value arrangement that *is* the module contract under test, not a concert-fixture duplicate. |

### C — Not migrated (the fixture write is the subject under test, or production code)

- `src/db/concerts.rs` tests — `upsert_listing`/`update_metadata` are the functions under test.
- `src/events.rs` (beyond its migrated `seed()` helper) — `no_import_event_on_upsert_update`, `scraped_event_on_update_metadata`, and the `backfill` tests characterize exactly which events those write operations emit; the write is the subject.
- `src/sync.rs` `import_listings_inserts_new_and_returns_it_for_scrape`, `import_listings_inserts_undated_new_with_null_date_and_teaser` — `import_listings` (which wraps `upsert_listing`) is the subject under test.
- `src/scrape.rs` `apply_concert_info` — production code, not a test fixture.
- Inline `update_metadata`/`try_mark_*`/`set_*` calls composing module-specific state on an already-seeded row (`web/handlers.rs`, `db/split_timestamps.rs`, `db/playlists.rs`, etc.) — this is the arrangement pattern the new documentation section describes, not duplication to remove.

No new shared seed shape was introduced. No generic Test Database harness, fake persistence, database mock, or Test Control Assertion API dependency was added.

## Returned identifiers

Per the acceptance criteria, tests must consume returned Seed Result
identifiers rather than assume a generated row id. Four helpers previously
hardcoded concert id `1` (only correct because the fixture was the first
insert into a fresh database):

- `src/archive_import.rs::setup_db_with_concert` — now returns `(Connection, i64)`.
- `src/jobs/download.rs::seeded_db`/`seeded_db_with_set_list` — now return `(Arc<Mutex<Connection>>, i64)`.
- `src/jobs/prepare.rs::seeded_db` — same.
- `src/jobs/split.rs::seeded_db` — same.

Every call site (including `JobKey { concert_id: ... }`, `wait_for` polling
helpers, and `get_concert` assertions) was updated to thread the returned id
instead of the literal `1`. Verified by `rg -n "concert_id: 1|, 1\)|&conn, 1"`
returning no matches in the migrated files.

## Equivalence evidence

Three representative migrations (subtle optional/default fields) were
verified with a temporary scaffold test: two fresh in-memory databases — the
removed old arrangement on one, the new `SeedContext` call on the other —
comparing all deterministic `Concert` fields and the `db::tests::events_for`
event sequence, with `metadata_scraped_at` compared for presence only (it is
wall-clock generated):

- `playlist.rs` #6 (`concert_date: None`, `teaser` forced to `None` by
  `seed_scraped_concert`) — fields and event sequence matched; scaffold
  removed after the run passed.
- `web/handlers.rs` #2 (explicit teaser via `seed_listing`) — matched;
  scaffold removed.
- `jobs/scrape_queue.rs` #13 — matched; scaffold removed.

All three scaffolds passed on first run with no code changes needed, and were
deleted immediately after (they are not part of the final diff).

## Agent Review

**Plan review.** An adversarial engineering-lead review (Codex, via
`codex-companion.mjs task`) of the **implementation plan** found three
material gaps, all addressed before coding began:

1. **Incomplete inventory** — missed `scan.rs::scan_skips_concerts_without_album`,
   `test_control/scrape_driver.rs::seeded_concert`, and the two inline
   duplicates in `db/lifecycle.rs`. Added to the plan as items #14–#17 above.
2. **Assumed database ids** — `archive_import.rs` and the three `jobs::*`
   `seeded_db` helpers hardcoded concert id `1` instead of consuming a
   returned identifier. Fixed per "Returned identifiers" above.
3. **Stale canonical documentation** — the plan's docs update only added a
   new section, leaving `backend-persistence.md`'s existing claim that
   "nothing outside `db::seeds` and `crate::test_control` depends on it"
   false after the migration. Fixed by rewriting the dependency-direction
   paragraph to distinguish the unchanged production graph from the new
   `cfg(test)` dependents.
4. (Method refinement, not a plan gap) The equivalence-guard method was
   redefined to use two fresh databases instead of one shared database/URL,
   and to compare timestamp presence rather than string equality.

A follow-up non-adversarial review of the revised plan found the four
findings adequately resolved with no new issues, and approved the plan for
implementation.

**Code review.** An adversarial engineering-lead review of the resulting
diff (base `main`) found two issues:

1. **A group-B migration was planned but never applied.** `src/db/
   playlists.rs::seed_concert` still hand-rolled `upsert_listing` +
   `get_concert_by_url` instead of delegating the listing to `seed_listing`
   as item B (#55 above) specified — an implementation slip, not a plan
   error. Fixed: the listing is now seeded via `SeedContext::seed_listing`
   with `db::tests::listing`'s exact values (`concert_date:
   "2024-06-01"`, `teaser: "Great show"`), the returned id is consumed, and
   the local `update_metadata` transition is unchanged. Verified with
   `cargo test -p concert-tracker db::playlists::` (12 passed).
2. **Verification was recorded incompletely.** This Change Record initially
   cited only `cargo test` runs, not the plan-mandated `just test-rs`
   (nextest, full workspace), and had not yet logged the code-level
   adversarial review at all. Both fixed below.

The review found no fixture-equivalence defect elsewhere: migrated fields
preserve prior NULL/default semantics, the guarded normalization/reset
writes in `seeds.rs` are no-ops on fresh rows (preserving `updated_at` and
event history), and returned-id threading in `archive_import` and
`jobs::{download,prepare,split}` was complete with no remaining
generated-row-id assumption.

A follow-up non-adversarial review, run after applying the `playlists.rs`
fix and the verification-recording fixes below, confirmed both findings
resolved with no new issues.

## Verification

- Every migrated file's tests were run immediately after that file's
  migration (`cargo test -p concert-tracker <module>::`); all passed with no
  assertion changes required. This included a second pass on
  `db::playlists` after the code-review fix above (12 passed).
- Full suite, default features: `cargo test -p concert-tracker` — 518 passed, 0 failed.
- Full suite, `test-control` feature: `cargo test -p concert-tracker --features test-control` — 607 passed, 0 failed.
- `just test-rs` (cargo nextest, full workspace, all crates) — 783 tests run, 783 passed, 0 skipped.
- `cargo build -p concert-tracker` (default features, no `test-control`) —
  clean build, confirming `db::seeds` and the migrated test code (which is
  itself `cfg(test)`-gated) stay out of the production binary.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo clippy --all-targets --features test-control -- -D warnings` (concert-tracker) — clean.
- `cargo fmt --all -- --check` — clean.
- Documentation cross-references checked: `CONTRIBUTING.md` links to the new
  `docs/backend-persistence.md#testing-persistence-backed-modules` anchor;
  the new section links back to this Change Record.
- Live application and Playwright verification: **not applicable** — no
  production runtime behavior changed.
