# Stale job cancellation records a Failed Job

Issue [#197](https://github.com/gregwebs/tiny-desk-splitter/issues/197) reports
that a stale Job Run cancellation — a `*_started_at` flag left set because the
process that accepted the run is gone — writes the lifecycle failure columns and
the `*_error` event but no Failed Job row and no log file, so the concert card
and the `/jobs` page disagree about what happened. The operator's chosen fix
("option 1") is to write the Failed Job row, and to also give that Failed Job
its per-job log.

## Problem

`jobs::run::cancel`'s stale branch called `request.record_failure(conn,
CANCELLED_BY_USER)` directly. For each kind that is
`db::lifecycle::mark_download_failed` / `mark_split_failed` /
`mark_archive_failed`: append to `*_errors_json`, clear `*_started_at`, and
record the `*_error` event. Nothing else. The running-cancel branch and the run
task's own failure path both use `commit_failure_tx`, which wraps
`record_failure` **plus** `db::failed_jobs::insert_failed_job` in one
`unchecked_transaction()` and returns the new row id. The stale branch did not.

Two of the four failure paths also left the log missing:

| Path | Failed Job row | log file |
|---|---|---|
| run task failure | yes | yes (`persist_failure_log`) |
| cancel wins the terminal gate | yes | **no** |
| restart/shutdown recovery | yes | **no** (out of scope, see Residual) |
| stale cancellation | **no** | **no** |

Because `/jobs/{id}/log` resolves `<JobConfig::log_dir()>/<id>.log` and falls
back to "Log file not found.", a cancelled Failed Job had no readable log even
when the row existed.

## Change

The stale branch now goes through the engine's one-transaction terminal commit,
then writes the log file that commit makes possible:

```text
stale cancel ──▶ commit_failure_tx ──▶ ONE TX: errors_json + started_at=NULL
                                              + event + jobs row ──▶ id
                                    ──▶ write <id>.log:
                                          "cancelled by user
                                           No execution output is available ..."
```

- `run::cancel` gains a `log_dir: Option<&Path>` parameter. The stale branch
  (`MarkedStaleFailed`) and the won running-cancel branch (`CancelledRunning`)
  both call `commit_failure_tx` and then `persist_cancellation_log`, which
  synthesizes a short body — the cancellation reason plus why no execution
  output exists — because a cancellation never has captured output to persist
  (a won cancellation aborts the task whose `NamedTempFile` held the output; a
  stale cancellation had no task at all).
- `jobs::job_log_path(log_dir, job_id)` names one Failed Job's log file, so the
  `<id>.log` convention now has one definition instead of three copies
  (`run.rs`'s two persist sites and `web/handlers.rs::job_log`).
- `finish_as_failure`'s duplicated captured-output persist block collapsed into
  `persist_failure_log`, so there is one log-writing path for captured output
  and one for synthesized cancellation output.

### Companion fix

A slot that is *reserved but not yet activated* exposes no terminal gate, while
`registry.is_running` is true for a Reserved slot. Before this change a cancel
landing in that admission window fell through to the stale branch and wrote a
spurious `*_error` + event; with the Failed Job + log added it would have
broadened that spurious write. `run::cancel` now checks `registry.is_running`
after dropping queued dependents and returns `NoSuchActiveJob`, writing
nothing; a reserved slot that still has queued dependents keeps the
pre-existing `DroppedQueued` outcome instead. This is what makes `run::cancel`'s
documented "reserved-but-not-yet-accepted slot writes nothing" true while
preserving the `DroppedQueued` shape for `prepare`'s download→split edge.

## Scope notes

- **The won *running*-cancellation log (R5) is beyond the literal issue**, which
  concerns the stale branch. It is kept because the operator's "also include the
  log" reads as "a cancelled Failed Job's `/jobs/{id}/log` opens with content",
  and that path rendered "Log file not found." for exactly the same reason.
- **Cancellation-log persistence runs while `Concerts::cancel_job` holds the db
  mutex**, unlike run-failure-log persistence, which runs after the mutex is
  dropped. Accepted because the write is bounded (create one directory, write
  one small file into a directory the process already creates for every Job Run)
  and strictly after the commit; the rejected alternative (return the failed-job
  id from `cancel` and write after unlock) would reshape `CancelOutcome`,
  `CancelJobOutcome`, and eight test assertions to move a ~200-byte `fs::write`
  off the lock. A log-write failure is `tracing::warn!` and continue, the same
  contract `persist_failure_log` already has for captured output; the Failed Job
  row is the durable record, the log is auxiliary.

## Tests

Regression-first: both new tests were added against the plumbing-only
intermediate state and observed failing before the behaviour was implemented.

- `cancelling_a_stale_job_creates_a_consistent_failed_job` failed with
  `assertion 'left == right' failed: exactly one Failed Job for concert 1
  (left: 0, right: 1)` — the issue's reproduction.
- `cancelling_a_reserved_but_not_accepted_run_writes_nothing` failed with
  `assertion 'left == right' failed: left: MarkedStaleFailed, right:
  NoSuchActiveJob` — the reserved slot fell through to the stale branch.

The implemented tests:

- `cancelling_a_stale_job_creates_a_consistent_failed_job` — loops download,
  split, and archive from the stale row shape and checks all five surfaces:
  lifecycle columns, event history, `db::failed_jobs::list_failed_jobs` (Jobs
  page), `list_for_concert` (concert card), and the `<id>.log` file content.
  Cancelling again is `NoSuchActiveJob`, not a second terminal.
- `stale_cancellation_persistence_failure_writes_no_failed_job_or_log` — a
  trigger-rejected `split_error` event rolls the whole terminal back: no Failed
  Job row, no orphan `<id>.log`, `split_started_at` survives.
- `cancelling_a_running_split_writes_a_cancellation_log` — a won running
  cancellation also gets a log (R5).
- `cancelling_a_reserved_but_not_accepted_run_writes_nothing` — the companion
  fix: `NoSuchActiveJob`, no error-column change, no row, no log.
- `cancelling_a_reserved_upstream_with_queued_dependents_drops_them` — a
  reserved-not-activated upstream with a queued split dependent still returns
  `DroppedQueued` (the pre-existing `prepare` download→split shape): the
  dependent edge is gone and nothing else is written.
- `web::handlers::tests::job_log_renders_the_log_a_cancellation_wrote` — drives
  `cancel_job` then `job_log` on a real `AppState` and asserts the rendered body
  contains "cancelled by user", the synthesized "No execution output" marker,
  the stale-row reason phrase, and not "Log file not found.".

## Residual

`run::recover_failed` still writes no log file, so a restart/shutdown-recovery
Failed Job opens with "Log file not found." Recovery is not a user cancellation;
fixing it needs the log directory threaded into `lifecycle::fail_in_progress_jobs`
and its two call sites (`bin/concert_web.rs` at startup and shutdown). Recorded
in `docs/jobs.md`, not fixed here.
