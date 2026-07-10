# Split SQLite Persistence: Expand the Module Shell

Implements [#63](https://github.com/gregwebs/tiny-desk-splitter/issues/63), the
*expand* step of the wider `db.rs` domain split ([#69](https://github.com/gregwebs/tiny-desk-splitter/issues/69)).
Behavior-preserving code motion only: no schema, SQL, timestamp-format, event,
or error-behavior changes.

## Scope

- Converted `concert-tracker/src/db.rs` (3684 lines) into `concert-tracker/src/db/mod.rs`
  plus three new domain modules, moved verbatim:
  - `db::connection` — `open`, `open_in_memory`, pragma configuration,
    `run_migrations`, `backfill_audit_timestamps`, and the idempotent
    column/rename helpers.
  - `db::settings` — `Theme`, `Settings`, `get_settings`,
    `update_archive_location`, `update_theme`.
  - `db::time` — `now_string`.
- Everything else (concerts, lifecycle, split timestamps, sync, playlists,
  failed jobs, jobs bookkeeping, and their tests) stays in `db/mod.rs` for now;
  it moves in #64/#65.
- Added a temporary compatibility facade in `db/mod.rs` (`pub use` re-exports)
  so all 21 existing caller files keep compiling against `db::open`,
  `db::Theme`, `db::now_string`, etc. without any caller edits. The facade is
  marked for removal in #68.
- Moved the settings tests into `db::settings::tests` and the audit-backfill /
  migration-rerun / column-rename tests into `db::connection::tests`. Two test
  helpers (`seed`, `concert_ts`), previously private inside `db::tests`, are
  now `pub(crate)` so the new `db::connection::tests` module can reuse them —
  the only sanctioned visibility change in this refactor.
- Documented the dependency direction between `db::connection`, concert reads,
  and `events` as a module doc comment in `db/mod.rs` (see below), satisfying
  the #63 acceptance criterion ahead of #64 moving concert-read code.

## Dependency direction (documented, not yet enforced by module boundaries)

```text
db::connection::run_migrations
        │
        ▼
   events::backfill  ──────────▶  crate::db::list_concerts / get_concert
                                   (concert reads; still in db/mod.rs,
                                    moves to db::concerts in #64)
```

`events` may depend on concert **read** operations. `db::connection` may
depend on `events`. Concert read operations must never depend back on
`db::connection` internals or on `events` — otherwise the migration startup
path forms a cycle. This must hold before #64 relocates the concerts code.

## State changes

None. This is a pure module reorganization; all SQL, migrations, pragma
settings, event emission, and timestamp formats are unchanged.

## Verification

- `cargo test -p concert-tracker db::` — 109 tests pass (same count as
  baseline before the split); moved tests now report under
  `db::connection::tests::*` and `db::settings::tests::*`.
- `cargo test -p concert-tracker` — 461 lib tests + 76 integration tests pass.
- `cargo check --workspace` — passes.
- `just lint` — passes with no warnings.
- Manual: started `concert-web` on a separate port with a fresh `--db` and
  `--workdir` to confirm startup migrations, event backfill, and settings
  read/write still work end to end.
- engineering-lead Agent Review: approve-with-changes on the plan (two
  compile-breaking test-move gaps — `seed`/`concert_ts` visibility and a test
  that calls `run_migrations` directly — both incorporated before
  implementation); code changes matched the amended plan.
