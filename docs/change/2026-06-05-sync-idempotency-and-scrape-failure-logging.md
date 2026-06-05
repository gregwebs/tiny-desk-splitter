# Month-sync idempotency + visible scrape failures

Fixes three defects surfaced by pressing **Sync** for June 2026 (a month with ~no
listings): it imported 15 concerts — mostly **May** — re-scraped them, and 3
scrapes failed (NAS write errors) that never appeared in the **Failed Jobs** list.

Plan: `~/.claude/plans/i-pressed-sync-for-idempotent-pascal.md`.

## Root causes

1. **Cumulative archive → month bleed.** NPR's archive URL
   `…/archive?date=MM-DD-YYYY` returns *every* concert up to that date, so a June
   sync (`date=06-30-2026`) pulled in May and earlier.
   `sync_month` upserted all of them. (`scraper/src/archive_scraper.rs`)
2. **Re-sync was not idempotent.** `db::upsert_listing`'s
   `ON CONFLICT(source_url) DO UPDATE` overwrote `title`/`concert_date`/`teaser`
   with raw archive values (e.g. reverting a scraped-clean title) on every
   re-sync.
3. **Scrape failures were invisible.** The background scrape worker logged a
   `WARN` and returned; it never recorded the failure, so NAS write errors never
   reached the `jobs` table / Failed Jobs UI.

(Note: in the reported incident the DB was already empty *before* the sync —
startup logged `backfill: 0 concerts` and every row's `first_seen_at` equals the
sync instant — so the prior data loss happened outside this sync. These changes
make sync safe and idempotent so that class of loss can't recur via sync.)

## Changes

### Sync is now month-scoped and idempotent — `concert-tracker/src/sync.rs`

- `MonthPartition` + pure `listings_for_month(listings, ym)`: keep listings dated
  to `ym`, drop other months (the cumulative-archive bleed), and keep + **count**
  undated listings. A missing `<time datetime>` is treated as a likely NPR HTML
  format change: `sync_month` logs an `ERROR` (with sample URLs) but still imports
  the concert so nothing is silently dropped.
- `sync_month` = fetch → partition → log-undated → `import_listings` →
  `mark_month_synced` (the mark always runs, even when 0 new).
- `import_listings(conn, kept)` implements the rule **skip only existing AND
  scraped**:

  | DB state of `source_url`            | Action                                            |
  | ----------------------------------- | ------------------------------------------------- |
  | exists, `metadata_scraped_at` set   | skip entirely — no overwrite, not scraped         |
  | exists, never scraped               | re-queue for scrape, **fields left untouched**    |
  | new                                 | insert (records `Import`), queue for scrape       |

  So a re-sync only ever scrapes new + previously-failed concerts, and never
  rewrites an existing concert's listing fields. Pressing Sync on a half-scraped
  month "completes" it (retries the gaps).

  Atomicity: the per-listing check-then-act is not atomic at the DB level; it
  relies on `sync_month` running under the single global `Mutex<Connection>` held
  by `web::handlers::sync_month_handler` (documented in-code).

- `db::upsert_listing` is unchanged — still insert-or-update for its other callers
  (scrape, scan, archive_import, download, split, events). Only the sync path
  changed. The now-unused `sync::upsert_listings` wrapper was removed.

### Scrape failures now reach the Jobs page — `concert-tracker/src/jobs/scrape_queue.rs`

- `const SCRAPE_JOB_NAME = "scrape"` + `record_scrape_failure(conn, id, err)`
  (best-effort: a failed insert is logged, never propagated, so it can't mask the
  original scrape error).
- Called on each real failure path in `scrape_item` (skip-check error, fetch
  error, apply error), **keeping** the existing `WARN` logs. The already-scraped
  guard still records nothing (it's a normal no-op). Failures **append** one
  `jobs` row per attempt, matching download/split/archive.

### Jobs UI — `handlers.rs` + `templates/jobs.html`

- `jobs_list`: `"scrape"` failed-filter arm + `"scrape" => "Scrape"` label.
- A **Scrape** filter chip alongside Download/Split. Failed scrape rows have no log
  file; `job_log` already degrades to "Log file not found."

## Tests

`concert-tracker` lib suite (259 tests) green. New:
- `listings_for_month`: requested-month kept, other months dropped, undated
  kept + counted.
- `import_listings`: new inserted + returned; existing+scraped skipped with title
  preserved (regression guard for root cause #2); existing-unscraped re-queued
  with fields untouched (same row, no duplicate); undated-new inserted with NULL
  date/teaser.
- `record_scrape_failure` appends a row per call; the already-scraped skip records
  nothing.

## Verification notes

Live "Sync <month>" verification requires network access to npr.org (outside the
local sandbox). Recommended manual pass against a **copy** of `concerts.db` on a
separate port/workdir (never the real DB): seed May as synced+scraped, Sync June →
only new June listings import and May is untouched; re-Sync → existing+scraped not
re-queued while an existing+unscraped row is; force a workdir write failure → the
scrape shows under Failed Jobs (filter = Scrape).
