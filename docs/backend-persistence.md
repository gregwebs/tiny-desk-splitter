# Backend Persistence

`concert-tracker`'s SQLite access lives under `concert-tracker/src/db/`, split
into domain modules (issue
[#69](https://github.com/gregwebs/tiny-desk-splitter/issues/69)). Each module
owns a slice of the schema plus the query/mutation functions and, where
relevant, the request/response types for that slice. There is no top-level
`db::` compatibility facade — every caller uses a domain path
(`db::concerts::upsert_listing`, `db::lifecycle::mark_split_succeeded`, etc.).

See [`./data.md`](./data.md) for the schema itself (columns, event list,
lifecycle transition prose). This document covers the module boundaries,
dependency rules, and the invariants that hold across all of them.

## Module map and type ownership

| Module | Owns | Public types |
|---|---|---|
| `db::connection` | Opening a connection, running migrations, one-time backfills | — |
| `db::concerts` | Listing CRUD, metadata, notes, ignored/wanted flags, teaser backfill | `NewListing`, `MetadataUpdate` |
| `db::lifecycle` | Download/split/archive state transitions, in-progress job bookkeeping, restart recovery | — |
| `db::split_timestamps` | Stored auto/user split timestamps, media duration, per-track present/liked state | `StoredSplitTimestamps` |
| `db::sync` | Synced-month tracking, earliest-concert-date lookup | — |
| `db::playlists` | Playlist CRUD, item mutation, membership lookup, nested-playlist cycle validation | `PlaylistError`, `PlaylistMembership` |
| `db::settings` | Singleton settings row (archive location, theme) | `Theme`, `Settings` |
| `db::failed_jobs` | Job-failure audit log | `FailedJob` |
| `db::time` | `now_string()` — the one place Rust code formats a `concerts`-table timestamp | — |
| `db::seeds` (test-only: `cfg(any(test, feature = "test-control"))`) | Database Seed API — the shared fixture vocabulary for co-located Rust module tests crate-wide and for the Test Control API (`crate::test_control`) | `SeedContext`, `FixtureIds`, `SeedListing`, `SeedScrapedConcert`, `SeedLifecycleConcert`, `SeedMediaConcert`, `SeedAlbumNullConcert` |

`Concert` itself (the read model returned by most `db::concerts` and
`db::lifecycle` queries) lives in `crate::model`, not in `db` — it's a
cross-cutting domain type, not owned by any one persistence module.

The presentation-neutral `crate::concerts::Concerts` application module sits
above this persistence layer. It composes persistence with filesystem and
in-memory work observations into canonical Concert State; it does not change
the table ownership or dependency rules documented here. See
[Concert application interface](concerts.md).

## Dependency direction

Domain modules depend on `crate::events`, not the other way around, with one
specific exception that's easy to get backwards. (`connection` is not itself
a runtime dependency of the other domain modules — they take `rusqlite::
Connection` directly; `db::connection::{open, open_in_memory}` are only
imported by `#[cfg(test)]` code to construct one.)

`db::connection::run_migrations` calls `events::backfill`, which reads
concerts via `db::concerts::list_concerts`. So `events` may depend on concert
*read* operations, and `db::connection` may depend on `events` — but concert
read operations must never depend back on `db::connection` internals or on
`events`, or the migration startup path forms a cycle. Concert *write*
operations depending on `events` (e.g. `db::concerts::upsert_listing`
recording an `Import` event) is a separate, permitted relationship — the
cycle constraint is specifically about reads used during migration/backfill.

`db::lifecycle` depends on `db::concerts` for `get_concert`/`concert_from_row`
(e.g. `mark_split_succeeded` reads the concert back to build the `Split`
event's JSON payload). `db::split_timestamps` also depends on `db::concerts`
for the same two functions (used by `toggle_track_liked` and the
`list_concerts_needing_tracks_backfill`/`list_concerts_missing_media_duration`
queries). `db::sync` depends on `db::time` for `now_string`. No other
cross-domain-module dependencies exist among `concerts`, `lifecycle`,
`split_timestamps`, `sync`, `playlists`, `settings`, and `failed_jobs`.

`db::seeds` depends on `db::concerts`, `db::lifecycle`, and
`db::split_timestamps` (it composes their domain functions to build
fixtures). It is test-only and sits outside the *production* dependency graph
above: no production code and no non-test build depends on it. Within
`cfg(test)`, the relationship is inverted — `db::seeds` is the shared fixture
vocabulary, and co-located test modules across the crate (`db::lifecycle`,
`db::sync`, `playlist`, `lifecycle`, `scan`, `split_timestamps`,
`archive_import`, `jobs::*`, `web::handlers`, `sync`, `events`,
`test_control`/`test_control::scrape_driver`, ...) intentionally depend on it
to arrange concert fixtures — see
[Testing persistence-backed modules](#testing-persistence-backed-modules)
below. `crate::test_control` additionally depends on it at the
`test-control`-feature build, not only under `cfg(test)`. Direct SQL inside
`db::seeds` is limited to fixture *normalization* (resolving
`upsert_listing`'s `COALESCE`-on-conflict semantics and clearing stale
lifecycle/timestamp state on a reused `source_url`) where the equivalent
domain functions would record misleading events for state nothing real ever
produced — see the module's doc comment for details.

## Event emission invariants

Every state-changing operation that has a corresponding entry in the events
table (see the list in [`./data.md`](./data.md#events)) calls
`events::record_now` as the last step, after the SQL write succeeds. Two
patterns recur:

- **Unconditional writes** (`mark_download_succeeded`, `mark_split_failed`,
  `toggle_ignored`, ...) always record their event.
- **Guarded state transitions** (`try_mark_download_started`,
  `try_mark_split_started`, `try_mark_archive_started`,
  `clear_archive_state`, ...) run an `UPDATE ... WHERE <guard>` and only
  record an event when `rows > 0` — i.e. when the guard actually matched and
  the state changed. A no-op call (e.g. starting a download that's already
  in progress) emits **no event**. `split_timestamps::clear_user_split_timestamps`
  is the same pattern with a SELECT-then-conditional-record instead of a
  `rows > 0` check: it reads whether the user-timestamps column is currently
  set and only records `SplitTimestampsReset` when it was. This is
  deliberate: the events table is meant to be a true transition log, not a
  call log.

Read-only queries (`list_concerts`, `get_split_timestamps`,
`list_fully_synced_months`, ...) never record events.

## Transaction invariants

Most single-row mutations run as one implicit SQLite statement and don't need
an explicit transaction. Two `db::playlists` operations are the exception,
because they combine a validation read with a write that must be atomic:

- `add_playlist_item` opens an `unchecked_transaction()` (the connection is
  shared behind an `Arc<Mutex<Connection>>`, so no `&mut Connection` is
  available for the checked `transaction()` API) to make the reference/cycle
  validation and the `INSERT` atomic.
- `reorder_playlist_items` opens a `transaction()` to make the
  set-equality check (does the submitted id list exactly match the
  playlist's current items?) and the position `UPDATE`s atomic.

`db::connection::run_migrations` runs each migration step through
`execute_batch`, which prepares and steps each statement in the SQL string
one at a time — it is **not** a single transaction. Neither the migration
SQL files nor `run_migrations` open an explicit `BEGIN`/`COMMIT` around a
step, so each statement commits individually in SQLite's autocommit mode. A
migration step that fails partway through can leave earlier statements in
that step already committed. This is why migrations are written to be
idempotent (see `db::connection` tests) rather than relying on rollback: a
retried migration must tolerate objects it already created.

## Key state transitions

The `concerts` table tracks three independent lifecycles — download, split,
archive — each following the same started → succeeded/failed → cleared
shape. `db::lifecycle` owns all three:

```
        try_mark_*_started (guard: not already started)
                    │
                    ▼
             ┌─────────────┐
             │ *_started_at│
             │   is set    │
             └──────┬──────┘
                     │
        ┌────────────┴────────────┐
        ▼                         ▼
 mark_*_succeeded           mark_*_failed
        │                         │
        ▼                         ▼
 *_at set,                 *_started_at cleared,
 *_started_at cleared      error appended to *_errors_json
        │                         │
        └────────────┬────────────┘
                      ▼
              clear_*_state
     (clears *_at / *_errors_json;
      download-clear preserves split
      state so existing tracks still
      play — see data.md)
```

Restart recovery (`fail_in_progress_jobs`) enters this diagram from the
`*_started_at is set` box directly into `mark_*_failed`, using `"server
restarted"` as the error text — a stale in-progress row is never silently
cleared back to the initial state. The CLI `reset-in-progress` command is the
one path that does clear `*_started_at` directly without recording a failure,
as a manual escape hatch.

Full transition prose (which columns each clear step touches, cascading
effects on `tracks_present`, redundant-source deletion, track deletion) is in
[`./data.md`](./data.md#lifecycle-transitions) — this diagram is the shape,
that doc is the detail.

## Testing persistence-backed modules

Co-located tests (`#[cfg(test)] mod tests` inside the module under test) are
the default seam for a persistence-backed deep module. Crate-level
integration tests (`concert-tracker/tests/`) remain appropriate only when a
test intentionally crosses crate boundaries, e.g. `tests/web_integration.rs`
driving the HTTP surface.

Every module test owns a fresh, migrated SQLite database:

- **In-memory is the default** — `db::connection::open_in_memory()`. It is
  fast, fully migrated, and isolated per test with no shared state or
  rollback-based cleanup.
- **A unique temporary file-backed database** is used only when the behavior
  under test genuinely depends on multiple connections, locking, process
  restart, or on-disk durability — cases where an in-memory database can't
  exercise the real code path.

Tests never share a database across cases and never rely on transaction
rollback for isolation; coexistence and query-scoping behavior is tested by
deliberately seeding unrelated records in the same fresh database, not by
relying on ordering or a shared fixture.

`db::seeds::SeedContext` (gated `cfg(any(test, feature = "test-control"))`,
see the module map above) is the shared arrangement vocabulary:

```
 open_in_memory() ──migrations──▶ fresh schema
        │
        ▼
 SeedContext::new(&conn).seed_*(…)          ← arrange: canonical valid state
        │            (returns Concert — use its id/urls/titles, never assume)
        ▼
 module/domain interface calls               ← arrange: module-specific state
 (update_metadata, try_mark_*, set_*…)
        │
        ▼
 module operation under test                 ← act
        │
        ▼
 assert interface-visible behavior           ← observe (persistence reads next;
                                               raw SQL only for unobservable invariants)
```

- **Arrange with a seed, then compose module-specific state through the
  normal interface.** `SeedContext::seed_listing`/`seed_scraped_concert`/
  `seed_lifecycle_concert`/`seed_media_concert`/`seed_album_null_concert`
  cover a small set of canonical valid starting states. A test that needs
  more (e.g. a specific lifecycle transition, a set of unliked tracks) calls
  the relevant domain function (`db::lifecycle::mark_split_succeeded`,
  `db::split_timestamps::set_tracks_liked`, ...) after seeding — the same way
  product code would reach that state. Seed shapes are not extended with
  controls for every module-specific combination; a new shared seed is
  justified only by a recurring canonical need.
- **Consume the Seed Result.** Seed methods return the created `Concert` (or,
  for Test Control's adapter layer, an equivalent result type) — tests use
  its `id`, `source_url`, `title`, etc. rather than assuming a generated
  value or a specific row id. Relying on SQLite's `id`-allocation order (e.g.
  assuming a fresh database's first insert is always id `1`) is exactly the
  kind of incidental coupling this avoids.
- **Behavior-relevant fields are explicit on the seed call.** A field a test's
  assertions depend on (e.g. an explicit `concert_date: None` to pin down
  NULL vs. a generated default) is passed explicitly rather than left to
  whatever the seed's `Default` happens to produce, even when that default
  would also satisfy the test today.
- **Assert interface-visible behavior first.** A test asserts what the module
  under test returns or what a normal persistence read (`db::concerts::
  get_concert`, `db::split_timestamps::get_split_timestamps`, ...) reports.
  Direct SQL in a test is reserved for otherwise-unobservable storage
  invariants (e.g. asserting an exact `events` row, or a raw column with no
  read API) — the same restraint `db::seeds` itself follows for fixture
  normalization.
- **Database-only seeds for persistence-only modules; a Scenario Seed plus a
  test-owned temp directory when the module's contract includes files.**
  `seed_media_concert` writes sentinel (or, opt-in, real-audio) files under a
  caller-supplied workdir — use it, with `tempfile::tempdir()`, only when the
  test genuinely exercises filesystem behavior (track/interlude/preview
  files, legacy `timestamps.json`). A test that only needs database state
  should not write unrelated dummy files just because a media seed exists.
- **No generic Test Database harness or fixture-builder abstraction.** Tests
  call `db::connection::open_in_memory()` / `SeedContext::new` directly.
  Local helpers that encode a meaningful module-specific transition (a
  scan-backfill call, a lifecycle state machine walk, a file-arrangement
  helper) stay local, delegating only the canonical concert-creation part to
  `SeedContext` where doing so preserves the exact prior state (including
  `NULL`s, defaults, and emitted events).

See `docs/change/2026-07-17-seed-reuse-module-tests.md` for the migration
inventory that applied this convention across the existing test suite.

## Change history

See [`./change/2026-07-11-split-db-contract-facade.md`](./change/2026-07-11-split-db-contract-facade.md)
for this document's origin and the full `db.rs` → domain-module split series
(issues #63–#69).
