# Keep the scrape queue separate from JobRegistry

Status: Accepted

The background metadata-scrape queue (`jobs::scrape_queue::ScrapeQueue`)
should remain its own protocol, not become a fourth `JobKind` folded into
`JobRegistry`/`jobs::run` (issue
[#129](https://github.com/gregwebs/tiny-desk-splitter/issues/129), the
read-only investigation that closes out the #124 deepening once #125–#128
made download, split, and archive share one Job Run engine end to end).

Scrape and Job Run are unlike protocols, not one protocol expressed twice.
Job Run admission is two-phase — an in-memory reservation followed by a
**persistent** started transition — and every accepted run reaches exactly
one terminal outcome (succeeded/failed/cancelled) arbitrated by a
`TerminalGate`, with transactional lifecycle/event/Failed-Job persistence,
`recover_failed` restart/shutdown recovery, explicit dependency edges, and
per-key concurrency (many runs at once, one per key). Scrape's "admission" is
a plain in-memory `HashSet` check-and-insert; it has no persistent started
column, no terminal-outcome arbitration, no cancellation, no recovery, and no
dependency edges. Its defining property — a single long-lived consumer
processing **one concert at a time**, deliberately, to avoid hammering NPR /
getting IP-blocked — is the opposite of what `JobRegistry` gives callers:
per-key concurrent admission. A restart simply drops the pending set; a
re-sync re-enqueues.

Applying the deletion test: folding scrape in would mean implementing the
7-method `JobRequest` trait (`validate`, `try_mark_started`, `setup`,
`execute`, `gather_success_facts`, `commit_success`, `record_failure`) with
most phases no-op or faked to fit a lifecycle scrape doesn't have — the
"fake download/split step" `docs/jobs.md` already calls out as the reason to
keep the boundaries apart. No orchestration logic is duplicated between the
two modules today, so nothing would reappear across callers if `ScrapeQueue`
were deleted; it isn't earning its keep as shared implementation, it's a
distinct implementation with a distinct interface. Consolidating would also
give `JobRegistry` a global-serial concept that no real Job Run needs,
enlarging a deep module's interface to serve one unlike caller instead of
deepening it.

The one fact genuinely shared between the two — **Failed Job** history — is
already shared at the correct seam: both write through
`db::failed_jobs::insert_failed_job` into the same `jobs` table and the same
Jobs page, scrape under the `SCRAPE_JOB_NAME` name. That sharing belongs at
the DB seam, not the orchestration seam, and moving it would not change this
decision. The existing Scrape Driver seam (`ScrapeItemFn`, satisfied by the
production `scrape_item` and, under Test Control, by
`test_control::scrape_driver::ScrapeDriver`) remains justified under the
skill's "two adapters means a real seam" rule and needs no change.

Should scrape ever need cancellation, a persistent `*_started_at`/recovery
story, or user-visible per-item retry/lifecycle UI, that would be a product
decision to make scrape a Job Run in its own right — not a refactor to reuse
`JobRegistry`'s existing shape. No such product need exists today, so no
consolidation ticket is opened. This decision does not identify any other
deepening opportunity in this area.
