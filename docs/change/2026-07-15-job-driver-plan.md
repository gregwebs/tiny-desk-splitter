# Job Driver and job-chain Hurl migration — implementation plan

Issue: [#108](https://github.com/gregwebs/tiny-desk-splitter/issues/108), slice 2
of the [remaining web integration Hurl migration](2026-07-14-remaining-web-integration-hurl-migration-spec.md)
(#106). Blocked-by #107 (typed job runner), which is merged into this branch
(`docs/remaining-web-integration-hurl-migration`).

## Goal

Add a Test Control **Job Driver** backed by the typed `JobRunner` (from #107)
so Hurl scenarios can configure deterministic download/split/opener outcomes
instead of injecting fake shell commands. Add **Job Observations** for
concurrency/dependency-edge assertions. Migrate the job-chain and
watch/opener Rust integration tests to `.hurl`.

## Architecture

### 1. `TestControlJobRunner` implements `jobs::JobRunner`

New file `concert-tracker/src/test_control/job_driver.rs` (module gated by
the existing `feature = "test-control"` on the `test_control` parent module —
no new cfg needed).

```rust
pub struct JobDriver {
    default_plan: Mutex<JobPlan>,
    concert_plans: Mutex<HashMap<i64, JobPlan>>,
    observations: Mutex<HashMap<(i64, JobStepKind), Observation>>,
    blocked: Mutex<HashMap<(i64, JobStepKind), BlockedStep>>,
}

pub struct JobPlan {
    pub download: StepOutcome,
    pub split: StepOutcome,
    pub open: StepOutcome,
}

pub enum StepOutcome { Succeed, Fail, Block }

pub enum JobStepKind { Download, Split, Open }

#[derive(Default, Clone, Copy)]
pub struct Observation {
    pub started: u32,
    pub completed: u32,
    pub failed: u32,
    pub blocked: u32,
    pub released: u32,
}
```

`JobPlan::default()` is all-`Succeed` (matches today's `JobConfig::test`
no-op-success behavior, so existing Hurl scenarios that don't touch the Job
Driver keep working unchanged).

`BlockedStep` holds a `tokio::sync::oneshot::Sender<StepOutcome>` (the
release outcome) that `run_download`/`run_split` awaits on while blocked.
Using a channel (not polling) means a blocked step consumes no CPU.

`TestControlJobRunner::run_download`/`run_split` resolve the effective plan
(concert override, else default), bump `started`, then:
- `Succeed`: create the domain-level output files (see "File ownership"
  below), bump `completed`, return `JobStepOutcome::Succeeded`.
- `Fail`: bump `failed`, return `JobStepOutcome::Failed { message: "test-control: <kind> plan=fail" }`.
- `Block`: bump `blocked`, register the oneshot sender under `(concert_id,
  kind)`, then `await` the receiver. On `Ok(outcome)` (a real release), bump
  `released` and act on `outcome` (`Succeed`/`Fail` — `Block` is not a valid
  release outcome and is rejected at the `test.job_release` boundary, before
  the sender is ever touched, with `Fail`/`OkResult` returned synchronously).
  On `Err(RecvError)` (the sender was dropped — see "Reset while blocked"
  below), resolve to `JobStepOutcome::Failed { message: "test-control: block
  cancelled (server reset while blocked)" }` — **never** `.unwrap()` the
  receiver result, since a panic here would take down the whole job task and,
  depending on where it unwinds to, potentially the process.

**Blocked-step release protocol (fixes a race identified in adversarial
review):** `job_release` looks up `(concert_id, kind)` in the `blocked` map.
If no entry exists — because the step hasn't reached its `Block` branch yet,
or was already released — `job_release` returns an error (mirrors
`assert_concert_state`'s "surface caller mistakes, don't silently no-op"
convention) rather than queuing a "pending release" for a step that hasn't
started yet. Hurl scenarios that block a step **must** poll
`test.assert_job_observation ... blocked=1` (using Hurl's `[Options] retry:
N retry-interval: Xms`) before calling `job_release`, exactly the same
poll-then-act idiom `hurl/*.hurl` already uses for async completion (e.g.
`wait_for_split` in the Rust tests being migrated). This is a deliberate
simplicity trade-off over a "pending release" queue: it keeps `job_release`
a single synchronous state check instead of adding a second kind of
in-flight state to reason about, at the cost of requiring one extra poll step
in each blocking Hurl scenario. Document this protocol prominently in
`hurl/README.md`'s Job Driver section, not just here.

**Reset while blocked:** `JobDriver::reset()` (called from `test.reset`,
see §3) clears the `blocked` map. Dropping a still-registered
`oneshot::Sender` as part of that `HashMap::clear()` causes the parked
`run_download`/`run_split` task's `.await` to resolve to `Err(RecvError)`,
which (per above) becomes a deterministic `Failed` outcome — the task
unblocks and the job fails cleanly instead of hanging forever or panicking.
This is a strictly better outcome than `reset_test_data`'s existing "in-flight
jobs are undefined behavior" carve-out for non-job-driver state, so it's
called out explicitly rather than inheriting that carve-out's ambiguity.

`open_media` needs a `concert_id` to look up its plan — see "Opener plan
scoping" below; the current `JobRunner::open_media(&self, path: &Path)`
signature from #107 cannot express this and must change.

### 1a. Opener plan scoping requires an `open_media` signature change

Adversarial review finding: `JobRunner::open_media(&self, path: &Path)` (from
#107, already merged) has no concert id, but `test.job_set_plan` with a
`concert_id` override and `test.assert_job_observation kind=open` both need
one to scope opener behavior per concert — a path alone cannot be reversed
into a concert id (and account for `watch` vs `watch_track` differences).

Fix: change the trait to `fn open_media<'a>(&'a self, concert_id: i64, path:
&'a Path) -> JobRunFuture<'a, OpenMediaOutcome>`, and `JobConfig::open_media`
to take and forward `concert_id`. `CommandJobRunner::open_media` ignores the
new parameter (the real `open`/`xdg-open` command never needed it) —
`OpenCommandFn`'s `Arc<dyn Fn(&Path) -> Command>` shape is unchanged, only
the trait method gains a parameter. Update both call sites in
`web/handlers.rs`: `watch` and `watch_track` already have `id` in scope, so
this is `state.jobs.open_media(id, &path).await` at both. This is a small,
contained change to code merged in #107, still pre-`main` on the shared
migration branch, not a divergence from that slice's accepted design — #107's
ADR describes the runner boundary, not the exact argument list.

### 2. File ownership on `Succeed`

Per the tracking-issue spec, a successful test-control step must own the
files the real job step would have created, because the *existing* lifecycle
code in `jobs/download.rs`/`jobs/split.rs` reads the filesystem right after
`JobStepOutcome::Succeeded` (`find_downloaded_file` for extension detection;
`read_analysis_timestamps` for `timestamps.json`; track files for
`tracks_present_on_disk`). This is not new lifecycle behavior — the test
runner must satisfy the same postconditions `CommandJobRunner`'s subprocesses
currently do:

- **Download succeed**: write a small sentinel file at
  `concert_dir(working_dir, job.album)/<sanitize_album(album)>.mp4` (mirrors
  `db::seeds::write_seed_media_files`'s existing sentinel-file convention —
  tiny non-media bytes, since no route under test exercises `ffprobe` on it).
- **Split succeed (`Analyze`)**: write one sentinel file per `set_list` title
  at `<sanitize_filename(title)>.m4a` — reuse `model::sanitize_filename`, the
  exact helper `write_seed_media_files` already uses for track sentinels
  (`db/seeds.rs`'s `seed_media_concert` path), not the raw title, since
  titles can contain filesystem-hostile characters — plus a `timestamps.json` in `output_dir`
  shaped like `ConcertInfo` with one `SongTimestamp` per song (deterministic
  fake start/end pairs, 90s apart — mirrors `split.rs`'s own
  `config_with_fake_analyze` test helper) so `read_analysis_timestamps`
  succeeds and auto-timestamps get stored.
- **Split succeed (`UserTimestamps`)**: write the track sentinel files, and
  when the validated timestamps leave gaps against `[0, media_duration]`,
  also write `interlude_NN.m4a` sentinels — mirrors
  `remove_stale_interlude_files`'s naming pattern
  (`interlude_\d{2}\.(mp4|m4a)`) so the coverage gate this feature exists for
  can be exercised.
- **Split succeed (`ResetToAuto`)**: write track sentinel files; do not write
  interludes (matches `SplitMode::ResetToAuto` real-splitter behavior — no
  `--emit-interludes`).
- **Open succeed**: no file effects; return `Succeeded` directly.

**`open` never blocks (follow-up review finding):** `job_set_plan`/
`job_release` reuse the generic `StepOutcomeParam` enum
(`succeed`/`fail`/`block`) for all three kinds, but `watch`/`watch_track`
call `state.jobs.open_media(...)` synchronously inline in the HTTP handler
(`web/handlers.rs`) — unlike download/split, which run in a detached spawned
task and return `200` immediately while the job continues in the background.
A blocked `open` would therefore hang the HTTP response itself, and since
Hurl executes requests strictly sequentially within a file (per
`hurl/README.md`), there is no way for a later request in the same scenario
to call `job_release` while an earlier request is still awaiting a response —
`block` for `kind=open` is structurally untestable against this Hurl
execution model, not just currently unneeded. Reject it explicitly:
`job_set_plan`/`job_release` return an error when `open` (default or
per-concert) would be set to/released as `block`, rather than silently
accepting a plan no Hurl scenario could ever unblock. `download`/`split`
keep all three outcomes.

This file-writing logic is intentionally *not* a new Scenario Seed (that's
#109's job for pre-existing fixtures) — it is the Job Driver's own job-output
responsibility, parallel to what `CommandJobRunner`'s subprocess does today.

### 3. Wiring into `AppState` / `concert_web.rs`

`concert_web.rs` currently always builds `JobConfig::production(...)`, even
under `--features test-control`. Acceptance criterion 1 requires test-control
runs to use the Job Driver. Change: when built with `feature = "test-control"`
**and** `--test-control-port` is `Some(_)`, build the `JobDriver` first and
construct `JobConfig::with_runner(workdir, Arc::new(TestControlJobRunner::new(job_driver.clone(), workdir.clone())))`
instead of `JobConfig::production(...)`; otherwise (including
`--features test-control` built but no `--test-control-port` passed) keep
`JobConfig::production(...)` unchanged — a test-control *build* with the API
turned off must behave like a normal production run.

`test_control::start` needs the same `Arc<JobDriver>` to answer the new RPC
methods. Add a `job_driver: Arc<JobDriver>` field to `TestControlServer`
(not to `AppState` — `AppState` has no other feature-gated fields today and
every non-test_control caller, including all of `web_integration.rs` and
`concert_web.rs`'s production path, constructs `AppState` directly; adding a
field there would ripple through call sites that have nothing to do with Test
Control). `TestControlServer::new(state, job_driver)` becomes the one
constructor; existing unit tests in `test_control.rs` that don't touch job
behavior pass a fresh `Arc::new(JobDriver::default())` (their `JobConfig` is
still the no-op `JobConfig::test(...)`, so the driver is present but never
exercised) instead of overloading with an `Option`.

`test.reset`'s handler (`reset_test_data`) already takes `&AppState`; add a
sibling call from the RPC method's Rust body (`TestControlApiServer::reset`)
to `self.job_driver.reset()` (clears plans back to `JobPlan::default()` and
clears observations — spec: "`test.reset` clears job-driver plans and
observations. It does not reset the fixture ID counter."), matching the
existing pattern where `TestControlServer::reset` is the seam that composes
independent reset concerns.

### 4. New Test Control API methods

**Adversarial-review correction:** the first draft of this section gave
`job_set_plan`/`job_release`/`assert_job_observation` each a single struct
parameter (`params: JobSetPlan`, etc.), copying the *seed* method shape. But
the adapter only wraps request bodies under `{"params": body}` for
`AdapterRoute::Seed` — `Reset`/`Assert` pass the body through flat
(`adapter.rs`'s `translate`: `AdapterRoute::Seed(_) => json!({ "params":
body_params }), AdapterRoute::Reset | AdapterRoute::Assert(_) =>
body_params`). A single-struct-param method dispatched with a flat body would
fail to deserialize. `assert_concert_state` avoids this today by taking
*individual* flat `RpcResult` arguments (`id`, `ignored`, `downloaded`,
`split`), not one struct. Fix: give all three new methods individual flat
arguments too, and treat the new `/test/job/{name}` route exactly like
`Assert` in the adapter — flat passthrough, no wrapping. This removes the
need for any route-specific wrapping logic in `translate()` beyond adding one
more `AdapterRoute` variant to the existing match arms.

Extends the `#[rpc(...)] trait TestControlApi` in `test_control.rs`:

```rust
#[method(name = "job_set_plan", param_kind = map)]
async fn job_set_plan(
    &self,
    concert_id: Option<i64>,       // None = set the default plan
    download: Option<StepOutcomeParam>,  // "succeed" | "fail" | "block"
    split: Option<StepOutcomeParam>,
    open: Option<StepOutcomeParam>,
) -> RpcResult<OkResult>;

#[method(name = "job_release", param_kind = map)]
async fn job_release(
    &self,
    concert_id: i64,
    kind: JobStepKindParam,        // "download" | "split" | "open"
    outcome: StepOutcomeParam,     // "succeed" | "fail" — "block" is invalid here
) -> RpcResult<OkResult>;

#[method(name = "assert_job_observation", param_kind = map)]
async fn assert_job_observation(
    &self,
    concert_id: i64,
    kind: JobStepKindParam,
    started: Option<u32>,
    completed: Option<u32>,
    failed: Option<u32>,
    blocked: Option<u32>,
    released: Option<u32>,
) -> RpcResult<OkResult>;
```

`StepOutcomeParam`/`JobStepKindParam` are `#[serde(rename_all =
"snake_case")]` string enums (`"succeed"`/`"fail"`/`"block"`,
`"download"`/`"split"`/`"open"`), matching how the rest of the Test Control
surface takes plain JSON strings for small closed sets rather than
introducing a numeric-code convention.

**Why `assert_job_observation` takes expected values rather than returning
raw counts:** mirrors `assert_concert_state`'s established shape — every
present field is checked, mismatches are collected and reported together, and
a call with every field omitted is rejected (same "no vacuous pass" rule).
This keeps one assertion idiom across the Test Control surface instead of
introducing a second "query and let Hurl jsonpath-compare" style for just
this one method.

**Adapter route:** `job_set_plan` and `job_release` are named `test.job_*`,
which doesn't match either `test.seed_*` or `test.assert_*` — the adapter's
`route_for` only recognizes `/test/reset`, `/test/seed/{name}`,
`/test/assert/{name}`. Add `AdapterRoute::Job(String)` → `/test/job/{name}` →
`test.job_{name}`, wired into `translate()`'s flat-passthrough branch
alongside `Reset`/`Assert` (not `Seed`'s wrapping branch — see the correction
above). `assert_job_observation` uses the existing `/test/assert/{name}`
route unchanged, now that its params are flat.

### 5. Migrating the Rust tests

Target file: new `hurl/job_chain.hurl` (plus opener cases, possibly a
separate `hurl/opener.hurl` — small enough it may fit in the same file;
decide during implementation based on line count, matching the existing
per-concern file split like `media_state_errors.hurl` vs
`media_info_navigation.hurl`).

Per acceptance criterion 4, each listed Rust test becomes a Hurl case using
`test.seed_scraped_concert` (or `seed_lifecycle_concert`) for setup,
`test.job_set_plan`/`test.job_release` for job behavior, the real
`POST /concerts/:id/download` / `/prepare` / `/watch` routes for the
exercised behavior, and `GET /concerts/:id/prepare-status` /
`test.assert_job_observation` for postconditions:

| Rust test | Hurl approach |
|---|---|
| `download_endpoint_spawns_job_and_returns_row` | seed scraped (no set list) → plan download=succeed → POST /download → 200 |
| `prepare_endpoint_runs_download_then_split_chain` | seed scraped w/ set list → default plan succeed → POST /prepare twice (second is no-op) → poll prepare-status until tracks_present all true |
| `download_auto_split_runs_full_chain` | seed scraped w/ set list → POST /download → poll prepare-status |
| `download_auto_split_reconciles_source_present_downloaded_at_null` | single `test.seed_media_concert` call with `album`, `set_list`, `source_file: true`, `downloaded: false` (source file written on disk, but lifecycle state stays undownloaded — reproduces the "manual copy" scenario in one seed call, confirmed against `seed_media_concert`'s actual behavior: it always writes `source_file` to disk regardless of the `downloaded` flag, since file writing and lifecycle-state seeding are independent in `db/seeds.rs`) → POST /download → split runs |
| `download_auto_split_retries_on_split_error` | seed scraped w/ set list → plan split=fail → POST /download → poll prepare-status until `split_errors` is non-empty (confirmed: `seed_media_concert`/`seed_lifecycle_concert` have no direct "split error" state seed, so drive it by causing a real failure first, matching the Rust test's own setup of calling `mark_split_failed` directly — here done through a real failed run instead) → plan split=succeed → POST /download again → poll prepare-status until split=split |
| `download_no_set_list_plain_download_no_split_queued` | seed scraped, empty set_list → POST /download → prepare-status split_queued=false |
| `download_does_not_resplit_already_split_concert` | seed via `test.seed_media_concert` with `split: true`, `track_files` covering the set list, no `source_file` → POST /download → `test.assert_job_observation` split started=0 (confirmed achievable per review: `seed_media_concert` already supports this fixture shape). The original Rust test's byte-content check ("track file must not be overwritten") is deliberately **not** ported — `started=0` is a strictly stronger assertion that no split ran at all, versus checking bytes of a file a split-that-shouldn't-happen might have touched; matches the migration standard of preferring a stronger existing assertion over incidental byte-level checks. |
| `download_double_click_does_not_drop_split_edge` | plan download=block → two POST /download → job_release download succeed → poll split complete; assert_job_observation split completed=1 |
| `download_force_starts_when_tracks_present_but_source_missing` | seed media concert with track files but no source → POST /download → poll for downloaded=true |
| `watch_uses_injected_opener_and_succeeds` | seed lifecycle downloaded=true + media source file → plan open=succeed → POST /watch → 200 |
| `watch_returns_500_when_opener_fails` | same setup → plan open=fail → POST /watch → 500 |

The `download_auto_split_retries_on_split_error` and
`download_does_not_resplit_already_split_concert` rows need confirmation
during implementation against what `seed_lifecycle_concert`/
`seed_media_concert` already support — flagged as a risk below, not a blocker
to starting.

Each migrated Rust test is deleted and replaced with a one-line breadcrumb
comment (matching the existing pattern already used throughout
`web_integration.rs` for prior slices, e.g. `// list_page_renders_seeded_concert
migrated to hurl/listing_status.hurl`).

## Verification plan

```sh
cargo check -p concert-tracker --features test-control
cargo check -p concert-tracker
cargo build --bin concert-web --features test-control
node scripts/hurl-test.js --glob 'hurl/job_chain.hurl'
just test-hurl
cargo nextest run -p concert-tracker --test web_integration
just lint
cargo build --release --bin concert-web --features test-control   # expected to FAIL (release guard)
```

## Adversarial review resolution

A Codex adversarial review of this plan (2026-07-15) returned "no-ship as
written" on the first draft, with three findings, all addressed above:

1. **Adapter parameter-shape mismatch** — the new methods were originally
   single-struct-param (seed-shaped) but routed through flat-passthrough
   (assert-shaped) adapter logic, which would have failed to deserialize at
   runtime. Fixed in §4 by switching to flat individual arguments, matching
   `assert_concert_state`'s existing precedent, and defining
   `AdapterRoute::Job` to reuse the flat-passthrough branch.
2. **`open_media` had no way to be scoped per concert**, making per-concert
   opener plans/observations/release unimplementable against the #107
   trait signature. Fixed in §1a by adding `concert_id` to
   `JobRunner::open_media` and its two call sites.
3. **Blocked-step release race and reset-panic risk** — `job_release` calls
   that arrive before a step reaches its `Block` branch, and a `test.reset`
   dropping an in-flight blocked step's sender, were both underspecified.
   Fixed in §1: `job_release` errors (doesn't silently no-op or queue) when
   no blocked entry exists yet, with the poll-first (`assert_job_observation
   blocked=1`) protocol now spelled out as a requirement for Hurl scenarios;
   a dropped sender resolves to a deterministic `Failed` outcome via
   `Err(RecvError)` handling, never a panic or an unbounded hang.

The review also confirmed the migration-table risk rows were resolvable
without architecture changes (§5, updated with concrete approaches) and that
the overall `JobRunner`-based direction for download/split is sound.

## Follow-up (non-adversarial) review resolution

A second, non-adversarial Codex review of the revised plan (2026-07-15)
returned "ready with minor caveats": all three original findings confirmed
resolved against the actual source, plus three smaller issues, all addressed
above:

1. The `download_auto_split_reconciles_source_present_downloaded_at_null` row
   was ambiguous about whether it needed one or two seed calls. Confirmed
   `seed_media_concert` writes the on-disk `source_file` independently of the
   `downloaded` lifecycle flag, so a single call suffices — fixed in §5.
2. `open` inherited `block` from the shared `StepOutcome` enum, but a blocked
   `open` is structurally untestable through Hurl's sequential execution
   model (the `/watch` handler awaits `open_media` inline, unlike
   download/split's detached spawn). Fixed in §2: `open` is explicitly
   restricted to `succeed`/`fail`.
3. Split-track sentinel filenames must go through `model::sanitize_filename`
   (the same helper `db::seeds::write_seed_media_files` already uses), not
   raw set-list titles. Fixed in §2.

Ready to implement.

## Remaining risk (accepted, not blocking)

**Cross-file/cross-case Hurl execution ordering.** `job_set_plan` with no
`concert_id` sets the *default* plan for every concert not otherwise
overridden, and `just test-hurl` runs every `.hurl` file against one shared
process (per `hurl/README.md`). A scenario that changes the default plan
(e.g. default `open=fail` for a "watch fails" case) must restore it (default
`open=succeed`) before the file ends, or set per-concert overrides instead of
touching the default, so later files/cases aren't affected. Document this
requirement in `hurl/README.md`'s Job Driver section rather than relying on
authors to infer it from `test.reset`'s existing "don't call reset mid-run"
guidance, which is adjacent but not the same rule.
