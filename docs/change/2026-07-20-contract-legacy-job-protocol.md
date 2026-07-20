# Contract the legacy job execution protocol

Implements [#128](https://github.com/gregwebs/tiny-desk-splitter/issues/128),
the final **contract** step of
[#124 — Deepen concert job orchestration](https://github.com/gregwebs/tiny-desk-splitter/issues/124)'s
expand-migrate-contract sequence. #125 introduced the `jobs::run` engine
(downloads), #126 migrated split, and #127 migrated archive plus
restart/shutdown recovery. All three kinds already started via `run::submit`,
cancelled via `run::cancel`, and recovered via `run::recover_failed` before
this change — the engine was already the sole *execution* protocol. What
remained were legacy admission/cancellation primitives kept alive only
because `#[cfg(test)]` code still called them, so the deep orchestration
module was not yet the sole Job Run *protocol*. This PR removes them and
makes the job vocabulary reachable from a README-linked canonical location.

Scope was confirmed with the user as **dead legacy only**: the
`start_download`/`start_split`/`start_archive` typed adapters and the twin
`*Cancellation`/`*Request` structs are kept (they are the module's current
interface, not legacy — the `*Cancellation` duplication was reviewed and
accepted as pre-existing convention in the #127 review). The scrape queue is
untouched (that is #129). No new public adapter seam was added; the
`JobRunner` production/test-control adapters are unchanged.

## What changed

1. **Removed `JobRegistry::insert`** (`jobs/mod.rs`), the "legacy admission
   path for split/archive during migration" that created an already-accepted
   slot outside the reservation flow. It had zero non-test callers. Kept
   `JobSlot.handle: Option<JoinHandle<()>>` unchanged — its `None` variant is
   the **Reserved** state used by `try_reserve`/`activate`, core to the
   engine, not a legacy artifact; only its doc comment was reworded to drop
   the "Legacy `insert`" clause.
2. **Removed the legacy single-key registry cancellation**: `JobRegistry::cancel`,
   `cancel_with_outcome`, and `enum RegistryCancelOutcome`. Production
   cancellation has routed through `jobs::run::cancel` since #125–127; these
   had only test callers. Kept `cancel_all` (the graceful-shutdown path used
   by `bin/concert_web.rs`) — it has its own drain loop independent of
   `cancel_with_outcome`.
3. **Removed dead `persist_job_log`** (`jobs/mod.rs`) — no callers; the
   engine's `finish_as_failure` (`jobs/run.rs`) already owns log persistence
   for every kind.
4. **Dropped `cancel_job`'s unused `_jobs: &JobConfig` parameter**
   (`lifecycle.rs`) — a residual from the pre-migration cancellation path.
   Updated the sole production call site (`web/handlers.rs`) and the five
   lifecycle cancel tests.
5. **Migrated every `#[cfg(test)]` caller** of the removed primitives onto the
   real admission API: a small test-only `reserve_running(registry, key,
   handle)` helper (added identically in `jobs/mod.rs` and `lifecycle.rs`)
   calls `try_reserve` + `activate` to produce an accepted, cancellable slot
   — the same shape `insert` produced, but through the real reservation flow.
   `jobs/split.rs`'s three teardown-only `registry.cancel(&key)` calls (each
   just stopping a background sleeper so its test exits promptly) became
   `registry.cancel_all()`. Registry-level tests that exercised the removed
   `cancel`/`cancel_with_outcome` were rewritten to test the primitives
   `run::cancel` itself now calls directly: `abort_and_release` (replacing
   `cancel_aborts_running_task`) and `drop_dependency_edges` (replacing the
   two dependent-dropping tests and the unknown-key test).
   `insert_legacy_slot_has_a_winnable_terminal_gate`, which tested the
   removed `insert` specifically, was deleted outright.
6. **Documentation**: linked `CONTEXT.md` from `README.md`'s "Repository
   documentation" list, so the canonical Job Request / Job Run / Failed Job /
   Job Run Recovery vocabulary is reachable by following README links (it was
   previously only referenced in prose from `docs/jobs.md`, not linked
   itself). Updated `docs/jobs.md`'s "#128 will contract the legacy protocol"
   forward-reference to past tense, naming what was actually removed, and
   added a short **Failed Job** narrative paragraph tying the state diagram
   to the term (a `jobs` table row committed in the same transaction as the
   lifecycle failure columns and event). Reworded three stale code comments
   that referenced now-removed things: `jobs/mod.rs`'s `JobSlot.handle` doc
   comment, `jobs/run.rs`'s `CANCELLED_BY_USER` doc comment ("legacy cancel
   path" → names `cancel_job` routing through `run::cancel`), and
   `jobs/run.rs`'s `log_dir` doc comment (referenced the removed
   `persist_job_log`).

## Behavior changes (intentional)

None. This is a pure dead-code removal and test migration — every
non-test code path was already routing through `jobs::run` before this
change; no production behavior differs.

## Tests

No new production behavior to test. Test migrations preserve or improve
coverage:

- `jobs/mod.rs`: `abort_and_release_aborts_running_task_and_frees_the_slot`
  (new name for the migrated `cancel_aborts_running_task`) exercises
  `abort_and_release` directly — the primitive `run::cancel` actually calls
  on a won gate, which the old test only exercised indirectly through the
  removed `cancel`. `drop_dependency_edges_drops_queued_dependents_of_the_key`
  and `drop_dependency_edges_removes_the_key_from_other_upstreams_queues`
  similarly now test the real primitive directly instead of through the
  removed wrapper, and no longer need a throwaway spawned task to do it.
  `drop_dependency_edges_returns_false_for_unknown_key` replaces
  `cancel_returns_false_for_unknown_key`. `cancel_all_aborts_all_running_tasks`
  keeps its assertions, rebuilt on `reserve_running`.
- `lifecycle.rs`: all five cancel/recovery tests
  (`cancel_distinguishes_running_queued_stale_and_absent_jobs`,
  `cancelling_a_running_{download,split,archive}_creates_failed_job_history`,
  `split_cancellation_reports_terminal_persistence_failure`) pass unchanged
  in assertions, only their setup now uses `reserve_running` instead of
  `registry.insert` and their `cancel_job` calls drop the removed parameter.
- `jobs/split.rs`: the three tests whose teardown called the removed
  `registry.cancel` now call `registry.cancel_all()`; their actual
  assertions are untouched.

## Verification performed

- `cargo check -p concert-tracker --tests` — passed.
- `cargo check -p concert-tracker --tests --features test-control` — passed.
- `cargo test -p concert-tracker --lib -- jobs:: lifecycle::` — 151 passed.
- `cargo clippy -p concert-tracker --tests` — clean, no warnings.
- `just test-rs` — 819 passed across the workspace (`cargo nextest run --tests`).
- `just lint` — `cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, shellcheck, `ts-check`, and oxlint all
  passed clean.
- `git diff --check` — passed (no whitespace errors).
- Grep-verified no remaining references to `JobRegistry::insert`,
  `registry.insert(`, `cancel_with_outcome`, `RegistryCancelOutcome`,
  `persist_job_log`, or single-key `registry.cancel(` anywhere in
  `concert-tracker/src`; the only surviving mentions are the new test-helper
  doc comments explaining what they replace, and the historical #127 change
  record (which correctly describes past state).

Manual, against an isolated `concert-web --features test-control` process on
ephemeral ports with a scratch `--db`/`--workdir` under
`/private/tmp/.../scratchpad/verify-128` (never the real `concerts.db`):

1. **Cancellation race through the real HTTP route**: seeded a concert via
   Test Control, configured the Job Driver to block its download, started it
   with the real `POST /concerts/{id}/download`, confirmed
   `blocked=1` via `test.assert_job_observation`, then cancelled it with the
   real `POST /jobs/{id}/cancel/download`. `/jobs` showed exactly one row
   ("cancelled by user"). Released the still-parked download step afterward
   (`test.job_release` with `outcome: "succeed"`) and confirmed `/jobs` still
   showed exactly one row and `/concerts/{id}` gained no
   `status-downloaded`/`badge-downloaded` indicator — the terminal gate
   correctly prevented the released step from resurrecting success after
   cancellation already won it.
2. **Restart recovery**: set `download_started_at` directly in the scratch
   db via `sqlite3` to simulate a crash between accept and terminal commit
   (the first server process was left running rather than killed — this
   sandbox blocks sending signals to background processes it doesn't
   supervise — but recovery only runs at startup/shutdown regardless of
   whether the earlier process is still up, so a second `concert-web`
   instance was started against the same scratch db/workdir on fresh ports).
   Its startup log read `marked 1 stale download(s), 0 stale split(s), and 0
   stale archive(s) as failed on startup`. `/jobs` on the new instance showed
   the recovery row (`download` / `server restarted`) alongside the earlier
   cancellation row (2 rows total), and the db showed
   `download_started_at` cleared with `download_errors_json` recording
   `"server restarted"` — confirming the untouched recovery path still works
   end to end after this diff's removals.

Both scratch server processes were left running (bound to ephemeral loopback
ports, isolated scratch db/workdir, no production data) since this sandbox
does not permit sending them a kill signal from this session; they hold no
locks or state that affect anything outside their own scratch directory.

## Review record

An adversarial code review ran independently on two axes (Standards and
Spec) against the full staged diff. Both axes reported no hard findings.

- **Standards**: no violations of `CODING_STANDARDS.md`/`CONTRIBUTING.md`.
  Two judgement calls, both accepted as-is: the two-line `reserve_running`
  test helper is duplicated verbatim between `jobs/mod.rs` and
  `lifecycle.rs` rather than shared, which `CODING_STANDARDS.md`'s DRY
  section explicitly permits below three call sites; and the three
  `jobs/split.rs` teardown swaps to `registry.cancel_all()` are a broader
  operation (clears the whole registry) than the narrower removed `cancel`
  call, but harmless since each test owns a single-purpose registry.
- **Spec**: all five #128 acceptance criteria satisfied; no scope creep
  beyond the confirmed "dead legacy only" narrowing; the test migrations
  (`reserve_running`, the `abort_and_release`/`drop_dependency_edges`
  rewrites, the `cancel_all` teardown swaps) were verified to preserve or
  tighten the coverage of what they replaced, not just compile.

(Note: the repo's `/codex:rescue`-routed review path failed with a
consistent `EPERM` opening Codex's own job-log file across multiple
attempts, including with the sandbox disabled — an environment/runtime
issue, not a code issue. The review fell back to standard Claude subagents
for both axes instead.)
