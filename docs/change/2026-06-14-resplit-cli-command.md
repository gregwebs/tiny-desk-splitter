# `concert-db resplit` command

## Motivation

After recent improvements to the splitting algorithm (OCR, silence detection, smart video
cuts), concerts that were split under the old algorithm may have mis-aligned track cuts or
missing tracks. This command re-runs automated splitting across every concert that has been
split (or previously errored) but has **no** user-edited timestamps, making it easy to
batch-apply the improved splitter without touching any manually adjusted concerts.

## What changed

### New DB query (`src/db.rs`)

`list_resplit_candidates(conn)` — returns concerts matching all of:
- `user_split_timestamps_json IS NULL` (never manually edited)
- `split_started_at IS NULL` (not currently mid-split)
- `split_at IS NOT NULL OR COALESCE(split_errors_json, '[]') != '[]'` (was split or errored)

Ordered by id. Tested with 6 unit tests covering each inclusion/exclusion condition.

### New CLI subcommand (`src/bin/concert_db.rs`)

```
concert-db [--db concerts.db] [--workdir .] resplit [--dry-run] [--confirm]
```

**Flags:**
- `--dry-run` — list candidates with their status slugs; make no changes.
- `--confirm` — required to actually mutate the database (safety gate).

**Behaviour:**
1. Queries `list_resplit_candidates`.
2. If `--dry-run`, prints the list and exits.
3. Without `--confirm`, prints a warning with the count and `--db` path, and exits.
4. Warns on missing dependencies (`live-set-splitter`, `ffmpeg`).
5. Processes concerts **sequentially** (one heavy ffmpeg/OCR job at a time), reusing the
   existing `start_split` / `run_split` orchestration via a local tokio runtime.
6. Determines per-concert outcome by comparing `split_errors.len()` before and after:
   a failed re-split keeps the old `split_at` (status slug alone would misreport it as
   "split"), but always appends to `split_errors`.
7. Prints per-concert `OK` / `FAILED` / `SKIPPED (source file missing)` /
   `SKIPPED (in progress)` / `ERROR`.
8. Prints a final summary line (succeeded / failed / skipped / errored).

### Orchestration tests (`src/jobs/split.rs`)

Two new `#[tokio::test]` tests validate the success/failure detection logic:
- `resplit_success_reports_ok_via_error_count` — already-split concert, fake-analyze
  splitter → `split_errors.len()` unchanged → detected as succeeded.
- `resplit_failure_detected_via_error_count` — already-split concert, exit-1 splitter →
  `split_errors.len()` increased → detected as failed.

## Usage

```sh
# Preview which concerts would be re-split
concert-db --workdir /path/to/media resplit --dry-run

# Back up first, then run
cp concerts.db concerts.db.bak
concert-db --workdir /path/to/media resplit --confirm
```
