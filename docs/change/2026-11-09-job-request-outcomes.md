# Job Request outcomes

Issue [#163](https://github.com/gregwebs/tiny-desk-splitter/issues/163) asks for
download and split Job Requests to move behind `Concerts` "while returning typed
accepted/rejected outcomes with immediate Concert State". The move itself landed
with the rest of the Concert application interface in
[the #177 contraction](2026-07-25-concert-catalog-and-status.md), recorded in
[Job Requests and scrape behind Concerts](2026-07-25-job-request-interface.md):
`request_download`, `request_split`, `prepare`, and `cancel_job` own admission,
`PrepareStatus` is a projection of Concert State, and the HTMX routes are thin
adapters over them.

Auditing the ticket's acceptance criteria against `main` after that contraction
left one criterion unmet and another only loosely asserted, plus one acceptance
criterion with no interface-level coverage at all.

- **"Rejected requests contain a precise typed reason and latest state when
  available."** The reason was typed; the state was not carried. The three
  methods returned `Result<_, ConcertCommandError>`, so a rejection arrived as an
  error value with no way to attach a Concert State. `prepare` on a concert with
  no set list is exactly the case the criterion is about — the concert exists and
  is merely missing a set list — and the caller received
  `Invalid { message }` and nothing else. (The #161 audit recorded the opposite
  decision for `set_status`/`set_notes`, and deliberately: `NotFound` is their
  only rejection, and no Concert State exists to attach. This is the ticket that
  has a rejection with a state.)
- **"Accepted outcomes contain Concert State after persistent Job Run
  admission."** True of the code, but the tests read one or two fields of the
  returned state instead of asserting it is the state a caller would observe
  next — the same shape of hole the #162 audit found in the playback commands.
- **"Interface-level tests cover acceptance, rejection, dependency chaining,
  failure, cancellation, and retry behavior."** The queued split edge was
  asserted as a boolean; nothing at the interface level showed the split actually
  running after the download succeeded, a download failing and dropping the split
  queued behind it, a retry after failure admitting a fresh run, or a split being
  cancelled. Those existed only as engine tests in `jobs::{download, split}`.
  Download cancellation was covered at the interface, but not the refreshed state
  or the single Failed Job it must write, and split cancellation was not covered
  at all.

One documented admission also had no test: `JobAdmission::NothingToDo` from
`request_download`, which fires when preparation finds every track already on
disk and the source present. That branch is the one the pre-#177 `download`
handler served by doing nothing at all, so it needed pinning rather than
changing.

## Changes

The interface gained the outcome type the ticket asks for; nothing about a Job
Run, a route, a payload, a status code, or a permission changed.

- `Concerts::request_download`, `request_split`, and `prepare` return
  `JobRequestOutcome`:
  - `Accepted(JobRequestResult)` — the admission decision (`Admitted`,
    `AlreadyRunning`, `NothingToDo`) and the Concert State observed after
    persistent admission.
  - `Rejected(JobRejection)` — the precise `CommandRejection` reason, shared with
    every other command so adapters keep one mapping, and the latest
    `ConcertState` when the concert exists. Only an absent concert has no state
    to attach.
  - `Err(ConcertCommandError::Operational)` — a genuine fault, as before. Expected
    refusals are values and faults are errors, matching the rest of the interface.
- The admission logic moved into private `admit_download`, `admit_split`, and
  `admit_prepare` steps; the public methods are one shared conversion from those
  results to the outcome. `CommandRejection::concert_id` is new, so the
  conversion attaches the state without matching every rejection variant.
- `prepare` now reports an admission. `Ready` (every track already exists) is
  `NothingToDo`; otherwise the observation made *before* preparation decides
  between `AlreadyRunning` and `Admitted`, the same way `request_download` already
  did. `chain_active` is that observation, shared by both.
- `web/handlers.rs` renders the outcome (`job_request_response`): an accepted
  request returns the card, a rejected one returns the reason's own client-error
  status. `prepare_concert` keeps its historical 422-with-the-message for the
  no-set-list rejection and sends every other rejection through the shared
  mapping.
- `concert-db resplit` matches the outcome: a rejection increments the errored
  count and prints `ERROR` like a start error did, with its reason on stderr.

## Preserved behaviour

No route, payload, status code, markup, CLI verb, stored fact, or Job Run
semantic changed, and every line `concert-db resplit` prints on stdout is
unchanged — the only difference is the stderr diagnostic for a refused request
(`... — rejected: <reason>` where a start error said `... — start error: <cause>`),
which no test or script matches on.

- `POST /concerts/:id/download`, `/split`, and `/prepare` answer as they did: 200
  with a card fragment, 200 with `PrepareStatus`, 422 with the message for a
  concert with no set list, 404 for an unknown concert, 500 for a fault.
- `POST /jobs/:id/cancel/:kind` still redirects, and still tolerates an unknown
  concert. `JobCancellation` already carried the refreshed state.
- Nothing in `jobs::run` moved: race-safe admission, dependency edges, exactly one
  terminal outcome, cancellation, recovery, and Failed Job history stay with the
  Job Run engine, and the production and deterministic (`JobConfig::test`, the
  Job Driver) adapters are untouched.
- The one behaviour that looks like a change is not one: `request_download` on an
  already download-and-split concert still re-downloads, because the Download
  control is hidden but the endpoint has always re-downloaded (see
  `hurl/job_chain.hurl`'s "does not re-split" case). Admission does not consult
  `PermittedActions`; it reads the same observation, which is why a control the
  state offers is a command the interface accepts, not the reverse.

## Tests

Interface level (`concerts.rs`), all on seeded in-memory persistence with
temporary media and real `JobRegistry` state:

- `a_download_request_with_a_set_list_queues_the_split_behind_it`,
  `a_download_request_without_a_set_list_starts_only_a_download`,
  `a_split_request_without_a_source_is_accepted_and_reports_nothing_to_do`,
  `preparing_queues_the_download_and_split_chain`,
  `a_second_download_request_keeps_the_queued_split` — now also compare the
  returned Concert State with a fresh observation (`assert_is_current_state`), so
  "contains Concert State after admission" is pinned rather than sampled.
- `a_second_download_request_reports_an_already_active_chain` — now also pins
  that the repeat records exactly one `download_started` event, so "accepted and
  changes nothing" cannot hide a second Job Run.
- `a_download_request_with_nothing_left_to_run_reports_nothing_to_do` — the
  branch nothing covered: source and every track already on disk, split not
  recorded. It asserts the state does not offer download *and* the request starts
  nothing, which is the only direction of the permitted-action/admission
  agreement that holds.
- `the_split_queued_behind_a_download_runs_when_the_download_succeeds` — a
  deterministic download writes the source, the split that was queued behind it
  writes the tracks; the test waits for the observed state to reach
  `SplitStatus::Split`, then asserts the track rows, that no activity or queued
  edge lingers, and that the download it depended on succeeded first.
- `a_failed_download_is_terminal_drops_the_queued_split_and_a_retry_runs_the_chain`
  — the download fails (a real `sh -c 'exit 7'` command), the observed state
  reaches `DownloadError`, the queued split never runs, exactly one Failed Job is
  written, and the control is offered again. A retry is admitted and the chain
  completes, writing no second Failed Job.
- `cancelling_a_running_download_is_reported_and_repeating_it_is_not_an_error` —
  strengthened: the control is withdrawn while the run is active and returns
  afterwards, the returned state is current, cancellation writes exactly one
  Failed Job whose message is `cancelled by user`, and asking again neither
  cancels nor records anything.
- `cancelling_a_running_split_returns_the_refreshed_state` — the same for a split:
  `SplitStatus::SplitError`, no `split_at`, `tracks_busy` cleared, one Failed Job,
  and `NothingActive` on the repeat.
- `preparing_without_a_set_list_is_rejected_with_the_latest_state` — the
  state-carrying rejection: `Invalid` with the no-set-list message *and* the state
  it was refused in, compared against a fresh observation.
- `job_requests_are_rejected_for_an_absent_concert_without_a_state` — all three
  methods reject an unknown concert as `NotFound` with `state: None`.
  `a_one_shot_process_cannot_admit_job_requests` still pins that a process with
  no Job Runs reports that as a fault rather than a rejection.
- `a_split_request_without_a_source_reports_nothing_to_do` and
  `a_split_request_without_a_source_is_accepted_and_does_nothing` were merged into
  the one test above; they asserted the same request.

Adapter level (`web/handlers.rs`):

- `a_rejected_job_request_answers_with_the_reasons_client_error`,
  `job_requests_for_an_absent_concert_stay_404s` — the outcome mapping the
  download and split routes now depend on: a rejection is a client error, not a
  server error.
- `preparing_without_a_set_list_answers_422_not_a_server_error` — the special
  case that keeps `POST /prepare`'s long-standing 422 and its message, now that
  the rejection is a value rather than an error.

Black-box HTTP (`hurl/`, unchanged and re-run): `job_chain.hurl` already covers
the chain through the real routes — plain download, `prepare` idempotency, the
download → split chain, retry after a split error, the double-click that must keep
the queued edge, cancellation with its Failed Job and `download-error`, and the
"tracks present but source missing" force-download — against the deterministic Job
Driver. Nothing needed adding there: the routes and their behaviour did not move,
which is what that file pins.

## Mutation checks

Each was applied, the named test observed failing, then the change reverted:

- Returning `state: None` from every `JobRejection` fails
  `preparing_without_a_set_list_is_rejected_with_the_latest_state`.
- Returning the Concert State observed *before* admission from
  `admit_download` fails
  `a_download_request_with_a_set_list_queues_the_split_behind_it`.
- Dropping the "source already present after preparation" branch from
  `admit_download` fails
  `a_download_request_with_nothing_left_to_run_reports_nothing_to_do`.
- Removing the split's dependency registration in `jobs::prepare` fails
  `the_split_queued_behind_a_download_runs_when_the_download_succeeds` and
  `a_failed_download_is_terminal_drops_the_queued_split_and_a_retry_runs_the_chain`.
- Reporting `JobCancellation::Cancelled` for a request with nothing active fails
  `cancelling_a_running_download_is_reported_and_repeating_it_is_not_an_error`
  and `cancelling_a_running_split_returns_the_refreshed_state`.

## Verification gates

Run in this checkout with artifacts in a scratch target directory on `/tmp` (the
checkout volume holds 19 G of `videos/` out of 29 G, so an in-repo debug build
cannot fit), `CARGO_INCREMENTAL=0`:

- `cargo fmt --all -- --check` — clean.
- `cargo clippy --workspace --all-targets -- -D warnings` — exit 0.
- `cargo nextest run --tests -p concert-tracker --features test-control` — 774
  tests: 772 passed and the two pre-existing `jobs::download` tests described
  below failed. The same tree ran 774/774 green earlier in the session and those
  two tests pass in isolation, so neither failure is attributable to this change.
  The workspace-wide `cargo nextest run --tests` runs 928 tests and passed 926 in
  its last run, failing the same two.
- `node scripts/hurl-test.js` — 14 files, 298 requests, 100% (the corpus total
  moves with the poll-and-retry files, as earlier Change Records note).
- `npx playwright test` — 173 passed.
- `./scripts/shellcheck.sh`, `./scripts/ts-check.sh`, `./scripts/ts-lint.sh` —
  exit 0 (no shell, TypeScript, or template changed; run to confirm the standard
  lint suite still passes).

Two neighbours in `jobs::download::tests` are red in this sandbox under load:
`failed_download_records_error` and `successful_download_marks_downloaded_at`
sleep a fixed 200 ms and then assert that an asynchronous Job Run has committed.
They pass in isolation, fail in the same way on unmodified `main` (4/4 runs of
`test(jobs::download::tests::)` with this working tree stashed) — they are the
same fixed-timing assumption
[the #170 fix](2026-07-24-fix-split-job-test-timeout.md) removed from the
split helper, still present in these two download tests. They are untouched by
this change and should get the generation-bound wait, not a longer sleep, in
their own slice.
