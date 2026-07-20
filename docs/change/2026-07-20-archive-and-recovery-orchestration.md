# Migrate archiving and Job Run recovery

Implements [#127](https://github.com/gregwebs/tiny-desk-splitter/issues/127),
the third implementation slice of
[#124 — Deepen concert job orchestration](https://github.com/gregwebs/tiny-desk-splitter/issues/124).
The PR is based on the `job-module` parent branch established by #125 and
extended by #126. This is the last **migrate** step of #124's
expand-migrate-contract sequence — #128 will contract the legacy protocol now
that download, split, and archive all share `jobs::run`.

## Purpose

Archive Job Requests still hand-rolled admission and lifecycle exactly as
pre-#125 download did: a bare `registry.is_running` check, a direct
`try_mark_archive_started` call, `registry.insert`, and a `run_archive` task
that wrote terminal state with `let _ =` — discarding lifecycle/event errors —
and non-transactionally. Restart recovery and graceful shutdown
(`fail_in_progress_jobs`) had the same shape: they appended a lifecycle error
column and a best-effort event, but created **no Failed Job history** for a
stale accepted Job Run, unlike every other unsuccessful outcome.

This change:

1. Moves archive onto the `jobs::run` engine (`ArchiveRequest`/
   `ArchiveCancellation`), so archive admission, success, failure, panic, and
   cancellation get the same race-safe-admission, exactly-one-terminal-outcome,
   Failed-Job-on-any-unsuccessful-outcome guarantees download and split
   already have.
2. Gives restart/shutdown recovery a transactional terminal commit
   (`jobs::run::recover_failed`, reusing the engine's private
   `commit_failure_tx`) so a stale accepted Job Run — of any kind — becomes a
   proper Failed Job with the recovery reason, not just an error-column
   append.
3. Removes the legacy registry-only cancellation fallback in
   `crate::lifecycle::cancel_job` (all three kinds now route through
   `jobs::run::cancel`) and a second, divergent, test-only implementation of
   `fail_in_progress_jobs` in `db::lifecycle` that never created Failed Jobs.

Archive-specific filesystem behavior — `do_archive`'s rename-or-copy +
symlink, and its safety checks — is unchanged and stays owned by archive's
`execute`, run on a blocking thread inside the engine future exactly as
`run_archive` ran it before.

## Design

See [`docs/jobs.md`](../jobs.md#job-run-orchestration) for the updated
orchestration diagram (now covering all three kinds) and the new
[Recovery](../jobs.md#recovery-stale-accepted-job-runs) section, and
`CONTEXT.md`'s new **Job Run Recovery** term.

Key pieces:

- `jobs::archive::ArchiveRequest` implements `JobRequest`: `validate` reads
  the concert and rejects synchronously (`ArchiveValidationError`, downcast to
  `StartOutcome::NothingToArchive`, exactly mirroring split's
  `SplitValidationError::NotDownloaded`) when neither `downloaded_at` nor
  `split_at` is set, before building the source/dest paths; `try_mark_started`
  reuses `try_mark_archive_started` unchanged; `setup` is the identity (no
  separate preparation step, like download); `execute` runs `do_archive` on a
  `spawn_blocking` thread and maps its `Result`/panic to `JobStepOutcome`;
  `commit_success` reuses `mark_archive_succeeded`. `start_archive` keeps its
  exact pre-existing signature and 3-variant `StartOutcome`, so the one caller
  (`web/handlers.rs`'s `archive` route, which already ignores the outcome)
  needed no change.
- `jobs::run::recover_failed(conn, &impl JobCancellation, reason)` is a small
  new engine entry that calls the engine's existing private
  `commit_failure_tx` directly — no `JobRegistry` reservation, no
  `TerminalGate` claim. It is safe without either only because of where its
  sole caller, `crate::lifecycle::fail_in_progress_jobs`, runs: before the
  registry exists at startup, and after `JobRegistry::cancel_all` has already
  aborted and released every slot at shutdown. `*_started_at` is therefore the
  sole recovery coordination signal, and `fail_in_progress_jobs` holds the db
  mutex across its whole select-and-commit loop so no run task can interleave
  within it.
- `db::lifecycle::mark_archive_succeeded`/`mark_archive_failed` switched from
  best-effort `events::record_now` to fallible `events::try_record_now`,
  matching download/split — required so an event-insert failure aborts the
  encompassing terminal transaction in both the engine's own commit and
  `recover_failed`'s.
- `crate::lifecycle::cancel_job` now routes `JobKind::Archive` through
  `jobs::run::cancel` alongside Download/Split, which made the legacy
  `registry.cancel_with_outcome` fallback branch (and the free helpers
  `mark_job_failed`/`has_stale_in_progress` used only by it) dead code;
  removed. `registry.cancel_with_outcome` itself is untouched — it keeps a
  live test caller and is a #128 concern.
- `db::lifecycle::fail_in_progress_jobs`, a second recovery implementation
  used only by its own two tests and never called in production (both
  `bin/concert_web.rs` call sites already used `crate::lifecycle`'s version),
  removed along with those tests, leaving one canonical recovery path.

## Behavior changes (intentional)

- Cancelling a running **archive** now records a Failed Job row (visible on
  `/jobs`); previously only the lifecycle error column was written (matches
  #125's download precedent and #126's split migration).
- Restart recovery and graceful shutdown now create a Failed Job for **every**
  stale accepted Job Run (download, split, and archive), not just an error
  column — the pre-#127 `/jobs` history was silently incomplete after any
  unclean shutdown or restart.
- An archive success whose persistence fails (e.g. the `archived` event insert
  fails) now yields a Failed terminal instead of a partially-written,
  non-transactional state.

## Known pre-existing residual (not introduced by this change)

`do_archive` runs on a detached `spawn_blocking` thread that cannot itself be
cancelled/aborted. If a graceful shutdown aborts an in-flight archive's outer
future, the underlying blocking thread keeps running; in principle it could
still reach `commit_success` after `fail_in_progress_jobs` has released the db
mutex, landing both `archived_at` and a "server shutdown" Failed Job for the
same concert. This is identical to the pre-#127 `run_archive` task's behavior
and not made worse by this change; the window is narrow (abort happens at the
`spawn_blocking` await almost immediately, long before a real directory move
completes). Documented in `docs/jobs.md`'s Recovery section as a known
limitation, not fixed here.

## Tests

New tests in `jobs::archive::tests` (5): synchronous `NothingToArchive`
rejection creates no lifecycle/event/Failed-Job history; concurrent
`start_archive` accepts exactly one; successful archive sets `archived_at`,
creates the symlink, and emits the `archived` event; execution failure
(missing source directory) produces one Failed Job named `"archive"`; success
whose `archived` event insert is rejected by a trigger produces a Failed Job
and leaves `archived_at` unset.

New/updated tests in `lifecycle.rs`: `cancelling_a_running_archive_creates_failed_job_history`
(new, mirrors the existing download/split cancellation tests);
`restart_recovery_marks_stale_download_split_and_archive_jobs_failed` (updated
in place to assert exactly one Failed Job per kind, not just the error
columns); `recovery_is_transactional_per_kind` (new: a rejected `split_error`
event trigger rolls back only the split row's terminal write, leaving its
`split_started_at` and Failed Job history untouched, while an unrelated
download row processed earlier in the same call still committed); `recovery_does_not_re_fail_an_already_committed_success`
(new: a row whose `*_started_at` is already cleared because its Job Run
already committed success is left untouched by recovery).

All pre-existing `jobs::archive`, `lifecycle`, and `db::lifecycle` tests pass
unchanged (aside from the one updated assertion above and the two removed
duplicate-recovery tests).

## Verification performed

Automated:

- `cargo check -p concert-tracker --tests` — passed.
- `cargo test -p concert-tracker --lib -- jobs::archive jobs::run lifecycle:: db::lifecycle`
  — 82 passed.
- `cargo clippy -p concert-tracker --tests` — clean, no warnings.
- `cargo test -p concert-tracker --lib` — 555 passed (full `concert-tracker`
  lib suite).
- `just test-rs` — 820 passed across the workspace (`cargo nextest run --tests`).
- `just lint` — `cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, shellcheck, `ts-check`, and `oxlint` all
  passed clean.
- `just test-hurl` — 12 files / 245 real product HTTP requests passed against
  an isolated `concert-web --features test-control` server with the harness's
  scratch database and workdir.
- `cargo check -p concert-tracker --tests --features test-control` — passed.
- `git diff --check` — passed (no whitespace errors).

Manual, against an isolated `concert-web --features test-control` process on
ephemeral ports with a scratch `--db`/`--workdir` under
`/private/tmp/.../scratchpad/archive-verify` (`dest/` as a separate archive
location, never the real `concerts.db`):

1. **Successful archive**: seeded a downloaded+split concert with a real
   source file and one track file (`test.seed_media_concert`), set
   `archive_location` via the real `POST /settings` route, then
   `POST /concerts/{id}/archive` (the real product route). Confirmed the
   source directory was moved to `dest/`, a symlink was left in its place,
   the concert card rendered `status-archived`/`badge-archived`, and `/jobs`
   reported "No failed jobs."
2. **Execution failure**: seeded a downloaded concert with no source file on
   disk, then `POST .../archive`. Confirmed `/jobs` showed exactly one
   `Archive` Failed Job with message `source directory does not exist: ...`.
3. **Restart recovery**: seeded a third downloaded concert, then (with the
   server stopped) set `archive_started_at` directly in the scratch sqlite
   db to simulate a crash between accept and terminal commit, and restarted
   the server. Startup logged `marked 0 stale download(s), 0 stale split(s),
   and 1 stale archive(s) as failed on startup`; the concert's
   `archive_started_at` was cleared, `archive_errors_json` recorded
   `"server restarted"`, an `archive_error` event was written, and the
   `jobs` table gained a row (`name="archive"`,
   `failure_message="server restarted"`) — confirming restart recovery now
   produces transactional Failed Job history, not just an error column.

Archive is real filesystem work, not routed through the test-control
`JobRunner`, so Hurl/Playwright add nothing beyond the Rust engine tests and
this manual live run — which is the verification surface for this slice, per
the #126 record's precedent.

## Review record

An adversarial code review ran independently on two axes (Standards and Spec)
against the full uncommitted diff. Both axes independently converged on the
same single real finding: `jobs::run.rs`'s module doc comment still read
"archive keeps its hand-rolled admission/lifecycle code until it migrates in
#127" — stale as of this diff, and inconsistent with `docs/jobs.md`/
`CONTEXT.md`, which this same diff correctly updated. Fixed by rewriting the
module doc to state all three kinds route through the engine and mention
`recover_failed`. A minor readability nit (`ArchiveJob::execute` rebuilding
its clone field-by-field instead of `#[derive(Clone)]`) was also applied.
Both axes otherwise reported the acceptance criteria fully satisfied, no
scope creep, and no logic/race-condition defects; two baseline "Duplicated
Code" smells (the `JobCancellation` impl pairs, and a `wait_until_finished`
test helper repeated across `download.rs`/`split.rs`/`archive.rs`) were noted
as pre-existing repo convention rather than new duplication, so left as-is.

Verification after the fixes: `cargo clippy -p concert-tracker --tests` and
`cargo test -p concert-tracker --lib -- jobs::archive jobs::run lifecycle::`
(82 passed) reran clean; `just lint` and `just test-rs` (820 passed) reran
clean; `git diff --check` clean.
