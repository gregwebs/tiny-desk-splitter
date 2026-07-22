# Job execution

`concert-tracker` runs download, split, archive, and opener work through the
job modules under `concert-tracker/src/jobs/`.

Download, split, and archive routes share the same lifecycle orchestration:

- the route or workflow validates the concert state,
- `JobRegistry` prevents duplicate running jobs,
- dependency edges queue follow-up jobs such as download then split,
- lifecycle persistence records started, succeeded, or failed state,
- successful split completion refreshes track availability and timestamp state,
- failed jobs record user-visible errors and job logs.

## Job Run orchestration

Download, split, and archive all use the `jobs::run` engine (issues
[#125](https://github.com/gregwebs/tiny-desk-splitter/issues/125),
[#126](https://github.com/gregwebs/tiny-desk-splitter/issues/126), and
[#127](https://github.com/gregwebs/tiny-desk-splitter/issues/127), part of the
deepening tracked by [#124](https://github.com/gregwebs/tiny-desk-splitter/issues/124)).
[#128](https://github.com/gregwebs/tiny-desk-splitter/issues/128) contracted
the legacy protocol: the registry's pre-reservation `insert` admission path
and single-key `cancel`/`cancel_with_outcome` are gone now that all three
kinds share the engine end to end, and `jobs::run` is the sole Job Run
protocol. See [`CONTEXT.md`](../CONTEXT.md) for the **Job Request** / **Job
Run** / **Failed Job** / **Job Run Recovery** domain vocabulary this section
assumes.

A **Job Request** (`jobs::run::JobRequest`) becomes a **Job Run** only after
acceptance. Every accepted Job Run reaches exactly one terminal outcome —
succeeded, failed, or cancelled — and every unsuccessful terminal outcome
creates **Failed Job** history: a row in the `jobs` table, inserted in the
same transaction as the lifecycle failure columns and event, retained for
inspection on the `/jobs` page. A rejected Job Request is never a Failed Job
— rejection happens before acceptance, so it has no lifecycle history at all.

```
                         Job Request
                              │
                    registry.try_reserve(key)
                              │
              ┌───────────────┴─ occupied ──▶ AlreadyRunning (no history)
              ▼
        [Reserved] ── validate(conn) fails ──▶ Rejected (reservation rolled
              │                                back; NO lifecycle, NO Failed Job)
       try_mark_started (DB, atomic)
              │
              ├─ false ──▶ AlreadyRunning (reservation rolled back)
              ▼
        [Accepted = Job Run]  (spawn task, attach handle to reservation)
              │
        post-acceptance setup ── Err ─┐
              │                       │
           execute ──────── panic ────┤        ┌──────────────────────────┐
              │                       ├───────▶│ Failed terminal          │
              ├─ Failed{typed} ───────┤        │ ONE TX: lifecycle cols + │
              │                       │        │ event + Failed Job row   │
        commit_success (ONE TX) ─ Err ┘        └──────────────────────────┘
              │                                        ▲
              ▼                                        │
     ┌─────────────────────┐            user cancel wins terminal gate
     │ Succeeded terminal  │            ("cancelled by user" + Failed Job,
     │ (persisted first)   │            then abort the task)
     └─────────────────────┘
              │                                        │
       spawn_dependents(key)                 drop_dependency_edges(key)
              │                                        │
              └────────────► registry.release(key) ◄───┘
                    (reservation held through terminal commit
                     and dependency handling, then removed)
```

Exactly one of {Succeeded, Failed, Cancelled} commits, arbitrated by a
per-run `TerminalGate` (an atomic claim): whichever of the run task or a
concurrent `run::cancel` call claims it first owns writing the terminal
state and releasing the `JobRegistry` slot; the other party writes nothing.

Three invariants make this safe under `tokio::spawn`/abort and a shared
`Arc<Mutex<Connection>>`:

1. **No `.await` and no async filesystem I/O between claiming the gate and
   committing the terminal transaction.** Terminal commits are synchronous
   rusqlite calls inside `unchecked_transaction()`, so a gate winner cannot
   be interrupted mid-commit — this is what makes shutdown's gate-blind
   `JobRegistry::cancel_all()` safe to abort a task that might be
   mid-success. (Success's FS-only fact-gathering, e.g. resolving the
   downloaded extension, deliberately runs *before* the gate is claimed and
   before the DB mutex is taken, so a slow working dir can't freeze other
   handlers waiting on that mutex.)
2. **DB → registry lock ordering only.** A registry lock is never held
   while acquiring the DB mutex; the run task and `run::cancel` both take
   registry locks only after releasing the DB mutex.
3. **The terminal-gate winner owns `release`.** The run task calls
   `spawn_dependents` and `registry.release` after its own commit; a
   winning `run::cancel` calls `registry.abort_and_release` after its
   commit. A losing side does nothing further — an in-flight `abort()` on
   an already-finished task is harmless.

A `JobRequest` implementor (`jobs::download::DownloadRequest`,
`jobs::split::SplitRequest`, or `jobs::archive::ArchiveRequest`) supplies
`validate` / `try_mark_started` / `setup` / `execute` / `gather_success_facts`
/ `commit_success` / `record_failure`, each mapping to one phase of the
diagram above. Archive's `execute` runs its real filesystem rename-or-copy
and symlink work (`do_archive`) on a blocking thread inside the engine
future — archive-specific behavior and safety checks stay owned by archive
execution, not by the engine.

### Recovery: stale accepted Job Runs

A Job Run's owning process can disappear without ever reaching a terminal
outcome — an unclean restart, or graceful shutdown's `JobRegistry::cancel_all`
aborting a still-running task. `jobs::run::recover_failed` converts such a
stale accepted Job Run into a Failed Job through the same one-transaction
terminal commit `run::cancel` and the run task itself use:

```text
stale *_started_at (owning process gone)
              │
    run::recover_failed(reason)
              │
   ONE TX: lifecycle failure cols +
       event + Failed Job row
```

Unlike `submit`/`cancel`, `recover_failed` takes no `JobRegistry` reservation
and claims no `TerminalGate` — it is safe without either only because of
*where* `crate::lifecycle::fail_in_progress_jobs` (its only caller) runs:

- **Startup** (`bin/concert_web.rs`): before the `JobRegistry` is constructed.
  No Job Run task exists yet, so there is nothing to race.
- **Graceful shutdown** (`bin/concert_web.rs`, after `cancel_all`): every slot
  and its `TerminalGate` have already been aborted and released, so there is
  no gate left to claim.

`fail_in_progress_jobs` holds the db mutex across its whole select-then-commit
loop, and `*_started_at` is the sole coordination signal: a row whose run
already committed success has cleared that column and is left untouched (see
`recovery_does_not_re_fail_an_already_committed_success` in `lifecycle.rs`).
The one pre-existing residual gap this does not close: an aborted archive
run's `do_archive` executes on a detached `spawn_blocking` thread that cannot
itself be cancelled, so in principle its outer future could still reach
`commit_success` after recovery's caller has released the db mutex. The window
is narrow — abort lands at the `spawn_blocking` await almost immediately,
long before a real directory move completes — and is identical to the
pre-#127 archive code's behavior, not introduced by recovery.

### Split completion and dependency intent

Split uses the engine's two preparation phases. Pre-acceptance validation
re-reads the concert and constructs typed input; a rejected request creates no
split lifecycle or Failed Job history. Post-acceptance setup creates temporary
splitter inputs, rechecks the filesystem, and removes stale interludes. Any
failure after acceptance reaches the normal failed terminal transaction.

Before a successful terminal commit, split gathers filesystem completion facts.
Executed analysis requires readable generated timestamps. Existing-track
recovery requires track presence but treats missing legacy timestamp metadata
as repairable: it preserves stored timestamp columns and logs a warning.
Required lifecycle, track-presence, timestamp, and event writes commit in the
same transaction before the Job Run is visible as successful.

A split execution failure is `JobStepFailure::Ordinary` or
`RecoverablePartialSplit { message, tracks }`. For a recoverable partial, the
failed terminal transaction validates the unique titles against the set list,
writes their exact `tracks_present` vector, appends the split-error event, and
inserts the Failed Job row together. It never sets `split_at` or starts success
dependents. Any validation or database error rolls back all of those facts.

```text
Partial outcome → win terminal gate → ONE DB transaction
                                      ├─ exact tracks_present
                                      ├─ split_started_at = NULL
                                      ├─ split_at remains NULL
                                      ├─ split_error event
                                      └─ Failed Job row
```

The automatic download-to-split edge stores only a `JobKey` identifying split
intent:

```text
Download Job Run ── queued Split JobKey ──▶ download success commits
                                                   │
                                                   ▼
                                        take intent exactly once
                                                   │
                                                   ▼
                                        re-read current concert state
                                                   │
                                        submit SplitRequest(Analyze)

download failure/cancellation ──▶ drop queued key; no Split Job Run
```

No validated concert data, timestamps, paths, or `SplitJob` are retained in the
edge. Release constructs a fresh automatic request from current state. Key
identity continues to provide deduplication, queued-status rendering, and
queued cancellation.

## Typed runner boundary

Download, split, and opener execution goes through `JobRunner`, held by
`JobConfig`. The runner returns typed outcomes (`JobStepOutcome` and
`OpenMediaOutcome`) instead of exposing process exit status to the lifecycle
code.

The production runner still builds the existing subprocess commands:

- `yt-dlp` for downloads,
- `live-set-splitter` for splits,
- the configured opener command for media files.

The split command runner also reads the CLI's structured outcome file. A
non-zero process exit plus `Partial` becomes the same typed failure as the
in-process library adapter; stderr is only a fallback for missing or malformed
reports. This keeps both adapters on the same terminal transaction path.

## Test-control runner

`concert-tracker/src/test_control/job_driver.rs`'s `TestControlJobRunner`
implements the same `JobRunner` trait with configurable per-step outcomes
(`succeed`/`fail`/`block`) instead of real subprocesses. `concert-web`
switches to it only when built with `--features test-control` *and* started
with `--test-control-port` — otherwise (including a test-control build run
without that flag) it uses `JobConfig::production` unchanged. See
[`hurl/README.md`](../hurl/README.md)'s "Job Driver" section for the Test
Control API this runner is configured through.

See
[`docs/adr/0005-typed-job-runner-for-test-control.md`](adr/0005-typed-job-runner-for-test-control.md)
for the architectural decision and
[`docs/change/2026-07-14-remaining-web-integration-hurl-migration-spec.md`](change/2026-07-14-remaining-web-integration-hurl-migration-spec.md)
for the wider Hurl migration plan.

## Scrape runner boundary

The background metadata-scrape queue is separate from `JobRunner`. It owns a
pending set and calls an injected `jobs::scrape_queue::ScrapeItemFn` for each
queued concert; download, split, and opener steps instead use typed
`JobRunner` methods and `JobRegistry` lifecycle coordination. Keeping the
boundaries separate prevents scrape-specific queue semantics from becoming a
fake download/split step.

Production uses the normal network-backed scrape item. When Test Control is
both compiled and enabled with `--test-control-port`, `concert-web` injects
`test_control::scrape_driver::ScrapeDriver`, which supports deterministic
per-concert success/block plans and observations while exercising the same
queue and pending-card behavior. The Scrape Driver's API and reset semantics
are canonical in [`hurl/README.md`](../hurl/README.md#scrape-driver).

Issue [#129](https://github.com/gregwebs/tiny-desk-splitter/issues/129)
investigated folding this queue into `JobRegistry` after the #124 Job Run
deepening and recommended against it — see
[`docs/adr/0006-scrape-queue-separate-from-job-registry.md`](adr/0006-scrape-queue-separate-from-job-registry.md)
for the full deletion-test and leverage analysis.
