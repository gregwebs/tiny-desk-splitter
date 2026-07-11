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

`Concert` itself (the read model returned by most `db::concerts` and
`db::lifecycle` queries) lives in `crate::model`, not in `db` — it's a
cross-cutting domain type, not owned by any one persistence module.

## Dependency direction

Domain modules depend on `connection` (for the `Connection` type) and on
`crate::events`, not the other way around, with one specific exception that's
easy to get backwards:

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
event's JSON payload). No other cross-domain-module dependencies exist among
`concerts`, `lifecycle`, `split_timestamps`, `sync`, `playlists`, `settings`,
and `failed_jobs`.

## Event emission invariants

Every state-changing operation that has a corresponding entry in the events
table (see the list in [`./data.md`](./data.md#events)) calls
`events::record_now` as the last step, after the SQL write succeeds. Two
patterns recur:

- **Unconditional writes** (`mark_download_succeeded`, `mark_split_failed`,
  `toggle_ignored`, ...) always record their event.
- **Guarded state transitions** (`try_mark_download_started`,
  `try_mark_split_started`, `try_mark_archive_started`,
  `clear_archive_state`) run an `UPDATE ... WHERE <guard>` and only record an
  event when `rows > 0` — i.e. when the guard actually matched and the state
  changed. A no-op call (e.g. starting a download that's already in progress)
  emits **no event**. This is deliberate: the events table is meant to be a
  true transition log, not a call log.

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

## Change history

See [`./change/2026-07-11-split-db-contract-facade.md`](./change/2026-07-11-split-db-contract-facade.md)
for this document's origin and the full `db.rs` → domain-module split series
(issues #63–#69).
