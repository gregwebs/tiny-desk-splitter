# Split Job Run orchestration and download dependency intent

Implements [#126](https://github.com/gregwebs/tiny-desk-splitter/issues/126),
the second implementation slice of
[#124 — Deepen concert job orchestration](https://github.com/gregwebs/tiny-desk-splitter/issues/124).
The PR is based on the `job-module` parent branch established by #125.

## Purpose

Move split Job Requests onto the shared `jobs::run` engine introduced for
downloads. A split request must be rejected synchronously before lifecycle
history exists, or be accepted once and reach exactly one terminal outcome.
Download-to-split chaining must retain only automatic-split intent: after the
download has committed success, the dependent constructs a new split request
from the concert's current database and filesystem state.

This is the **migrate split** step of #124's expand-migrate-contract sequence.
Archive remains on the legacy path until #127 and the legacy execution protocol
remains until #128.

## Implementation Plan

### Public test seams

Tests exercise behavior at these existing public boundaries:

1. `jobs::split::start_split` and `jobs::run::cancel` are the Rust application
   seam for split request admission and terminal arbitration. Tests use a real
   in-memory SQLite database, real scratch files, and the existing typed
   `JobRunner`; internal helper calls are not mocked.
2. Product routes plus the Test Control Job Driver are the black-box HTTP seam
   for automatic, user-timestamp, reset-to-automatic, download dependency,
   failure, and cancellation behavior. Existing `hurl/job_chain.hurl` and
   `hurl/split_timestamps_flow.hurl` scenarios will be extended only where the
   acceptance criteria are not already observable.

### State changes

Direct split request:

```text
Split intent + current concert state
              |
       reserve Split key
        /            \
 occupied          reserved
    |                 |
AlreadyRunning   validate current DB state
 (no history)      /             \
               reject          valid input
                 |                 |
          no lifecycle       mark_split_started
          no Failed Job        /          \
                            false        accepted
                              |              |
                       AlreadyRunning   post-acceptance setup
                                         /       |       \
                                      failure  execute   auto-recover
                                         |        |          |
                                         +-- terminal gate --+
                                                   |
                        +--------------------------+---------------------+
                        |                                                |
                 failed/cancelled                                   succeeded
                        |                                                |
          one transaction: split failure,             gather required completion facts
          event, Failed Job row                        before claiming terminal gate
                        |                                                |
            drop dependent requests                    one transaction: split success,
                                                       event, tracks-present and required
                                                       timestamp facts
                                                                    |
                                                   best-effort reconciliation warnings
                                                                    |
                                                       release dependents and registry
```

Automatic download-to-split dependency:

```text
POST download / prepare
        |
store edge: Download key -> AutomaticSplitIntent(concert_id)
        |
download terminal outcome
   /                         \
failed or cancelled        success persisted
   |                         |
drop intent;              take intent
no split Job Run            |
                      re-read current concert
                      and filesystem state
                         /          \
                    reject/no-op   submit fresh SplitRequest(Analyze)
                    synchronously             |
                    no Job Run        normal split state machine
```

### Code changes

#### 1. Preserve the keyed, intent-only dependency edge

The existing dependent `JobKey { concert_id, kind: Split }` already is the
required intent-only representation: it identifies an automatic split without
capturing a `Concert`, `SplitJob`, timestamps, paths, or validated input. Keep
that representation and its identity semantics rather than introducing a
parallel dependent-request enum.

The release operation takes queued keys exactly once and dispatches them after
the prerequisite's success transaction:

```rust
registry.add_dependent(
    download_key,
    JobKey { concert_id, kind: JobKind::Split },
);
```

`spawn_dependents` turns that key into a fresh
`split::start_split(..., SplitMode::Analyze)` call. That call re-reads and
validates the current concert after download success is durable. Preserve and
test all current key-based behavior: dependent deduplication, `has_dependent`
for card/status rendering, removal by upstream or dependent key, queued-split
cancellation, and the finish-versus-enqueue race guard. Failure and cancellation
drop the key without submitting a split Job Request.

#### 2. Implement split as a shared Job Request

Refactor `concert-tracker/src/jobs/split.rs` around a `SplitRequest` implementing
`jobs::run::JobRequest`:

```rust
pub(crate) struct SplitRequest {
    concert_id: i64,
    mode: SplitMode,
    config: JobConfig,
}

impl JobRequest for SplitRequest {
    type Input = SplitInput;
    type Setup = SplitJob;
    type Facts = SplitCompletionFacts;
    // validate / try_mark_started / setup / execute /
    // gather_success_facts / commit_success / record_failure
}
```

- `validate` reads the current concert, rejects a missing download or invalid
  current timestamp/set-list relationship before acceptance, and builds a
  typed `SplitInput` without creating temporary files. A downcastable
  `SplitValidationError::NotDownloaded` preserves the existing
  `StartOutcome::NotDownloaded` mapping after `run::submit` returns the typed
  rejection; there is no validation before the registry reservation and no
  duplicated/racy pre-check.
- `try_mark_started` uses `db::lifecycle::try_mark_split_started` as the final
  fallible admission operation.
- `setup` performs race-safe filesystem preparation after acceptance: locate
  the current source, build splitter/timestamp temp files and paths, or identify
  the existing-track automatic recovery case. It also removes stale interlude
  files before Analyze/Reset execution; inability to inspect or remove a stale
  matching file is an accepted setup failure because leaving it can corrupt the
  required coverage fact. Setup errors therefore become accepted failures with
  Failed Job history.
- `execute` runs `JobConfig::run_split`. Automatic recovery completes through
  the same terminal path without invoking the runner.
- `gather_success_facts` verifies required completion facts outside the DB
  mutex through a typed variant. A normal analysis requires current track
  presence and readable generated timestamps. `ExistingTracksRecovery`
  requires current track presence but does not require `timestamps.json`;
  readable timestamps are backfilled when present, while absent/unreadable
  timestamp metadata is a repairable warning that preserves existing stored
  timestamp state. Missing facts required by the selected variant fail the
  accepted run. Test recovery with valid, missing, and malformed
  `timestamps.json`.
- `commit_success` persists `split_at`, the success event, `tracks_present`, and
  mode-required timestamp state in the engine's single transaction.
  User-timestamp mode commits its user timestamps and media duration;
  reset-to-auto clears user timestamps; analysis commits automatic timestamps
  and clears the superseded user cut. The typed `ExistingTracksRecovery`
  variant is distinct from executed analysis: it commits `split_at`, the Split
  event, and track presence; optionally backfills automatic timestamps only
  when readable; and otherwise preserves both stored automatic and user
  timestamp columns. Tests assert missing/malformed recovery metadata cannot
  erase either stored column.
- Repairable/non-required reconciliation remains warning-only and is clearly
  separated from required facts. No required database error is discarded.
- `record_failure`, `has_stale_in_progress`, `log_dir`, and
  `spawn_dependents` provide the same engine behavior as download.
- Map every accepted request, including existing-track automatic recovery, to
  `StartOutcome::Spawned`; retire `AlreadySplit` and update its callers/tests.
  The post-acceptance recovery decision is asynchronous and therefore cannot be
  returned synchronously without adding an unnecessary engine result channel.
  Preserve synchronous `AlreadyRunning` and `NotDownloaded` behavior.

#### 3. Make split lifecycle terminal writes transactional

Update `concert-tracker/src/db/lifecycle.rs` and, where useful,
`concert-tracker/src/db/split_timestamps.rs` with transaction-compatible,
fallible operations so the engine transaction atomically commits:

- split terminal lifecycle columns;
- the split event;
- required track-presence and timestamp facts;
- on unsuccessful outcomes, the Failed Job row (added by `jobs::run`).

The functions must propagate event and required-fact persistence errors. The
transaction includes `SplitStarted` at acceptance as today, then at terminal
success `Split` plus `SplitTimestampsUser` for user mode or
`SplitTimestampsReset` for reset/analyze where those events are currently part
of the public history; failure includes `SplitError`. Rework the timestamp
helpers so event insertion can be fallible inside the encompassing transaction
without duplicate events. Tests force each required event insert to fail and
assert every associated lifecycle/timestamp/failure write rolls back. The
existing non-transactional callers remain supported until their migration is
complete; no archive behavior changes in this issue.

#### 4. Route split cancellation through the engine

Update `concert-tracker/src/lifecycle.rs` to construct `SplitRequest` and call
`jobs::run::cancel` for `JobKind::Split`, matching download. Running split
cancellation, panic, setup failure, and execution failure will each compete at
the terminal gate and create at most one failure outcome and Failed Job row.
Queued automatic split intent is dropped without starting or failing a split
Job Run.

Fresh-state dependent rejection is not a Failed Job because no split was
accepted. Expected ineligibility/no-op and unexpected read/infrastructure
errors receive distinct structured warning fields (`upstream`, dependent key,
and reason); Test Control Job Observations must show zero split starts. Cover a
removed download, deleted concert, and changed set list after queueing. A mode
with user timestamps is never stored on this edge, so current timestamps cannot
become stale inside the automatic request.

#### 5. Update lasting and ephemeral documentation

Update `docs/jobs.md` (linked from `README.md`) so split and download are both
documented as shared-engine implementations, including the intent-only
dependency release diagram and the required-versus-repairable completion rule.
Update `CONTEXT.md` only if implementation reveals a missing durable term.
Finalize this change record with the actual design, review resolutions, and
commands/results. Check all links between README, job documentation, ADR 0005,
and this record.

### TDD slices

Work one red-green slice at a time at the seams above:

- direct pre-acceptance rejection creates neither split lifecycle nor Failed
  Job history;
- concurrent split submissions accept at most one Job Run;
- accepted setup failure and execution failure each create one terminal outcome
  and Failed Job;
- split panic and cancellation races create exactly one terminal outcome;
- successful automatic, user-timestamp, and reset-to-auto runs commit their
  required facts before success becomes visible;
- required completion-fact persistence failure converts success into failure;
- repairable reconciliation failure is logged but does not change success;
- download success releases exactly one intent and rebuilds from changed
  current concert state;
- download failure/cancellation drops intent with no split started event or
  Failed Job;
- duplicate prepare calls retain one dependency, queued cancellation removes
  it, status reports it while queued, and download completion racing enqueue
  does not lose the request;
- every split terminal path releases the registry key and permits retry,
  including success, failure, cancellation, panic, and persistence failure;
- existing HTTP flows remain green, adding focused Job Driver observations
  where lifecycle visibility alone cannot distinguish a dropped intent.

Restart/shutdown conversion of stale accepted Job Runs to transactional Failed
Job history remains explicitly in #127. This issue preserves existing startup
recovery behavior and verifies it does not regress, but does not broaden the
archive/recovery slice.

### Verification

Automated checks during development:

```sh
cargo check -p concert-tracker --tests
cargo test -p concert-tracker --lib -- jobs::split
cargo test -p concert-tracker --lib -- lifecycle::
just test-hurl
just lint
just test-rs
cargo check -p concert-tracker --tests --features test-control
git diff --check
```

Manual verification uses a separate server port, database, and workdir as
required by `CONTRIBUTING.md`. Run the repository's test-control Hurl harness
for `job_chain.hurl` and `split_timestamps_flow.hurl`, then start the same
test-control binary on an unused port with scratch `--db`/`--workdir`. Use the
Test Control API to seed one automatic chain and one user-timestamp concert,
POST the real download/split/reset/cancel routes, poll the real status routes,
and inspect `/jobs`, generated media, and server logs after success, configured
failure, cancellation, and retry. Record the exact commands and scratch paths
in this change record. Run focused Playwright only if rendered UI behavior or
interactions change; otherwise Hurl over real product routes is the browser-
independent verification surface.

### Checklist

- [x] Confirm the two public test seams with the user before writing tests.
- [x] Review this plan adversarially and resolve findings.
- [x] Preserve keyed intent-only dependencies and verify fresh release dispatch.
- [x] Add `SplitRequest` and migrate direct split submission.
- [x] Make required split success/failure facts transactional.
- [x] Route split cancellation through `jobs::run`.
- [x] Add red-green tests at the confirmed seams.
- [x] Update `docs/jobs.md` and finalize this change record.
- [x] Run focused checks, full Rust/Hurl suites, lint, and live verification.
- [x] Run adversarial code review and a follow-up review after fixes.
- [x] Commit, push, and open PR #133 against `job-module` with `Resolves #126`.
- [ ] Wait for the complete post-fix CI result.

## Review and verification record

The implementation followed the plan with one reviewed refinement: cancellation
uses a small `JobCancellation` interface and per-kind cancellation descriptors,
so cancelling a split does not require fabricating an execution mode or
`JobConfig`. Cancellation terminal persistence errors abort/release the task and
registry slot but propagate to the HTTP error boundary; they are never reported
as successful cancellation.

The adversarial code review ran independently on two axes:

- **Standards:** found ignored `read_dir` entry errors, raw-SQL fixture reversal,
  execution-only state in cancellation, and an invalid optional completion-fact
  shape. The follow-up approved the fixes: errors propagate, `SeedContext`
  arranges rejection, typed cancellation descriptors own only cancellation
  state, and mode-specific fact variants make invalid states unrepresentable.
- **Spec:** found false-success cancellation on terminal persistence error,
  incomplete dependent-rejection observability, and missing rollback/race
  coverage. The follow-up approved structured rejection logs and tests for all
  requested current-state, event rollback, panic/retry, recovery metadata, and
  cancellation race cases.

Verification performed after review fixes:

- `cargo check -p concert-tracker --tests --features test-control` — passed.
- Focused `jobs`, `lifecycle`, split, download, and prepare suites — passed.
- `just test-rs` — 806 passed before the final review additions; the final
  post-review rerun is recorded below.
- `just test-hurl` — 12 files / 246 real product HTTP requests passed against an
  isolated server on ports 61043/61042 with the harness's scratch database and
  workdir. This exercised automatic download-to-split, user timestamps,
  reset-to-auto, failure/retry, cancellation/late release, Failed Job history,
  and zero split starts after cancelled download.
- `just lint` — formatting, clippy, shellcheck, TypeScript checking, and oxlint
  passed before the final review additions; the final rerun is recorded below.
- `git diff --check` — passed.
- Final post-review `just test-rs` — 814 passed, 0 skipped.
- Final post-review `just lint` — all Rust, shell, and TypeScript checks passed.

No templates, CSS, or frontend interaction code changed. The isolated live Hurl
server exercised the affected product routes directly, so Playwright would not
add a visual/interaction assertion for this backend-only slice.

The first PR CI run passed frontend, Rust, and ShellCheck, but its Linux
Playwright job exposed an outdated E2E contract: `stub-splitter.js` exited
successfully after creating track files without the `timestamps.json` artifact
that a successful Analyze split is now required to produce. The two automated
split-on-play cases consequently observed a failed split. The stub now writes a
valid `ConcertInfo`-shaped automatic timestamp artifact in Analyze mode while
leaving user-timestamp mode unchanged. A focused local Playwright attempt could
not enter test logic because Chromium hit the repository's known macOS
`SIGTRAP` launch failure; the replacement Linux CI run is the browser
verification for this correction.
