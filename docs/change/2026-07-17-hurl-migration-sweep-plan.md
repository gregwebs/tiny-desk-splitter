# Hurl migration sweep — implementation plan

Issue: [#111](https://github.com/gregwebs/tiny-desk-splitter/issues/111)  
Parent spec: [remaining web integration Hurl migration](2026-07-14-remaining-web-integration-hurl-migration-spec.md)

## Goal

Finish the remaining-web-integration migration by making the lasting Hurl
documentation canonical, reducing the Rust integration test file to the three
intentional in-process exceptions plus concise migration breadcrumbs, recording
the completed migration, and verifying every production and test-control build
boundary named by the specification.

This ticket changes documentation and obsolete test-file comments only. It does
not change product behavior, Test Control contracts, or the three remaining
Rust tests.

## Documentation state change

```text
Before
  hurl/README.md                  mostly complete Test Control reference
  web_integration.rs             3 Rust tests + many historical breadcrumbs
  per-slice change records       implementation history, no final summary
       |
       v
After
  hurl/README.md                  canonical current API + exception guide
  web_integration.rs             3 Rust tests + concise canonical pointer
  final migration change record  completion summary + verification evidence
```

No application state changes in this ticket.

## Required changes

- [x] Audit `hurl/README.md` contract-by-contract against these sources and a
  representative black-box consumer:
  - Test Control RPC declarations and event assertions:
    `concert-tracker/src/test_control.rs`, its adapter routes in
    `concert-tracker/src/test_control/adapter.rs`, and
    `hurl/test_control_adapter.hurl` / `hurl/concert_playback.hurl`.
  - Job Driver plans, observations, release/reset, and activation:
    `concert-tracker/src/test_control/job_driver.rs`,
    `concert-tracker/src/bin/concert_web.rs`, and `hurl/job_chain.hurl`.
  - Scenario Seed parameters and file ownership:
    `concert-tracker/src/db/seeds.rs` and the media/split/playback Hurl files.
  - Scrape Driver plans, observations, release/reset, and activation:
    `concert-tracker/src/test_control/scrape_driver.rs`,
    `concert-tracker/src/bin/concert_web.rs`, and `hurl/scrape_pending.hurl`.
- [x] Keep current contracts, usage constraints, scenario locations, and the
  three Rust-only exceptions canonical in `hurl/README.md`. Replace its long
  chronological migration history with a compact current coverage map, remove
  duplicated Known Gaps material, and point architectural claims to lasting
  `docs/jobs.md` / ADR 0005 rather than an ephemeral slice plan.
- [x] Keep the three intentional Rust-only exceptions explicit and accurate:
  real auto-scrape failure, production embedded-JavaScript router wiring, and
  served-vs-built OpenAPI consistency.
- [x] In `concert-tracker/tests/web_integration.rs`, replace all scattered
  `// <test> migrated to ...` blocks with one header comment after the imports:
  `// Black-box product HTTP coverage lives in hurl/*.hurl; see hurl/README.md.`
  Preserve and recheck the detailed doc comments explaining why each of the
  three remaining tests is Rust-only.
- [x] Add `docs/change/2026-07-17-hurl-migration-sweep.md` with sections for
  scope and moved scenarios, the final Rust-only boundary, canonical
  documentation, a state/coverage diagram, Agent Review, and command-by-command
  verification evidence.
- [x] Add a direct Hurl/Test Control guide link from `README.md`, and remove the
  duplicated `testing.` line in that documentation list while editing it.
- [x] Validate every touched relative Markdown link resolves using an explicit
  repository-local link enumeration/check, then use `rg` to check that current
  Test Control explanations are canonical rather than duplicated across
  lasting documents. Cross-check `README.md`, `hurl/README.md`, `docs/jobs.md`,
  ADR 0005, the parent spec, and the final Change Record.

## Test seams and verification

The issue and parent spec pre-agree these public/build seams. No new tests are
needed because no behavior changes; verification exercises the real binary and
existing tests rather than documentation-specific implementation details.

- [x] `cargo check -p concert-tracker --features test-control`
- [x] `cargo check -p concert-tracker`
- [x] `cargo build --bin concert-web --features test-control`
- [x] `just test-hurl`
- [x] `cargo nextest run -p concert-tracker --test web_integration`
- [x] `just lint`
- [x] Confirm `cargo build --release --bin concert-web --features test-control`
  exits nonzero and capture stderr to prove it contains the exact
  `compile_error!` diagnostic from `concert-tracker/src/test_control.rs`, not an
  unrelated build failure.
- [x] Treat the full Hurl suite as live manual-equivalent verification for this
  documentation-only ticket: it starts a real `concert-web` on isolated
  scratch DB/workdir/ports and drives the public HTTP routes. Inspect its server
  startup, scenario count, and teardown output plus the three-test Rust result.
  Playwright is not ticket-specific because no visual or interaction behavior
  changes; record that decision in the final Change Record.

## Review sequence

- [x] Adversarial engineering-lead review of this plan; apply findings before
  editing the sweep artifacts.
- [x] Adversarial engineering-lead review of the completed diff.
- [x] Run verification.
- [x] If verification causes changes, request a non-adversarial follow-up
  review and rerun affected checks.
- [ ] Commit, push, open a PR against
  `docs/remaining-web-integration-hurl-migration`, and monitor CI.
