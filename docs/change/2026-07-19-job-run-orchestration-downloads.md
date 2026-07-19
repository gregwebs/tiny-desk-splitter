# Job Run orchestration through downloads

Implements [#125](https://github.com/gregwebs/tiny-desk-splitter/issues/125),
the first sub-issue of [#124 — Deepen concert job orchestration](https://github.com/gregwebs/tiny-desk-splitter/issues/124).
Downloads now go through a new `jobs::run` engine that gives every download
Job Request race-safe admission, exactly one terminal outcome, and Failed Job
history on any unsuccessful outcome (including cancellation). Split and
archive are unchanged for now — this PR is the **expand** step of #124's
expand-migrate-contract sequence; #126 migrates split, #127 migrates archive
and restart/shutdown recovery, #128 contracts the legacy protocol.

The domain vocabulary (Job Request / Job Run / Failed Job) was already
recorded in `CONTEXT.md` on this branch prior to this change.

## Defects fixed

The pre-#125 `jobs/download.rs` hand-rolled admission, and had five real
defects this PR closes for downloads:

1. **Acceptance was not atomic with registry state.** `start_download`
   checked `registry.is_running`, then did the DB started-transition, then
   spawned and inserted the handle — a registry reservation never existed
   before acceptance, so an error between the DB transition and spawning
   (e.g. `get_concert` failing) left `download_started_at` stuck with no
   terminal outcome and no Failed Job.
2. **No single terminal outcome.** Cancellation aborted the tokio task
   *then* wrote the failure — the abort could land mid-success-commit. A
   panic in the spawned task produced no terminal state at all.
3. **Inconsistent failure history.** Execution failure recorded a `jobs`
   row; cancellation did not. Lifecycle columns, the event, and the `jobs`
   row were separate, non-transactional writes.
4. **Dependents released even when success persistence failed.**
   `mark_download_succeeded`'s `Result` was discarded (`let _ =`) and
   `spawn_dependents` ran regardless.
5. **Registry entries were never released** — finished handles lingered in
   the map, masked only by `is_finished()`.

## Design

See [`docs/jobs.md`](../jobs.md#job-run-orchestration) for the state diagram
and the three invariants (no `.await`/async-FS between gate claim and
transaction commit; DB → registry lock ordering only; the terminal-gate
winner owns `release`) that make the engine safe under `tokio::spawn`/abort
and a shared `Arc<Mutex<Connection>>`.

Key pieces:

- `JobRegistry` (`concert-tracker/src/jobs/mod.rs`) gained `JobSlot`
  (`Option<JoinHandle>` + `Arc<TerminalGate>`), `try_reserve`/
  `JobReservation` (admission-rollback-on-drop guard), `ActivationSignal`
  (parks the spawned run task until its handle is attached, closing a race
  where a trivially-fast run could finish before the registry even knows
  about its own handle), `release`, `abort_and_release`, and
  `terminal_gate`. Legacy `insert` (still used by split/archive) creates an
  already-accepted slot with a fresh gate, so cancellation for those kinds
  is unaffected.
- `jobs::run` (new module) is the engine: the `JobRequest` trait
  (`validate` / `try_mark_started` / `setup` / `execute` /
  `gather_success_facts` / `commit_success` / `record_failure` /
  `has_stale_in_progress` / `log_dir` / `spawn_dependents`), `submit`, and
  `cancel`. `commit_success`/`record_failure` run inside one
  `unchecked_transaction` (the same pattern as `db::playlists::add_playlist_item`).
- `events::try_record_now` (new, `concert-tracker/src/events.rs`) is a
  fallible sibling of the existing best-effort `record_now`; the download
  `mark_download_succeeded`/`mark_download_failed` functions
  (`concert-tracker/src/db/lifecycle.rs`) now use it so an event-insert
  failure rolls back the terminal transaction instead of being logged and
  swallowed — required for the "lifecycle state + event + Failed Job commit
  atomically" acceptance criterion.
- `jobs::download::DownloadRequest` implements `JobRequest`;
  `start_download`'s signature and `StartOutcome` are unchanged, so
  `web/handlers.rs`, `prepare.rs`, and `spawn_dependents` needed no changes.
- `lifecycle::cancel_job` gained a `&JobConfig` parameter and routes
  `JobKind::Download` through `jobs::run::cancel`; `Split`/`Archive` keep
  the pre-existing registry-only path.

## Behavior changes (intentional)

- Cancelling a running **download** now records a Failed Job row (visible
  on `/jobs`); previously only the lifecycle error column was written.
- A download success whose persistence fails now yields a Failed terminal
  and does **not** start a queued split (previously dependents spawned
  regardless of the `Result`).
- A pre-acceptance validation failure no longer leaves `download_started_at`
  set.

## Tests

New `jobs::run::tests` (11 tests) exercise the engine directly against a
test-only `JobRequest` and real in-memory SQLite (no DB mocks, per
`docs/backend-persistence.md`): admission races, synchronous rejection,
post-acceptance setup failure, execution failure, panics in both setup and
execute, success persistence failure, cancellation while blocked, cancellation
after the run already won the gate, and an instantly-completing run (the
activation-parking race). New `jobs::mod.rs` tests (9 tests) cover the
registry primitives directly (reservation occupancy, rollback-on-drop,
activation, the terminal gate's single-winner guarantee). A new
`lifecycle::tests::cancelling_a_running_download_creates_failed_job_history`
pins the new cancel-creates-Failed-Job behavior. All pre-existing
`jobs::download`, `jobs::prepare`, and `lifecycle` tests pass unchanged.

## Verification performed

- `cargo check -p concert-tracker --tests`
- `cargo test -p concert-tracker --lib -- jobs::`
- `cargo test -p concert-tracker --lib -- lifecycle::`
- `cargo clippy -p concert-tracker --tests`
- `cargo fmt -p concert-tracker` (clean)
- `cargo build --workspace --tests`
- `just test-rs` / `just lint` / `just test-hurl` (see PR CI)
