# Don't mark a month "fully synced" until it has ended

## Motivation

The index page shows a per-month "Sync" button (`concert-tracker/src/month_walk.rs`).
Once a month is "fully synced" — has a row in `synced_months` — its button
disappears, since re-fetching NPR's archive for that month is assumed to be
pointless.

That assumption breaks for the *current* month: syncing June while still in
June only captures concerts published so far. NPR keeps publishing new Tiny
Desk concerts throughout the month, including in its last few days. Once the
calendar rolled into July, June's `synced_months` row was already present, so
its Sync button vanished — permanently hiding the only way to fetch the
late-June concerts that were missed.

The fix: a month should only count as "fully synced" once a sync is recorded
**after that month has ended**, e.g. June becomes fully synced only once a
sync happens during July or later.

## What changed

`synced_months` (`migrations/0001_init.sql`) already stored a `synced_at`
timestamp per `(year, month)`, but nothing read it — presence of the row alone
meant "fully synced". Completeness is now decided at **read** time from that
timestamp instead of at write time, so the fix is self-healing: no migration
or backfill is needed, and any row (including June's) is just re-evaluated
against the new predicate.

- `db::list_synced_months` → renamed `db::list_fully_synced_months`
  (`concert-tracker/src/db.rs`). The query now excludes any row whose
  `synced_at` is before that month's end:
  ```sql
  SELECT year, month FROM synced_months
  WHERE datetime(synced_at) >=
        datetime(printf('%04d-%02d-01 00:00:00', year, month), '+1 month', ?1)
  ORDER BY year, month
  ```
  `'+1 month'` (SQLite date arithmetic) handles the December→January wrap
  without any Rust-side month math. `datetime(...)` also normalizes the
  project's known two-timestamp-format hazard defensively.
- A `MONTH_END_SYNC_GRACE = "+5 hours"` constant is bound as `?1`. `synced_at`
  is UTC, but NPR's publishing clock is US Eastern, so a bare UTC month-end
  boundary would mark a month complete a few hours before its Eastern end and
  reintroduce a smaller, sticky version of the same bug in that window. `+5h`
  covers the EST offset so the boundary is never early relative to Eastern,
  only up to ~1h late during EDT — late is harmless, early is the bug being
  fixed. (No `chrono-tz` dependency added; this is a fixed SQL offset, not a
  real timezone conversion.)
- `db::mark_month_synced_at(conn, year, month, synced_at)` is a new explicit
  write helper; `db::mark_month_synced` now delegates to it with
  `db::now_string()`. This lets tests inject a `synced_at` and exercise the
  completeness predicate deterministically, with no clock mocking.
- `sync::synced_months_set` (`concert-tracker/src/sync.rs`) updated to call
  the renamed function; no other logic changed. `sync::sync_month` still
  unconditionally records a sync via `mark_month_synced` on every request —
  recording the fact is still correct, only the derived "complete" judgment
  moved. `month_walk::build_month_items`'s `ym == *current` display exception
  is now redundant (the current month's `synced_at` is never past its own end)
  but left in place as harmless and already covered by its own test.

## Verification

- 8 new unit tests in `db.rs` pin down the predicate: mid-month sync excluded,
  post-month sync included, December→January wrap, re-sync after month end,
  and four cases at the `MONTH_END_SYNC_GRACE` boundary (just before, within,
  exactly at, and the ~1h EDT linger). The two pre-existing
  `mark_month_synced_*` tests were rewritten against `mark_month_synced_at`
  with post-month timestamps, since they previously asserted "synced == now"
  presence, which the new predicate now excludes.
- `cargo test -p concert-tracker` and `just lint` (fmt + clippy) both pass.
- Manual: ran `concert-web` against a scratch `--db`/`--workdir` (separate from
  the real database). Seeded `synced_months` rows for June 2026 (synced
  mid-month — the bug case) and May 2026 (synced properly after May ended),
  plus one concert row in each month. Confirmed via direct HTML inspection:
  July (current month) always shows its Sync button; June's button reappears
  even though it has a `synced_months` row; May's button stays hidden. Then
  updated June's `synced_at` to a July timestamp (simulating a re-sync) and
  confirmed its button disappeared on reload — exactly the intended rule.
- Reviewed by engineering-lead before implementation, who required the
  UTC/Eastern grace window (initial approach used a bare UTC boundary, which
  would have left a sticky ~4-5h gap) and the `mark_month_synced_at` helper
  for deterministic tests.

## Files changed

- `concert-tracker/src/db.rs` — `mark_month_synced_at` (new),
  `mark_month_synced` (delegates), `MONTH_END_SYNC_GRACE` constant,
  `list_synced_months` → `list_fully_synced_months` with the completeness
  predicate; tests rewritten/added
- `concert-tracker/src/sync.rs` — `synced_months_set` calls the renamed
  function
