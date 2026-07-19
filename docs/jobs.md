# Job execution

`concert-tracker` runs download, split, archive, and opener work through the
job modules under `concert-tracker/src/jobs/`.

Download and split routes share the same lifecycle orchestration:

- the route or workflow validates the concert state,
- `JobRegistry` prevents duplicate running jobs,
- dependency edges queue follow-up jobs such as download then split,
- lifecycle persistence records started, succeeded, or failed state,
- successful split completion refreshes track availability and timestamp state,
- failed jobs record user-visible errors and job logs.

## Job Run orchestration

Download and split use the `jobs::run` engine (issues
[#125](https://github.com/gregwebs/tiny-desk-splitter/issues/125) and
[#126](https://github.com/gregwebs/tiny-desk-splitter/issues/126), part of the
deepening tracked by [#124](https://github.com/gregwebs/tiny-desk-splitter/issues/124)).
Archive keeps its hand-rolled admission/lifecycle code until #127; #128 then
contracts the legacy protocol once all three share the engine. See
`CONTEXT.md` for the **Job Request** / **Job Run** / **Failed Job** domain
vocabulary this section assumes.

A **Job Request** (`jobs::run::JobRequest`) becomes a **Job Run** only after
acceptance. Every accepted Job Run reaches exactly one terminal outcome —
succeeded, failed, or cancelled — and every unsuccessful terminal outcome
creates **Failed Job** history:

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
              ├─ Failed{msg} ─────────┤        │ ONE TX: lifecycle cols + │
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

A `JobRequest` implementor (`jobs::download::DownloadRequest` or
`jobs::split::SplitRequest`)
supplies `validate` / `try_mark_started` / `setup` / `execute` /
`gather_success_facts` / `commit_success` / `record_failure`, each mapping to
one phase of the diagram above; `jobs::run::submit` and `jobs::run::cancel`
are the only two engine entry points archive will adopt when it migrates.

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

The command runner converts subprocess success, non-zero exit, and spawn
failure into typed outcomes before the lifecycle code handles success or
failure. This keeps production behavior unchanged.

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
