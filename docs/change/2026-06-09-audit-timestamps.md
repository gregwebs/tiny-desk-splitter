# Audit timestamps (`inserted_at` / `updated_at`)

## Why

The DB had uneven, purpose-specific timestamps and no general "when did this row
last change?" signal, which made debugging and auditing harder. `concerts` had
`first_seen_at` and per-stage `*_at` columns but no overall `updated_at`; `jobs`
only had `failed_at`; `settings` had no timestamps at all.

## What changed

- **Rename:** `concerts.first_seen_at` → `concerts.inserted_at` (it already held
  the insert time). Applied to fresh DBs via `0001_init.sql` and to existing DBs
  via an idempotent `rename_column_if_exists` helper (`ALTER TABLE … RENAME
  COLUMN`, SQLite ≥ 3.25).
- **New columns:** `concerts.updated_at`; `jobs.inserted_at` / `jobs.updated_at`;
  `settings.inserted_at` / `settings.updated_at`. Added nullable via
  `add_column_if_missing` (SQLite `ADD COLUMN` cannot take a `datetime('now')`
  default), then populated.
- **Triggers** (`migrations/0003_audit_timestamps.sql`): `AFTER INSERT` sets the
  initial values; `AFTER UPDATE` bumps `updated_at = datetime('now')`. The app
  never sets these columns by hand, so none of the 27+ `UPDATE concerts SET …`
  statements changed.
- **Backfill** (`backfill_audit_timestamps`): `concerts.updated_at` ←
  `MAX(datetime(events.at))` (fallback `datetime(inserted_at)`); `jobs` ←
  `failed_at`; `settings` ← `datetime('now')`. Guarded by `… IS NULL`, so re-runs
  are no-ops. The `datetime(...)` wrapper is load-bearing — see the timestamp
  format note below.

Untouched: `events` (append-only, already has `inserted_at`) and `synced_months`
(already has `synced_at`).

## Key design points

- **The `AFTER UPDATE` `WHEN NEW.updated_at IS OLD.updated_at` guard** is what
  makes this safe. Ordinary app updates never mention `updated_at`, so the guard
  is true and the trigger bumps it. The one place that *does* set it explicitly —
  the backfill — trips the guard false, so the trigger stays quiet and the
  historical value is never overwritten with `now()`. This holds regardless of
  whether the triggers already exist when the backfill runs.
- **Recursion** is a non-issue: `configure()` now sets `PRAGMA
  recursive_triggers=OFF` explicitly (it is also the SQLite default), so the
  trigger body's own `UPDATE … SET updated_at` cannot re-fire any trigger. The
  pragma is set next to the trigger-bearing connection setup so the invariant
  isn't an implicit dependence on a global default.

```
ordinary UPDATE (no updated_at)   backfill UPDATE (sets updated_at)
        │                                  │
        ▼                                  ▼
NEW.updated_at IS OLD.updated_at   NEW.updated_at <> OLD.updated_at
   → WHEN true  → trigger fires       → WHEN false → trigger skipped
   → updated_at = now()               → explicit/historical value kept
```

### Timestamp formats (a real hazard)

The DB stores datetimes in **two non-interchangeable formats**: SQLite
`datetime('now')` space form (`2026-06-09 20:33:05`, used by column defaults,
the triggers, and the backfilled import/wanted/ignored events) and chrono ISO
form (`2026-06-09T20:33:05Z`, used by `events::record_now`). These are **not
lexicographically comparable** — the space byte `0x20` sorts before both digits
and `T` — so a raw string `MAX(events.at)` can return a chronologically *earlier*
row. The backfill therefore compares `MAX(datetime(at))`, which parses both
forms and re-emits canonical space format; this both fixes the ordering and
leaves `updated_at` in the same format the triggers write.

On the current production data the raw-`MAX` bug happens to be *latent* (the
space-format events are always a concert's earliest, so they never win a max),
but the normalized form is correct regardless of which format is latest.

**Deferred tech debt:** the project stores timestamps stringly-typed in two
formats. `updated_at` is internally consistent (triggers + backfill both space
form), but any *future* feature that string-compares `updated_at` against an
event `at` inherits the same hazard. The real fix is to unify on one format
project-wide (or parse into a typed value); that's out of scope here.

## Migration order (run_migrations)

1. `rename_column_if_exists(first_seen_at → inserted_at)`
2. `add_column_if_missing` for the new columns
3. existing `events::backfill`
4. `backfill_audit_timestamps`
5. create triggers (`MIGRATION_003`)

## Verification

- `cargo test` (268 lib tests incl. new trigger/backfill/rename tests, plus a
  mixed-format regression test where a later space-format event must beat an
  earlier `T`-format event — this fails against a raw `MAX(at)`).
- Migrated a copy of the real `concerts.db` (230 concerts / 1212 events /
  19 jobs): rename applied, all 5 triggers present, `PRAGMA recursive_triggers`
  = 0, 0 remaining NULLs, jobs `inserted_at == failed_at`. Chronological
  correctness checked **independently** of the backfill expression using
  `julianday()`: 0 concerts had any event later than `updated_at`, and 0
  mismatched the true `MAX(julianday(at))`. All 230 `updated_at` values are
  canonical space format. An ordinary `UPDATE` advanced a pinned `updated_at`. A
  second migration pass left every timestamp identical (idempotent). The real
  `concerts.db` was not modified.
