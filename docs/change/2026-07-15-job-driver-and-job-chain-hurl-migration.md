# Job Driver and job-chain Hurl migration

Implemented slice 2 of the remaining web-integration Hurl migration
([#108](https://github.com/gregwebs/tiny-desk-splitter/issues/108)): a
deterministic Test Control **Job Driver** backed by the typed `JobRunner`
from slice 1 (#107), plus migration of the download/prepare/split chain and
watch-opener Rust integration tests to Hurl.

## What changed

- `concert-tracker/src/test_control/job_driver.rs` (new): `JobDriver` holds
  default/per-concert download/split/open plans (`succeed`/`fail`/`block`)
  and started/completed/failed/blocked/released observation counts.
  `TestControlJobRunner` implements `jobs::JobRunner` against it — a
  `succeed` outcome writes the same sentinel files (source video, track
  files, `timestamps.json`, interludes) the real subprocess would have
  produced, since the existing lifecycle code in `jobs/download.rs`/
  `jobs/split.rs` reads the filesystem immediately after success.
- Three new Test Control RPC methods: `test.job_set_plan`,
  `test.job_release`, `test.assert_job_observation`, reached via a new
  `/test/job/{name}` adapter route (flat params, same passthrough as
  `/test/assert/{name}`) plus the existing `/test/assert/{name}` route.
  `test.reset` now also resets the Job Driver's plans/observations.
- `JobRunner::open_media` gained a `concert_id` parameter (a small signature
  change to the #107 trait, still pre-`main` on this migration branch) so
  opener plans/observations can be scoped per concert; `open` never accepts
  `block` — `watch`/`watch_track` await it synchronously inline in the HTTP
  handler, so a blocked open could never be released within Hurl's
  sequential request execution.
- `concert_web.rs` uses the Job Driver-backed runner only when built with
  `--features test-control` *and* started with `--test-control-port`;
  otherwise (including a test-control build run without that flag) it's
  unchanged production behavior.
- `hurl/job_chain.hurl` (new, 43 requests): migrates
  `download_endpoint_spawns_job_and_returns_row`,
  `prepare_endpoint_runs_download_then_split_chain`,
  `download_auto_split_runs_full_chain`,
  `download_auto_split_reconciles_source_present_downloaded_at_null`,
  `download_auto_split_retries_on_split_error`,
  `download_no_set_list_plain_download_no_split_queued`,
  `download_does_not_resplit_already_split_concert`,
  `download_double_click_does_not_drop_split_edge`,
  `download_force_starts_when_tracks_present_but_source_missing`,
  `watch_uses_injected_opener_and_succeeds`, and
  `watch_returns_500_when_opener_fails` out of
  `concert-tracker/tests/web_integration.rs` (30 tests -> 19), replaced with
  breadcrumb comments.
- `docs/jobs.md` and `hurl/README.md` document the test-control runner and
  the new Job Driver Test Control API, including the poll-then-release
  protocol for blocked steps (`job_release` errors if the step hasn't
  registered as blocked yet — poll `assert_job_observation` for `blocked=1`
  first) and the "prefer per-concert overrides over the shared default plan"
  guidance for a shared-process test suite.

See
[`docs/change/2026-07-15-job-driver-plan.md`](2026-07-15-job-driver-plan.md)
for the full design, including two rounds of Codex adversarial/follow-up
review and the issues they found and resolved before implementation started.

## Verification performed

- `cargo check -p concert-tracker`
- `cargo check -p concert-tracker --features test-control`
- `cargo test -p concert-tracker --features test-control test_control::` (68
  passed, including 20 new `job_driver` unit tests)
- `cargo build --bin concert-web --features test-control`
- `node scripts/hurl-test.js --glob 'hurl/job_chain.hurl'` (43/43)
- `just test-hurl` (164/164 across all 8 files)
- `cargo nextest run -p concert-tracker --test web_integration` (19/19)
