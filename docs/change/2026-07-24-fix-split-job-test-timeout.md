# Reliable split Job Run completion in tests

Issue: [#170](https://github.com/gregwebs/tiny-desk-splitter/issues/170)

Status: Implemented

## Problem

`jobs::split::tests::library_backend_splits_user_timestamps_end_to_end` starts
the same asynchronous, FFmpeg-backed Split Job Run used by production, but its
test helper polls `JobRegistry::is_running` only 100 times at 10 ms intervals.
Loaded CI runners can require more than this roughly one-second budget even
when the split is progressing normally. When the helper panics, its temporary
workdir is removed while the blocking split worker is still running, obscuring
the original timeout with follow-on missing-file errors.

The root-cause investigation reproduced the exact panic in 31/32, 30/32, and
32/32 concurrent runs. A 32-way control using the same polling helper with a
fast typed runner passed every run, confirming that the fixed timing assumption
rather than Job Registry cleanup is the defect.

## Implemented design

### Generation-bound slot-release observation

`concert-tracker/src/jobs/mod.rs` now gives every test-built `JobSlot` a Tokio
notification shared with observers. The crate-visible, unit-test-only
`JobRegistry::wait_until_released(&JobKey)` method:

1. binds to the current slot generation by cloning that slot's completion
   notification while holding the slot mutex,
2. registers its waiter while still holding the mutex,
3. records whether that exact slot's task is already finished,
4. releases the mutex and returns immediately when the slot is absent or its
   task is finished, and
5. otherwise awaits the bound generation's release notification exactly once.

Registering while holding the same mutex every removal path needs prevents a
completion between observation and suspension from becoming a lost wakeup.
Binding once prevents a rapid re-run with the same `JobKey` from making an
observer for run A follow run B. Every path that removes or replaces an
occupied slot notifies that generation's observers before removal,
including normal release, cancellation, bulk cancellation, reservation
rollback, and replacement of a finished-handle slot.

The final API is test-only. Production `JobSlot`s carry no notification field
or allocation. A shared `remove_job_slot` function keeps the notify-before-
remove invariant in one place for single-slot removal; `cancel_all` signals
each slot while draining the map.

This is specifically a **slot-release** contract, not a promise that every
underlying operating-system or blocking thread has stopped. Ordinary Job Run
release occurs after terminal persistence and after the execution future
returns, which is the contract the split tests need. Cancellation can detach
non-abortable `spawn_blocking` work; canonical documentation calls out that
existing limitation rather than overstating the test seam.

### Split-test wait and timeout diagnostics

`concert-tracker/src/jobs/split.rs` now:

- replaces the fixed iteration loop with the signalled slot-release wait,
- defines a named 30-second deadlock ceiling for real media work,
- passes the database into the helper and uses one snapshot-formatting helper so
  timeout output includes the key, timeout duration, registry running state,
  and last persisted split lifecycle (`split_started_at`, `split_at`, and
  split errors),
- uses `try_lock` for both registry and database diagnostic snapshots so a held
  or poisoned mutex cannot hide or indefinitely extend the primary timeout.

The timeout remains a failure detector for a genuinely stuck job; it is no
longer the expected duration of ordinary work.

### Regression coverage

Focused `JobRegistry` tests cover:

- an observer waiting before normal release is released,
- reservation rollback releases its observer,
- `abort_and_release` releases its observer,
- `cancel_all` releases all observers,
- replacement of a finished-handle slot releases the old generation and a
  waiter bound to the old generation does not follow the replacement,
- an observer starting with no slot returns immediately,
- an observer of a finished handle still occupying its slot returns
  immediately, and
- poisoned registry diagnostics preserve useful timeout context.

The removal tests use a oneshot readiness handshake emitted after `Notified`
is enabled. They therefore prove the observer registered before the test
triggers removal instead of relying on scheduler timing.

The existing
`library_backend_splits_user_timestamps_end_to_end` test as the behavioral
regression because it exercises the production `start_split` → library adapter
→ blocking FFmpeg → terminal commit chain.

### Documentation

`docs/jobs.md` is the canonical description of the unit-test slot-release
contract, its generation semantics, and its cancellation limitation. Its state
diagram is reachable through README's existing Job execution link.

## State changes

```text
Test observer                         Job Registry / Job Run
     │                                         │
     ├── bind to generation A release ─────────▶│ Run A: Running
     │                                         │
     ├── register under slot lock ────────────▶│
     │                                         ├── terminal commit
     │                                         ├── notify release observers
     │◀──────── release notification ──────────┤
     ├── observe slot absent/finished          ├── release slot
     ▼                                         ▼
assert persisted terminal state             Released after terminal

Observer arrives after release ──▶ slot absent ──▶ return immediately
Run B reserves same JobKey ──▶ observer remains bound to completed run A
Deadlocked Job Run ──▶ outer timeout ──▶ panic with registry + DB state
```

## Verification performed

- Red reproduction before implementation: three 32-way batches of the real
  FFmpeg-backed test produced 31/32, 30/32, and 32/32 exact
  `split job did not finish` failures.
- Red focused tests: the initial registry completion test command failed
  because the observation API did not exist.
- Focused registry slot-release tests: 7 passed.
- Timeout snapshot and poisoned-registry diagnostic tests: passed.
- Single FFmpeg-backed library split E2E: passed.
- Review-driven 32-way FFmpeg stress batch: 32 passed, zero timeout failures.
- `cargo test -p concert-tracker --no-default-features`: 595 tests passed after
  the review additions (592 before the review additions).
- `just test-rs`: 850 workspace tests passed after review (847 before the
  review additions).
- Format and Clippy: passed with `just lint` up to the shellcheck wrapper.
- ShellCheck: passed when the tracked-script discovery from
  `scripts/shellcheck.sh` was run directly under zsh. The aggregate
  `just lint` wrapper cannot finish on this host because macOS Bash 3.2 lacks
  its `mapfile` builtin; this is unrelated host tooling, not a lint finding.
- `just ts-check` and `just ts-lint`: passed.
- Manual live smoke check: built `concert-web`, started it on port 43170 with
  `/private/tmp/tiny-desk-splitter.issue170.wfHfVD/{concerts.db,workdir}`,
  confirmed `/health` returned `concert-web` through
  `scripts/local-api-request.sh`, and stopped it cleanly with Ctrl-C.
- Adversarial review found slot-release naming, deterministic registration,
  poison-safe diagnostics, and Change Record gaps. All code/spec findings were
  fixed; follow-up Spec review reported no findings. Follow-up Standards review
  requested only this Change Record finalization.

## Checklist

- [x] Slot-release signal cannot lose a wakeup.
- [x] Slot-release observation stays bound to one Job Run generation.
- [x] Every slot-removal path notifies observers.
- [x] Existing split E2E awaits terminal release.
- [x] Timeout reports registry and persisted lifecycle state.
- [x] Focused tests cover waiting, absent, and finished-handle observers.
- [x] Canonical Job Run documentation includes test synchronization.
- [x] Change Record contains final verification results.
- [x] Adversarial code review has no unresolved code/spec findings.
- [ ] Pull request resolves #170 and CI passes.
