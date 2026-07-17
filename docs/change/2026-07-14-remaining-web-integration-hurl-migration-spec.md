# Remaining web integration Hurl migration spec

## Problem Statement

`concert-tracker/tests/web_integration.rs` still contains tests that exercise
black-box product HTTP behavior but cannot be moved to Hurl with the current
Test Control surface. The remaining blockers are deterministic job outcomes,
scrape-worker timing, coordinated database/file fixtures, opener behavior, and
test-only assertions for job observations and events.

The goal is not to delete every Rust test from `web_integration.rs`. Router
internals, in-process consistency checks, and one deliberately real
auto-scrape failure regression should stay Rust-only. The Hurl migration
should cover product HTTP behavior that a real `concert-web` route exposes,
while using Test Control only to arrange fixtures and inspect test-only
postconditions.

## Scope Boundary

Migrate remaining black-box product HTTP behavior to Hurl. Keep these
Rust-only:

- `served_openapi_spec_matches_built_api_doc`: compares the served OpenAPI JSON
  to `web::built_api_doc()` in-process.
- `prod_router_serves_embedded_js_without_livereload`: pins router/static asset
  construction rather than user workflow behavior.
- `detail_page_auto_scrape_failure_still_renders`: keeps the real failing
  outbound scrape path, proxy disabling, and retryable `metadata_scraped_at`
  behavior in Rust for determinism.

Earlier Hurl migration standards still apply: preserve user-visible behavior,
stable HTTP contracts, htmx response headers, public JSON shapes, and necessary
test-only postconditions. Do not preserve incidental byte-for-byte markup
assertions when Playwright, Foldkit, or a stronger public assertion covers the
behavior.

## Solution Overview

Introduce a typed job-runner abstraction used by both production and
test-control builds. Production runners keep spawning the current subprocess
commands. Test-control runners complete, fail, or block domain-level download,
split, and opener steps deterministically.

Add Test Control methods for:

- job-driver plans and releases,
- job observations,
- scenario seeds that coordinate database and scratch-workdir state,
- scrape-driver timing for background scrape card behavior,
- event assertions needed by interlude deletion.

Then migrate the remaining product HTTP tests to `.hurl` files in slices.

## Typed Job Runner

Replace direct `download_cmd`, `split_cmd`, and `open_cmd` execution in job
flows with a typed runner interface. The production implementation wraps the
existing command construction and `run_with_logging`; the test-control
implementation is driven by Job Driver plans.

The production and test-control paths must share the same job lifecycle
orchestration: start guards, dependency edges, success/failure marking,
timestamp persistence, `tracks_present` refresh, interlude cleanup, and
dependent job spawning should remain common behavior.

The accepted architecture decision is recorded in
[`docs/adr/0005-typed-job-runner-for-test-control.md`](../adr/0005-typed-job-runner-for-test-control.md).

## Job Driver API

The Job Driver API configures domain-level behavior, not shell commands.

Plans should support a global default plus per-concert overrides:

```text
default plan:
  download = succeed
  split    = succeed
  open     = succeed

per-concert override:
  concert_id = 42
  download = block
  split    = succeed
  open     = fail
```

`test.reset` clears job-driver plans and observations. It does not reset the
fixture ID counter.

Actions:

- set default plan
- set plan for a concert
- release a blocked concert/job-kind
- clear plans/observations, normally as part of `test.reset`

Outcomes:

- `succeed`: complete the job and create normal output files for that job step
- `fail`: mark the job failed with a deterministic error
- `block`: record that the step started, keep it running until released, then
  complete according to the release outcome

Successful job completion owns the files created by the route-triggered job:

- download success creates the source file
- split success creates track files
- user-timestamps split success creates interlude files when timestamp gaps
  require them
- reset-to-auto split success removes stale interlude files, matching current
  split cleanup behavior

Scenario Seeds own pre-existing fixture files needed before the product route
is called.

## Job Observation API

Some regressions are about concurrency and dependency edges that are not fully
observable from public final state. Add focused observations through Test
Control:

```text
concert 42 download:
  started = 1
  completed = 1
  failed = 0
  blocked = 0
  released = 0

concert 42 split:
  started = 1
  completed = 1
  failed = 0
```

Use observations for cases such as:

- second click did not start a duplicate download
- queued split edge survived while download was blocked/running
- already-split download did not run split again
- a blocked download is actually running before release

Prefer public HTTP assertions for user-visible state when those are sufficient.

## Scenario Seeds

Seeds may create coordinated database and scratch-workdir state. Prefer named
Scenario Seeds over a generic remote filesystem mutation API.

Needed scenario shapes:

- downloaded concert with source file
- downloaded concert with preview image
- split concert with selected track files
- split concert with source file still present
- split concert with interlude file and timestamp gap
- legacy split concert with `timestamps.json`
- timestamp-editable concert with real source media for `ffprobe`
- track-details fixture with `album = NULL`

Sentinel files are enough for routes that check existence, extension, or
playback JSON shape. Real tiny playable media should be generated only for
routes whose public behavior depends on `ffprobe`, notably user timestamp POST
validation.

## Scrape Driver API

Scrape control stays separate from the Job Driver API. The scrape queue is not a
download/split/open job; it has its own pending set and injected per-item
function.

Add a small Scrape Driver for the pending-card case:

- configure a scrape item for a concert to block
- enqueue or arrange the scrape through Test Control
- release the scrape with deterministic metadata and thumbnail creation
- observe public `/concerts/:id/status` before and after release

Keep the real auto-scrape failure regression Rust-only.

## Event Assertion API

Add `test.assert_concert_events` for internal event facts that have no public
HTTP surface. The first consumer is interlude deletion:

- assert an `interlude_delete` event exists
- assert no `track_delete` event was recorded for that deletion

This keeps event checks out of production response bodies.

## State Diagrams

### Job Driver Completion

```text
Product HTTP route
  |
  |-- normal route validation fails --------> public error response
  |
  `-- normal route validation passes
        |
        `-- start job through shared lifecycle code
              |
              |-- plan = block -------------> job remains running
              |                                observations.blocked += 1
              |                                |
              |                                `-- release
              |                                      |
              |                                      `-- complete/fail by release outcome
              |
              |-- plan = fail --------------> mark lifecycle failure
              |                                observations.failed += 1
              |
              `-- plan = succeed -----------> create job output files
                                               run shared success completion
                                               observations.completed += 1
```

### Download To Split Chain

```text
POST /concerts/:id/download or /prepare
  |
  |-- no set list --------------------------> download only
  |
  `-- set list present
        |
        |-- download already running --------> keep one running download
        |                                      ensure split dependent is queued
        |
        `-- start download
              |
              |-- download fails -----------> drop queued split
              |
              `-- download succeeds --------> create source file
                                                mark downloaded
                                                spawn queued split
                                                     |
                                                     `-- split succeeds
                                                           create track files
                                                           mark split
                                                           refresh tracks_present
```

### Scrape Pending Card

```text
Seed listing
  |
  `-- enqueue blocked scrape
        |
        `-- GET /concerts/:id/status
              |
              `-- pending set contains id -> loading thumbnail + polling

Release scrape
  |
  `-- scrape driver writes metadata/thumbnail and clears pending
        |
        `-- GET /concerts/:id/status
              |
              `-- thumbnail visible + polling removed
```

### Scenario Seed Vs Job Completion

```text
Scenario Seed
  |
  `-- arrange preconditions:
        DB rows, source files, track files, preview image, timestamps.json

Product HTTP Route
  |
  `-- starts job
        |
        `-- Job Driver Completion creates postconditions:
              downloaded source, split tracks, interludes, lifecycle state
```

## Delivery Slices

### Slice 1: Typed job runner

- Add typed job-runner abstractions for download, split, and open.
- Move production command execution behind the production runner.
- Keep production behavior unchanged.
- Keep existing Rust tests passing.
- Update job docs and keep ADR 0005 linked.

Verification:

```sh
cargo check -p concert-tracker
cargo check -p concert-tracker --features test-control
cargo nextest run -p concert-tracker --test web_integration
```

### Slice 2: Job Driver and observations

- Add test-control job runner.
- Add Job Driver API for default/per-concert plans and blocked-step release.
- Add Job Observation API.
- Use the test-control runner when `concert-web` starts with the test-control
  feature and Test Control API enabled.
- Migrate job-chain Hurl cases:
  - `download_endpoint_spawns_job_and_returns_row`
  - `prepare_endpoint_runs_download_then_split_chain`
  - `download_auto_split_runs_full_chain`
  - `download_auto_split_reconciles_source_present_downloaded_at_null`
  - `download_auto_split_retries_on_split_error`
  - `download_no_set_list_plain_download_no_split_queued`
  - `download_does_not_resplit_already_split_concert`
  - `download_double_click_does_not_drop_split_edge`
  - `download_force_starts_when_tracks_present_but_source_missing`
  - opener success/failure watch cases

### Slice 3: Scenario Seeds for file-heavy routes

- Add focused Scenario Seeds for coordinated DB/workdir fixtures.
- Add `test.assert_concert_events`.
- Migrate file-heavy public HTTP behavior:
  - delete-download file removal and htmx fragment/header behavior
  - prior split-error delete-download action set
  - play button after successful split
  - ignore deletes preview image
  - prepare-status filesystem track state
  - track details without album
  - track details busy state
  - split timestamp lazy backfill from `timestamps.json`
  - split timestamp count mismatch with source file present
  - split timestamp happy path and reset happy path
  - concert playback source/reconstruction/interlude modes
  - interlude delete file/event/fragment behavior

### Slice 4: Scrape Driver

- Add Scrape Driver API using the existing injectable scrape item shape.
- Migrate `pending_card_shows_loading_then_thumbnail`.
- Keep `detail_page_auto_scrape_failure_still_renders` Rust-only.

### Slice 5: Sweep and documentation

- Remove migrated Rust tests, leaving concise breadcrumbs to the Hurl files.
- Update `hurl/README.md`:
  - new Test Control APIs
  - remaining Rust-only exceptions
  - when to use Scenario Seeds, Job Observations, and event assertions
- Add a migration change note once implementation lands.
- Run adversarial Agent Review before verification, and follow-up review after
  verification changes.

## Verification Plan

Run focused checks after each slice and the full suite before PR:

```sh
cargo check -p concert-tracker --features test-control
cargo check -p concert-tracker
cargo build --bin concert-web --features test-control
just test-hurl
cargo nextest run -p concert-tracker --test web_integration
just lint
```

For Hurl files added in a slice, also run:

```sh
node scripts/hurl-test.js --glob 'hurl/<new-file>.hurl'
```

Keep the release-safety guard:

```sh
cargo build --release --bin concert-web --features test-control
```

That command should fail specifically because `test-control` must not compile
into release builds.
