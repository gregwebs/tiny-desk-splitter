# Archive and unarchive workflows behind Concerts

Issue [#167](https://github.com/gregwebs/tiny-desk-splitter/issues/167) asks for
archive Job Requests, archive cancellation, and the complete unarchive workflow to
move behind explicit `Concerts` methods, "returning typed outcomes with current
Concert State". Most of the move landed with the rest of the Concert application
interface in [the #177 contraction](2026-07-25-concert-catalog-and-status.md),
recorded in [Archive and unarchive behind Concerts](2026-07-25-archive-interface.md):
`Concerts::request_archive` owns admission, `Concerts::unarchive` owns the whole
unarchive workflow, and the two HTMX routes are thin adapters over them.

Auditing the ticket's acceptance criteria against `main` after
[the #163 outcome work](2026-11-09-job-request-outcomes.md) left two criteria
unmet and one asserted only at the engine level.

- **"Archive admission and cancellation use explicit `Concerts` methods and
  shared Job Request handling."** The methods were explicit; the handling was
  not shared. `request_archive` returned its own `ArchiveOutcome` and did its own
  `job_config` check, so archive re-derived what `request_download`,
  `request_split`, and `prepare` get from one conversion. Cancellation was
  already shared — `Concerts::cancel_job(id, JobKind::Archive)` routes through
  the same `lifecycle::cancel_job` as the other two kinds — but nothing at the
  interface level said so.
- **"Accepted and rejected archive outcomes carry immediate/latest Concert
  State."** Accepted carried it. Rejected did not: "no archive location
  configured" arrived as `Err(Rejected)`, the shape `#163` had already replaced
  for the other Job Requests, so a caller had the reason and no state to render
  beside it.
- **"Interface tests cover admission, rejection, success, failure, cancellation,
  unarchive retry, and state refresh."** Only rejection, "nothing to archive",
  unarchive success, unarchive rejection, and unarchive retry existed. Archive
  admission, a successful archive Job Run, an archive failure, and archive
  cancellation were covered at the engine level (`jobs::archive`,
  `db::lifecycle`) and nowhere else, and no archive test compared the returned
  Concert State with a fresh observation.

## Changes

The interface gained the outcome type the ticket asks for, archive joined the
existing Job Request conversion, and one admission that was mislabelled is now
labelled honestly. Nothing user-visible changed: no route, payload, status code,
stored fact, event, or permission.

- `Concerts::request_archive` now returns `JobRequestOutcome`, the same type
  `request_download`, `request_split`, and `prepare` return:
  - `Accepted(JobRequestResult)` — `Admitted`, `AlreadyRunning`, or
    `NothingToDo` (no download and no split to move, or the media is already
    archived), with the Concert State observed after persistent admission.
  - `Rejected(JobRejection)` — the precise `CommandRejection` reason (no archive
    location configured is `Conflict`; an unknown concert is `NotFound`) and the
    latest `ConcertState` when the concert exists.
  - `Err(ConcertCommandError::Operational)` — a genuine fault: a process that
    runs no Job Runs, a settings read failure, or an engine failure.
- `ArchiveOutcome` is gone. `ArchiveOutcome::NothingToArchive` is
  `JobAdmission::NothingToDo`, which is the same vocabulary a split request for a
  concert that is not downloaded already uses, so an adapter or batch tool
  reasons about "nothing to run" once. `archive_admission` converts
  `jobs::archive::StartOutcome` the way `download_admission` and
  `split_admission` convert theirs.
- An archive request on an already archived concert is `NothingToDo`, decided in
  `admit_archive` before the engine is asked to start anything. Previously that
  request reached `try_mark_archive_started`, which refuses to start an archive of
  an archived concert, and the refusal came back as
  `StartOutcome::AlreadyRunning` — an admission that claims a Job Run is active
  while the state it carries shows none. Keeping `AlreadyRunning` for "an archive
  Job Run of this concert is active" is what makes that outcome worth reading.
- The admission logic moved into a private `admit_archive`, so the public method
  is the one shared conversion from an admission result to an outcome.
- `web/handlers.rs` renders the outcome. An accepted archive request answers with
  the page refresh both archive routes have always sent; a rejected one answers
  500 with the reason, which is this endpoint's long-standing protocol (see
  "Deliberate non-changes"). The two handlers share one `page_refresh` response
  and one `archive_endpoint_error` mapping instead of two copies of each.

## Deliberate non-changes

- **No error-status change.** Archive and unarchive still answer **500** for a
  request the concert cannot satisfy (no archive location configured, not
  archived, an archive in flight, no album). `#168`'s manual pass found this
  inconsistent with the typed 409 the rest of the contracted surface returns, and
  [the hermetic hurl file](2026-11-09-archive-lifecycle-hurl-hermetic.md) pins it
  as-is. Changing it is a user-visible change this ticket does not ask for, so the
  adapter maps the interface's rejection onto the status it always used. The
  rejection body still names the concert, because `CommandRejection`'s own
  `Display` has always done that; only the source of the string changed.
- **Unarchive is not a `JobRequestOutcome`.** It is not a Job Request: it
  registers no Job Run, so it has no admission race, no cancellation, and no
  dependency edge. Its refusals stay `Rejected(ConcertCommandError)` like every
  other non-Job-Request command (`delete_track`, `set_split_timestamps`), which
  keeps "a Job Request has an outcome value" a property of the Job Request
  methods rather than of every command.
- **Unarchive failure is still an operational error, not an outcome.** A failed
  unarchive attempts to record `archive_errors` and a Failed Job entry, leaves the
  record unchanged, and reports the fault, so a retry fails the same way instead
  of reporting a restored concert. Both writes stay best effort (`let _ =`), as
  they have been: a broken record write must not mask the fault the caller has to
  see. Neither write failing is reachable without a persistence fault that would
  also break the observation the command ends with, so it is documented rather
  than tested.
- Nothing in `jobs::run` or `jobs::archive` moved: race-safe admission, exactly
  one terminal outcome, cancellation, restart recovery, and Failed Job history
  stay with the Job Run engine.

## State transitions

Documented in [docs/concerts.md](../concerts.md#archive-state-transitions):
`Not Archived → Archiving → Archived`, `Archiving → Archive Error`, a retry from
`Archive Error`, `Archived → Not Archived` by unarchive, and the two no-ops that
start nothing and record nothing (`request_archive` with no media to move, and
`request_archive` on an already archived concert).

`Archived` beats a recorded error, so a failed archive followed by a successful
retry reads as `Archived` with the failure still on record. A *cancelled* rewrite
of an already archived concert is not a case: lifecycle admission never starts an
archive for an archived concert, so that request is the `NothingToDo` no-op above.
`unarchive` clears the recorded archive errors along with the archive facts;
Failed Job history is audit data and is never cleared.

## Tests

Interface level (`concerts.rs`), all on seeded in-memory persistence with
temporary media and a real `JobRegistry`:

- `an_archive_request_without_a_configured_location_is_a_conflict` — now asserts
  the typed `Conflict` rejection, that it carries the concert's current state, and
  that a refused request writes no `archive_started`/`archive_error` event.
- `an_archive_request_with_neither_a_download_nor_a_split_has_nothing_to_do` —
  `NothingToDo`, current state, and no Failed Job. (Renamed from
  `..._has_nothing_to_archive`; the assertion changed from the deleted
  `ArchiveOutcome` variant to the shared admission.)
- `an_archive_request_on_an_archived_concert_has_nothing_to_do` — the admission
  bug the review of this change found: the request reports `NothingToDo` (not
  `AlreadyRunning`, which means a Job Run is active), the state reports
  `Archived` with no active archive work, the event history is unchanged, and no
  Failed Job appears.
- `an_admitted_archive_request_moves_the_media_and_reports_the_archived_state` —
  the admission is `Admitted` and the returned state already shows archive work;
  the run then reaches `Archived`, the concert directory becomes a symlink into
  the archive location, the media is there, `can_unarchive` is offered and
  `can_archive` is not, the state is the state a fresh observation reports, the
  last event is `archived`, and no Failed Job exists.
- `a_failed_archive_is_terminal_records_a_failed_job_and_a_retry_runs_again` — a
  missing source directory fails inside `execute`, after admission: the state
  reaches `ArchiveError`, `archived_at` stays empty, an error is recorded, the
  Archive control comes back, exactly one `archive` Failed Job exists; a retry is
  admitted and writes its own second Failed Job, which is what makes "the next
  request admits a fresh run" observable at the interface.
- `cancelling_a_running_archive_returns_the_refreshed_state` — the state offers
  neither Archive nor Unarchive while an archive is in flight; cancellation
  reports `Cancelled`, the lifecycle is `ArchiveError`, the control comes back,
  the returned state is the current one, exactly one Failed Job says
  `cancelled by user`, and asking again is `NothingActive` with no second
  terminal outcome. An archive run's own work is a single directory rename, far
  too fast to catch in flight, so the test holds the two facts an active run holds
  — the persisted `archive_started_at` admission writes, and a live registry slot
  whose task never finishes — and cancels through `Concerts::cancel_job`. That
  pins that archive's facts reach the engine's cancellation path and that its
  outcome reaches the caller; the path itself is the one the download and split
  tests exercise with real running commands.
- `unarchiving_moves_the_media_back_and_clears_the_lifecycle` — strengthened: the
  file's bytes come back, the symlink is gone, the state offers Archive and no
  longer Unarchive, it is the current state, and the last event is
  `archive_delete`.
- `unarchiving_an_archived_concert_with_an_archive_in_flight_is_a_conflict` and
  `unarchiving_an_archived_concert_with_no_album_is_a_conflict` — the two
  eligibility checks that had no coverage. The first is defense in depth:
  `try_mark_archive_started` refuses to start an archive of an archived concert,
  so the row shape is only reachable if a future change lets an archive run
  overlap one; it still pins that the symlink is left alone.
- `unarchiving_without_an_archive_symlink_records_the_failure_and_repeats_cleanly`
  — unchanged: the failure is recorded and a retry fails the same way.
- `job_requests_are_rejected_for_an_absent_concert_without_a_state` and
  `a_one_shot_process_cannot_admit_job_requests` now include
  `request_archive`, so archive's own `NotFound`/no-workers answers are pinned
  beside the other three Job Requests.

Adapter level (`web/handlers.rs`):

- `archiving_what_cannot_be_archived_is_this_endpoints_own_500` and
  `unarchiving_what_is_not_archived_is_this_endpoints_own_500` — the mapping the
  two routes depend on: the interface refuses, this protocol answers 500, and the
  reason survives as the body.

Black-box HTTP (`hurl/archive_lifecycle.hurl`, unchanged and re-run) already
covers both routes end to end: the unconfigured-location 500, the Archived filter
appearing once a location is set, archive moving the concert into that filter,
archiving an archived concert as a 200 no-op, unarchive bringing it back, the
not-archived 500, nothing-to-archive, and a missing concert's 500. Archive runs
do their own work in-process rather than through the deterministic Job Driver, so
that file waits on the run's terminal event instead of on a driver step.

## Mutation checks

Each was applied, the named test observed failing, then the change reverted:

- Returning the Concert State observed *before* `start_archive` from
  `admit_archive` fails
  `an_admitted_archive_request_moves_the_media_and_reports_the_archived_state`.
- Reporting `JobAdmission::Admitted` for `StartOutcome::NothingToArchive` fails
  `an_archive_request_with_neither_a_download_nor_a_split_has_nothing_to_do`.
- Dropping the already-archived branch from `admit_archive` fails
  `an_archive_request_on_an_archived_concert_has_nothing_to_do` (it reports
  `AlreadyRunning`, which is the bug the review found).
- Attaching no Concert State to a Job Request rejection fails
  `an_archive_request_without_a_configured_location_is_a_conflict`.
- Reporting `JobCancellation::NothingActive` for an active archive run fails
  `cancelling_a_running_archive_returns_the_refreshed_state`.
- Dropping the Failed Job write from a failed unarchive fails
  `unarchiving_without_an_archive_symlink_records_the_failure_and_repeats_cleanly`.

## Verification gates

Run in this checkout. The three mounted volumes here were effectively full (the
checkout volume holds 19 G of `videos/` of 27 G, and `/` held a 14 G stale
scratch target directory), so those scratch build caches were removed and
artifacts went to a fresh scratch target directory on `/tmp` with
`CARGO_INCREMENTAL=0`:

- `cargo fmt --all -- --check` — clean.
- `cargo clippy --workspace --all-targets -- -D warnings` — exit 0.
- `cargo nextest run --tests -p concert-tracker --features test-control` — 816
  tests, all passed.
- `node scripts/hurl-test.js` — 14 files, 100%, in every run (including
  `archive_lifecycle.hurl`'s 21 requests against the current routes). The total
  request count moves between runs — 298 to 300 here — because the poll-and-retry
  files issue a variable number of requests; `archive_lifecycle.hurl` does not use
  those, and the
  [per-run scratch archive location](2026-11-09-archive-lifecycle-hurl-hermetic.md)
  keeps repeated runs independent of each other.
