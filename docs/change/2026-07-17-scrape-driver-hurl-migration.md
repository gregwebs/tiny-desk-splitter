# Scrape Driver and pending-card Hurl migration

Implemented slice 4 of the remaining web-integration Hurl migration
([#110](https://github.com/gregwebs/tiny-desk-splitter/issues/110), parent
[#106](https://github.com/gregwebs/tiny-desk-splitter/issues/106)): a small
**Scrape Driver** Test Control surface for deterministic background-scrape
timing, used to migrate the last black-box test out of
`concert-tracker/tests/web_integration.rs` that had one. Slices 1 (#107,
typed job runner), 2 (#108, Job Driver), and 3 (#109, Scenario Seeds) landed
earlier on this branch.

## What changed

- New `concert-tracker/src/test_control/scrape_driver.rs`: `ScrapeDriver`,
  mirroring `job_driver::JobDriver`'s shape but adapted to the scrape queue's
  own seam (`jobs::scrape_queue::ScrapeItemFn`, a **synchronous** closure the
  queue worker runs inside `spawn_blocking` — not the `JobRunner` trait's
  async steps). Holds per-concert plans (`ScrapeOutcome::Succeed | Block`,
  default `Succeed`, no process-wide default plan), per-concert observation
  counters (`started`/`completed`/`blocked`/`released`), and a blocked map of
  `std::sync::mpsc::Sender<()>` (not `tokio::sync::oneshot`, since `run_item`
  is sync and can safely call `Receiver::recv()`). `scrape_item_fn(driver)`
  builds the `ScrapeItemFn` for `ScrapeQueue::start_with`. A released (or
  unblocked-default) scrape writes deterministic fixtures: `update_metadata`
  with artist `"Scrape Driver Artist {id}"`/album `"Scrape Driver Album
  {id}"`, plus a tiny **real, decodable JPEG** (via the `image` crate,
  already a dependency) written to the listing thumbnail path.
- `concert-tracker/src/test_control.rs`: added `test.scrape_set_plan`,
  `test.scrape_enqueue` (looks up the concert's `source_url` and calls the
  app's real `AppState.scrape_queue.enqueue`, returning `{ok, enqueued}`),
  `test.scrape_release`, and `test.assert_scrape_observation` (same
  "check-only-present-fields, reject-a-vacuous-call" shape as
  `assert_job_observation`). `TestControlServer` now holds an
  `Arc<ScrapeDriver>` alongside its `Arc<JobDriver>`; `reset()` clears both.
- `concert-tracker/src/test_control/adapter.rs`: new `AdapterRoute::Scrape`
  variant, `POST /test/scrape/{name}` → `test.scrape_{name}`, same
  flat-passthrough param translation as `/test/job/{name}` and
  `/test/assert/{name}` (not `/test/seed/{name}`'s request-object wrapping).
- `concert-tracker/src/bin/concert_web.rs`: the `ScrapeQueue` construction
  moved into the same `--test-control-port` match arm that already selects
  between the production and test-control `JobConfig` — a test-control build
  run without that flag still calls `ScrapeQueue::start` (the real
  `scrape_item`) exactly like a production build; only passing the flag
  swaps in `ScrapeQueue::start_with(scrape_item_fn(scrape_driver))`.
- New `hurl/scrape_pending.hurl`, migrating
  `pending_card_shows_loading_then_thumbnail`: seed a listing, block its
  scrape plan, enqueue it, assert the loading placeholder + polling markup on
  the public `GET /concerts/:id/status` (no retry needed — `enqueue` marks
  the queue's `pending` set synchronously before the worker ever picks the
  item up), poll `assert_scrape_observation {blocked: 1}`, release, assert
  (with retry) the thumbnail URL + no more polling on the same response, `GET`
  the thumbnail file and assert its JPEG magic bytes, then assert the final
  `started/completed/blocked/released = 1` observation.
- Removed `pending_card_shows_loading_then_thumbnail` and its now-dead
  helpers (`recv_soon`, `get_status_html`, the poll-budget constants) from
  `concert-tracker/tests/web_integration.rs`, leaving a breadcrumb comment.
  Extended `detail_page_auto_scrape_failure_still_renders`'s doc comment to
  explain why it stays Rust-only even though the Scrape Driver now exists:
  it exercises the detail view's *inline* auto-scrape (`ensure_scraped`), a
  real connection-refused failure on a synchronous call path that never goes
  through `ScrapeQueue` at all, so the Scrape Driver's injection seam does
  not reach it. `concert-tracker/tests/web_integration.rs` now has 3 tests
  left, all intentionally Rust-only (the other two are
  `prod_router_serves_embedded_js_without_livereload` and
  `served_openapi_spec_matches_built_api_doc`, unrelated router/build
  internals).
- `hurl/README.md` updated: adapter route table gained the `/test/job/{name}`
  row (previously undocumented there) and the new `/test/scrape/{name}` row;
  new "Scrape Driver" section (mirroring "Job Driver"'s structure); "Three
  ways to check something" gained a 5th "Scrape Driver control actions" item;
  "Why the remaining tests are Rust-only" updated to 3 tests with a new
  slice-4 history bullet; verification commands gained the new file's glob.

## Design decisions

- **No process-wide default plan for the Scrape Driver**, unlike the Job
  Driver. Nothing in this slice needs one (there is exactly one Hurl case),
  and omitting it removes one more piece of state a `.hurl` file could leak
  into another file sharing the same `just test-hurl` process — no "restore
  the default before the file ends" discipline is needed here at all.
- **No `fail` outcome yet.** The acceptance criteria only need block +
  deterministic success; a `ScrapeOutcome::Fail` (and the failed-job-row
  bookkeeping `record_scrape_failure` already does in production) is a
  reasonable future extension but was out of scope for what #110 asked for,
  so it was left out rather than speculatively added.
- **Real JPEG fixture, not `db::seeds::SENTINEL_BYTES`.** The listing card's
  `<img onerror="this.style.display='none'">` (`templates/concert_card.html`)
  silently hides a file that fails to decode as an image — writing the
  existing text sentinel to the thumbnail path would have let a `GET
  .../thumbnails/... → 200` Hurl assertion pass while the browser actually
  showed nothing. This surfaced from the adversarial plan review (below);
  the driver instead JPEG-encodes a real 1×1 pixel via the `image` crate
  (already a dependency, used the same way `scrape.rs`'s production
  thumbnail path does), and the Hurl case asserts the response's magic bytes
  (`bytes startsWith hex,ffd8ff;`) rather than only its status code. A
  driver unit test also round-trips the written file through
  `image::load_from_memory`.
- **Reset stays best-effort, matching the existing Job Driver/scrape-worker
  caveat, rather than adding queue quiescence/epoch machinery.** The
  adversarial plan review flagged that dropping a blocked scrape's sender
  wakes the parked item but does not wait for its `pending.remove` to land,
  and does not affect a request already queued but not yet picked up — so
  `test.reset` is not a true quiescence boundary for the scrape queue. The
  alternative (an acknowledged drain plus an epoch/generation guard so
  pre-reset requests can't write after reset) was presented to the project
  owner as a real option; the decision was to keep the existing
  `reset_test_data` semantics (already documented as non-quiescing for jobs
  and the scrape worker) rather than add new machinery to a test-only
  surface, since no `.hurl` file calls `/test/reset` mid-run in the first
  place (`--jobs 1`, one shared process per `just test-hurl` invocation).
  `reset_test_data`'s doc comment in `test_control.rs` was updated to state
  this precisely instead of only gesturing at "deferred to a later slice."
- **Scrape Driver kept as its own control surface, not a `JobStepKind`.**
  The scrape queue has its own `pending` set and injectable per-item
  function; it is not a `JobRunner` and is not one of download/split/open.
  Reusing the Job Driver's types would have conflated two independently
  evolving seams for no real code-sharing benefit (the block/release/reset
  shape is mirrored deliberately, but the concrete state — sync channel vs.
  oneshot, no step-kind dimension, no default plan — differs enough that a
  shared abstraction would have been more indirection than the two ~150-line
  modules it would replace).
- **No new ADR.** The scrape queue's injection seam (`ScrapeItemFn`,
  `ScrapeQueue::start_with`) already existed before this slice; this only
  plugs a Test Control-driven implementation into it, the same kind of move
  ADR 0005 already covers for the Job Driver.

## Adversarial plan review

An adversarial Codex review (engineering-lead persona) of the implementation
plan found two material issues before coding began:

1. **The deterministic thumbnail fixture was not a valid image.** As
   originally planned, the release path wrote `db::seeds::SENTINEL_BYTES`
   (plain text) to the thumbnail path. Since the card's `<img>` has
   `onerror="this.style.display='none'"`, a `GET .../thumbnails/... → 200`
   Hurl assertion would pass while the browser showed nothing — the test
   could certify behavior that was visibly broken. Fixed as described in
   "Design decisions" above: a real JPEG via the `image` crate, asserted by
   magic bytes in Hurl and decoded in a driver unit test.
2. **`test.reset` was described as a "deterministic cancel" without
   qualifying that it is not a queue quiescence boundary** — a request
   already queued (enqueued, not yet started) or one whose release just
   landed could still write after `reset` returns. The reviewer recommended
   an acknowledged drain plus an epoch guard. This was raised to the project
   owner as an explicit choice (full quiescent reset vs. keep the existing
   documented best-effort semantics); the owner chose to keep the existing
   semantics, since it matches the Job Driver/scrape-worker precedent
   already documented on `reset_test_data` and no Hurl file resets mid-run.
   The plan and `reset_test_data`'s doc comment were both updated to state
   the caveat precisely rather than only reference "deferred to a later
   slice."

A follow-up (non-adversarial) Codex review of the amended plan confirmed both
fixes: `bytes startsWith hex,ffd8ff;` is valid Hurl 8 syntax, the `image`
crate's JPEG encoder is available in a debug `--features test-control` build,
and the reset amendment is internally consistent once "deterministic
cancellation" is understood to mean only "wakes an already-blocked item
without writing fixtures," not queue-wide quiescence. No new issues from the
amendments themselves.

## Adversarial code review

An adversarial Codex review of the finished implementation (uncommitted
working-tree diff) found two material issues, both fixed:

1. **`reset()` could permanently strand a fresh block.** `plans` and
   `blocked` were two independent `Mutex`es. `run_item`'s block branch read
   the plan, then (a few instructions later) inserted its release sender
   into `blocked`; `reset()` cleared both maps independently. If `reset()`
   landed in the gap between those two steps, it would clear a `blocked` map
   that did not yet contain the entry `run_item` was about to insert — the
   scrape would then park forever with no sender anyone could ever release,
   and its `pending` id would never clear (the card would poll indefinitely).
   Fixed by combining `plans` and `blocked` into one `DriverState` behind a
   single `Mutex` (`ScrapeDriver::state`), and reading the plan +
   conditionally registering the sender as one atomic critical section under
   that lock. `reset()` now takes the same lock to clear both fields
   together, so every interleaving is safe: either `reset` runs fully before
   the block's atomic section (a fresh block against already-cleared state)
   or the atomic section completes first (`reset` then drops that very
   sender, resolving it immediately) — there is no window where an insert
   can be missed. A new stress test,
   `reset_racing_a_fresh_block_never_permanently_strands_it`, races 50
   unsynchronized block+reset pairs and asserts every one finishes within a
   timeout.
2. **A thumbnail-write failure could permanently mark a concert "scraped"
   with no thumbnail.** `write_scraped_fixture` originally committed
   `update_metadata` (setting `metadata_scraped_at`) *before* writing the
   thumbnail JPEG. A filesystem failure on the thumbnail write (e.g. an
   unwritable workdir) would leave the metadata write already committed,
   permanently marking the concert scraped with no image ever created and no
   way for a later scrape attempt to retry (this repo's own retry contract —
   see `detail_page_auto_scrape_failure_still_renders` — depends on
   `metadata_scraped_at` staying `None` on failure). Fixed by reordering:
   the thumbnail is now written first, and `update_metadata` only runs after
   it succeeds. A new test,
   `thumbnail_write_failure_leaves_metadata_unset_so_a_later_scrape_can_retry`
   (Unix-only, follows the same `PermissionsExt` technique as
   `test_control.rs`'s existing `reset_leaves_db_rows_intact_when_filesystem_cleanup_fails`),
   makes the thumbnails directory unwritable and asserts `metadata_scraped_at`
   stays `None` and `completed` stays `0`.

The two explicitly-decided-and-out-of-scope points from the plan review
(no `fail` outcome yet; `test.reset` non-quiescence for already-queued or
just-released requests) were re-confirmed as not new findings — the reviewer
was instructed not to re-litigate them absent a concrete new failure mode,
and found none.

A follow-up (non-adversarial) review after applying both fixes returned
"Ship — both original findings are resolved; no new material issue found,"
confirming: the single-mutex `DriverState` makes plan lookup and
blocked-sender registration atomic relative to `reset()`, so no interleaving
can permanently strand a scrape; and the thumbnail-first write order
guarantees `metadata_scraped_at` stays unset when the thumbnail write fails.
It noted one cosmetic, non-blocking observation — a `reset` racing the
narrow window between registering a blocked sender and bumping the
`blocked` observation counter can leave a stale `blocked=1` in
`ScrapeDriver::observation` even though nothing is actually blocked anymore
(no hang, no incorrect fixture write) — documented in `run_item`'s comments
as the same not-a-quiescence-boundary territory already covered by `reset`'s
docs, not worth further locking given the Hurl suite never resets mid-run.
The full verification suite (below) was re-run after both fixes and passes.

## Verification

```sh
cargo check -p concert-tracker --features test-control
cargo check -p concert-tracker
cargo build --bin concert-web --features test-control
cargo nextest run -p concert-tracker --features test-control test_control
node scripts/hurl-test.js --glob 'hurl/scrape_pending.hurl'
just test-hurl
cargo nextest run -p concert-tracker --test web_integration
just lint
cargo build --release --bin concert-web --features test-control  # expected to fail (release guard)
```

All of the above pass; the release-guard build fails with the expected
`compile_error!` from `concert-tracker/src/test_control.rs`, confirming
`test-control` still cannot leak into a release build.
