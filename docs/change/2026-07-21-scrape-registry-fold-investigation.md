# Investigate folding the scrape queue into JobRegistry

Investigates [#129](https://github.com/gregwebs/tiny-desk-splitter/issues/129),
the final, **read-only** sub-issue of
[#124 — Deepen concert job orchestration](https://github.com/gregwebs/tiny-desk-splitter/issues/124),
opened once #128 (contracting the legacy job execution protocol) closed and
download/split/archive fully shared the `jobs::run` engine. The issue asks
whether the background metadata-scrape queue should also fold into
`JobRegistry`, using the `/codebase-design` deep-module vocabulary — deletion
test, depth, locality, leverage, interface size, seam — without presuming
consolidation is the right outcome. No code changes were in scope; the
deliverable is a recommendation.

## What was compared

`concert-tracker/src/jobs/scrape_queue.rs`'s `ScrapeQueue` against
`concert-tracker/src/jobs/mod.rs`'s `JobRegistry` and
`concert-tracker/src/jobs/run.rs`'s `JobRequest`/`submit`/`cancel`/
`recover_failed` engine, across admission, concurrency model, terminal
outcomes, cancellation, persistence, recovery, dependency handling, and seam
justification. See the new ADR for the full comparison table and reasoning.

## Finding

Unlike protocols. `JobRegistry` gives two-phase (in-memory reserve +
persistent started) admission, per-key concurrency, exactly-one
`TerminalGate`-arbitrated terminal outcome, transactional lifecycle/Failed-Job
persistence, `recover_failed` restart/shutdown recovery, and explicit
dependency edges. `ScrapeQueue` is a single serial worker over an in-memory
pending `HashSet` with best-effort per-item completion and no persistent
started state, terminal arbitration, cancellation, recovery, or dependencies
— its single-worker seriality (deliberate, to avoid NPR IP-blocking) is the
opposite of `JobRegistry`'s per-key concurrency.

Applying the deletion test: folding scrape in would require a `JobRequest`
impl with most of its 7 phases no-op/faked — a shallow pass-through, not
shared implementation reappearing across callers. It would also force a
global-serial concept onto a deep module built for per-key concurrency that
no real Job Run needs (negative leverage). The one fact genuinely shared
today — Failed Job history — is already shared at the correct seam
(`db::failed_jobs::insert_failed_job`, both jobs and scrape write into the
same `jobs` table/Jobs page). The existing Scrape Driver seam
(`ScrapeItemFn`, production `scrape_item` + `test_control::scrape_driver::ScrapeDriver`)
remains justified under "two adapters means a real seam."

## Recommendation

Preserve the separate modules; do not consolidate. No consolidation ticket is
opened, and no alternative deepening opportunity in this area was identified.
Consolidation would only become worth revisiting if a future product decision
gives scrape its own cancellation, persistent started/recovery state, or
per-item lifecycle UI — at which point it would be evaluated as scrape
becoming a Job Run in its own right, not a reuse of `JobRegistry`'s existing
shape.

## Documentation

- New ADR:
  [`docs/adr/0006-scrape-queue-separate-from-job-registry.md`](../adr/0006-scrape-queue-separate-from-job-registry.md)
  records the decision so it is not re-litigated.
- `docs/jobs.md`'s "Scrape runner boundary" section now links the ADR.
- The recommendation was also posted as a comment on #129.
