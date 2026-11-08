# Scrape requests behind Concerts

Issue [#164](https://github.com/gregwebs/tiny-desk-splitter/issues/164) asks for
scrape and rescrape interaction to move behind explicit `Concerts` methods while
preserving the Scrape Queue as a protocol distinct from Job Run. It continues
[Job Request outcomes](2026-11-09-job-request-outcomes.md) and the
[#177 contraction](2026-07-25-concert-catalog-and-status.md); the scrape half of
the move itself landed with
[Job Requests and scrape behind Concerts](2026-07-25-job-request-interface.md).

Auditing the ticket's acceptance criteria against `main` after those slices left
three criteria unmet and one only partly met.

- **"Immediate outcomes return current Concert State, including observable
  active scrape work."** `Concerts::enqueue_scrape` returned a bare
  `ScrapeAdmission`, so the sync adapter could learn *that* work was queued but
  not render it: a card's "loading…" placeholder reads
  `state.active_work.scrape_pending`, which needed a second query. The other two
  scrape methods already returned state, but `ensure_scraped` returned `()` and
  left its adapter (the detail page) to query again immediately.
- **"Interface-level tests cover admission, duplicate work, completion, failure,
  and refreshed metadata."** One test covered the admission vocabulary and one
  pair covered automatic-scrape tolerance against a real unreachable URL. Nothing
  at the interface level observed a scrape actually running to completion,
  refreshed metadata, or a duplicate request staying a single scrape — the queue's
  own unit tests covered the pending set, but not what a caller sees. The explicit
  rescrape had no interface-level test at all, because its metadata fetch was
  hard-wired to the network.
- **"HTMX adapter tests cover only request/response translation."** The
  `POST /concerts/:id/scrape` route had no adapter test, for the same reason: any
  test would have reached for NPR.
- **"Documentation and the Change Record preserve the ADR-0006 distinction."**
  `docs/concerts.md` listed `scrape` and `ensure_scraped` but not the queued
  request, so the interface's scrape surface was documented incompletely.

Two pieces of the boundary were also still porous. `AppState` carried the
`ScrapeQueue` itself: no handler used it after #177, but the adapter could reach
past the interface to the queue, and the Test Control assertion for "the scrape
is pending" read the queue directly rather than the state a caller sees. And
`ScrapeQueue::enqueue` answered a `bool`, so "already queued" and "this process
has no worker" were the same answer — a distinction a caller reporting the
outcome has to make.

## Changes

The interface gained the outcome the ticket asks for, and the adapter lost its
last handle on the queue. No route, payload, status code, markup, stored fact,
queue semantic, or Job Run behavior changed.

- `Concerts::enqueue_scrape` is now `Concerts::request_scrape` and returns
  `ScrapeRequestResult { admission, state }`:
  - `ScrapeAdmission::{Enqueued, AlreadyQueued, NotNeeded}` — the same typed
    vocabulary — now carries the Concert State observed after the admission, so
    `Enqueued` and `AlreadyQueued` show `active_work.scrape_pending` and
    `NotNeeded` shows none.
  - A fourth admission, `NoWorker`, splits "already queued" from "this process
    runs no scrape worker". Before, a one-shot process (the `concert_db` CLI)
    answered `AlreadyQueued` while nothing was queued and nothing was pending —
    an admission that contradicted the state shipped beside it. The queue now
    says which it means (`ScrapeQueue::enqueue` returns
    `EnqueueOutcome::{Queued, AlreadyQueued, NoWorker}`), so the interface maps
    a fact instead of inferring one from a `bool`.
  - `ScrapeQueue::enqueue`'s dedupe insert and its channel send are now one
    critical section. With the answer split three ways, two concurrent requests
    on a queue whose worker had gone could previously produce `AlreadyQueued`
    for an entry whose send then failed and disappeared; the answer and the
    pending set it describes now cannot disagree. Nothing blocking happens under
    that lock — the send is unbounded, and the worker takes the lock only after
    its item has run.
  - The only rejection is the absent concert, so the result is a plain `Result`
    with the shared `CommandRejection::NotFound`, not a `JobRequestOutcome`: a
    scrape admits no Job Run, so there is no admission-after-persistence moment,
    no dependency edge, and no cancellation to report (ADR-0006).
  - The rename says "request a scrape" the way `request_download`,
    `request_split`, and `request_archive` do. Whether the work goes on the
    queue, not the Job Registry, stays in the vocabulary (`ScrapeAdmission`) and
    in the docs.
- `Concerts::ensure_scraped` returns the observed `ConcertState` instead of `()`.
  Its tolerance is unchanged: a failed automatic scrape is logged and the caller
  receives the listing-only state the concert already had, which keeps the row
  unscraped so the next view retries. The detail page now renders the state the
  interface returns instead of querying a second time, and `admit_download` /
  `admit_prepare` decide on that same observation rather than re-reading —
  `chain_active` is now a pure function of one observed state.
- The metadata fetch is an injectable seam, `MetadataRefreshFn`, with
  `production_metadata_refresh` as the production value and
  `Concerts::with_metadata_refresh` to replace it. This is the same injection
  point `ScrapeQueue::start_with` already gives the background item, extended to
  the explicit rescrape, and it is what makes "a rescrape refreshes metadata"
  and "an automatic scrape does not refetch existing metadata" testable without
  egress.
- `AppState` no longer carries `scrape_queue`; `Concerts` owns the only handle.
  Test Control's `scrape_enqueue` still answers the same `{ok, enqueued}` for the
  hurl protocol, and its own assertion now reads
  `state.active_work.scrape_pending` through the interface.

## Preserved behaviour

- The Sync route still hands every newly-listed concert that lacks metadata to
  the serial worker before responding, and still logs the number queued: it
  counts `ScrapeAdmission::Enqueued` from the interface instead of doing the
  decision itself.
- Automatic scrape-on-detail still runs before the page renders, still tolerates
  a failed fetch, and still renders the listing-only card in that case.
- `POST /concerts/:id/scrape` still refreshes inline: 200 with the rendered card
  on success, 404 for an unknown concert, 500 for a failed fetch. A rescrape of
  a concert whose background scrape is already queued is still allowed and still
  runs — the duplicate-work dedupe belongs to the queue, and an explicit rescrape
  has always done its own fetch.
- The Scrape Queue keeps its own protocol: an in-memory pending set, one
  long-lived consumer, no persistent started fact, no terminal-outcome
  arbitration, no cancellation, no recovery, and the shared Failed Job row under
  the `scrape` name — all of ADR-0006, untouched.

## Tests

Queue level (`jobs/scrape_queue.rs`):

- `enqueue_without_a_worker_reports_it_and_leaves_nothing_pending` — a detached
  queue answers `NoWorker`, and the id does not stay in the pending set.

Interface level (`concerts.rs`), on seeded in-memory persistence with the queue's
item injected and the metadata refresh injected:

- `a_scrape_request_admits_work_and_reports_the_pending_state` — `Enqueued`, the
  returned state shows `scrape_pending`, no metadata yet, and no Job Activity or
  lifecycle event exists (the scrape-invents-no-Job-Run pin). After releasing the
  worker's item, the observed state reaches the refreshed metadata, the pending
  flag clears, and the item ran once. The returned state at both points is
  compared with a fresh observation (`assert_is_current_state`).
- `a_repeated_scrape_request_reports_the_work_already_queued` — the duplicate is
  `AlreadyQueued` with the pending work still visible, and the worker ran the
  scrape exactly once.
- `a_failed_background_scrape_clears_the_pending_work_and_a_retry_is_admitted` —
  a queued scrape whose item applies nothing (the interface's view of a failed
  background scrape) clears the pending work, leaves the concert unscraped, and
  admits a retry rather than deduping it away; the retry then runs a second
  scrape. It waits for the pending flag rather than for a fixed delay, because
  this item finishes instantly.
- `a_scrape_request_for_a_concert_with_metadata_is_not_needed` — `NotNeeded`,
  nothing pending, and the worker never picked anything up.
- `a_scrape_request_for_an_absent_concert_is_rejected` and
  `a_one_shot_process_reports_that_there_is_no_worker` — the two edges: a typed
  `NotFound` rejection, and a process with no worker saying so rather than
  claiming work it never queued.
- `an_explicit_rescrape_reports_the_refreshed_state`,
  `a_failed_explicit_rescrape_is_a_fault_and_keeps_the_metadata` — the explicit
  path reports the refreshed state, queues nothing, and reports a failed fetch as
  a fault while leaving the recorded metadata intact.
- `an_automatic_scrape_reports_the_refreshed_state`,
  `an_automatic_scrape_tolerates_a_failing_refresh`,
  `an_automatic_scrape_does_not_refresh_existing_metadata` — the automatic path
  returns the state to render, keeps its tolerance, and never re-fetches a
  concert that already has metadata (counted, not assumed: a tolerated failure
  would otherwise hide a spurious fetch). These replace
  `ensure_scraped_keeps_metadata_that_is_already_there`,
  `ensure_scraped_tolerates_a_failing_fetch`, and
  `enqueueing_a_scrape_reports_whether_one_was_needed`, which asserted the same
  behaviour through the network or against no state.
- `get_combines_registry_queue_and_persisted_active_work` still pins that a
  pending scrape composes with active Job Runs in one observation.

Adapter level (`web/handlers.rs`):

- `rescraping_renders_the_refreshed_card` — the route asks the interface and
  renders what comes back: 200 and a swapped card whose embedded track list and
  track count are the refreshed ones.
- `rescraping_an_absent_concert_is_a_404` — the interface's `NotFound` maps to
  this endpoint's long-standing 404.

Existing black-box coverage re-run unchanged: `hurl/scrape_pending.hurl` (the
placeholder, the poll, the deterministic completion and its observation counts)
and `web_integration::detail_page_auto_scrape_failure_still_renders` (a real
connection-refused failure on the detail page) both still pass.

## Mutation checks

Each was applied, the named test observed failing, then the change reverted:

- Returning the Concert State observed *before* the queue admission fails
  `a_scrape_request_admits_work_and_reports_the_pending_state`.
- Dropping the "already has metadata" branch from `request_scrape` fails
  `a_scrape_request_for_a_concert_with_metadata_is_not_needed`.
- Propagating the refresh error from `ensure_scraped` fails
  `an_automatic_scrape_tolerates_a_failing_refresh`.
- Refreshing unconditionally in `ensure_scraped` fails
  `an_automatic_scrape_does_not_refresh_existing_metadata`.
- Mapping `EnqueueOutcome::NoWorker` to `AlreadyQueued` fails
  `a_one_shot_process_reports_that_there_is_no_worker`.
- Leaving a finished item in the queue's pending set fails
  `a_failed_background_scrape_clears_the_pending_work_and_a_retry_is_admitted`
  (it times out).

## Verification gates

Run in this checkout with `CARGO_TARGET_DIR=/tmp/tds-target` and
`CARGO_INCREMENTAL=0` (the checkout volume is 23 G used of 27 G, mostly 19 G of
`videos/`; with it fuller than that, `live-set-splitter`'s ffmpeg-backed tests
fail for want of space, not because of this change):

- `cargo fmt --all -- --check` — clean.
- `cargo clippy --workspace --all-targets -- -D warnings` — exit 0.
- `cargo nextest run --tests` — 939 tests, 939 passed.
- `cargo nextest run --tests -p concert-tracker --features test-control` — 785
  tests, 785 passed. The scrape tests were also run five times back to back to
  confirm the two that wait on the queue are not timing-dependent.
- `node scripts/hurl-test.js` — 14 files, 299 requests, 100% (the request count
  moves with the poll-and-retry files' timing).
- `npx playwright test` — 173 passed, in both the first and the last run on this
  tree, and on unmodified `main` for comparison. Two intermediate runs, one before
  and one after the review fixes, failed exactly one test:
  `player-queue.spec.js › track ending with nothing next stops cleanly without an
  error banner`, a player track-end timing test this change does not touch. It
  passes in isolation and in every other full run, and those two runs coincided
  with the checkout volume being full enough to fail an ffmpeg-backed test in
  `live-set-splitter`. It is a pre-existing timing flake, not a regression here.
- `./scripts/shellcheck.sh`, `./scripts/ts-check.sh`, `./scripts/ts-lint.sh` —
  exit 0 (no shell, TypeScript, or template changed; run to confirm the standard
  lint suite still passes).
