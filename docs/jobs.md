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
Control API this runner is configured through, and
[`docs/change/2026-07-15-job-driver-plan.md`](change/2026-07-15-job-driver-plan.md)
for the design.

See
[`docs/adr/0005-typed-job-runner-for-test-control.md`](adr/0005-typed-job-runner-for-test-control.md)
for the architectural decision and
[`docs/change/2026-07-14-remaining-web-integration-hurl-migration-spec.md`](change/2026-07-14-remaining-web-integration-hurl-migration-spec.md)
for the wider Hurl migration plan.
