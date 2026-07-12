# Wire Hurl Web Integration Tests Into CI

## Problem

The first Hurl slice (`docs/change/2026-07-11-hurl-web-integration-tests.md`,
parent issue #84) built the Hurl workflow and migrated a first batch of
`web_integration.rs` tests to `hurl/listing_status.hurl`, but deliberately
left `just test-hurl` out of CI as an explicit, separate decision. That meant
the behaviors already migrated — a listing appearing on `GET /`, the ignore
endpoint's badge markup, the ignored filter, the scraped-status fragment —
had no CI coverage at all: only `hurl/listing_status.hurl`, which a human or
agent had to remember to run locally.

This is the first PR of a second migration slice (tracked under #84) that
also migrates more of `web_integration.rs`'s "in-scope but not yet migrated"
bucket to Hurl. Wiring CI first, before any further Rust test is deleted, was
a deliberate sequencing decision — deleting Rust duplicates without CI
coverage of their Hurl replacements would silently drop that coverage.

## Change

`.github/workflows/ci.yml`'s `rust` job now runs `just test-hurl` as a
**blocking** step, right after `cargo nextest run --tests`:

- Installs `hurl` (pinned `HURL_VERSION=8.0.1`) from the official `.deb`
  release artifact — the method Hurl's own docs recommend for Debian/Ubuntu
  CI — and `just` via the `taiki-e/install-action` step already used for
  `nextest`.
- Installs Node 22 via `actions/setup-node` (the `rust` job previously had
  neither `just` nor Node available; `scripts/hurl-test.js`, which
  `just test-hurl` calls, is a dependency-free Node script).
- Runs `just test-hurl`, which builds `concert-web --features test-control`
  and runs `hurl/*.hurl` against it — identical to what a contributor runs
  locally.

A second new blocking step, **"Verify test-control cannot build in release
mode,"** guards the release-safety invariant independently of the Hurl suite:
it runs `cargo build --release --bin concert-web --features test-control` and
asserts the build *fails*, then asserts the failure is specifically the
`compile_error!` in `concert-tracker/src/test_control.rs`
(`"test-control must not be compiled into release builds"`) rather than some
unrelated build break that happens to also fail and would mask a missing
guard. This exists because `just test-hurl` only ever builds in debug mode
and would not notice if the release guard were ever removed or bypassed.

The existing "Verify working tree is clean" step was moved to run after these
new steps so it also covers them (building `concert-web --features
test-control` writes only to the gitignored `target/` directory and an
out-of-repo scratch dir under `os.tmpdir()`, so no drift is expected, but the
check is cheap and now covers the full job).

`hurl/README.md`'s "Known gaps" section is updated: the "not wired into CI"
gap is removed and replaced with a "CI" section describing both new steps.

## Why this shape

- **`.deb` over `cargo-binstall`/other install methods**: Hurl is not on
  `taiki-e/install-action`'s curated tool list, and CI reproducibility matters
  more here than saving one extra step — the `.deb` method is Hurl's own
  documented recommendation for this exact environment (Ubuntu/Debian CI) and
  pins an exact version.
- **Blocking, not advisory**: an advisory/non-blocking Hurl step would not
  close the CI-coverage gap this PR exists to close — a red Hurl run has to
  fail the PR for that migrated coverage to mean anything.
- **Release guard as its own step, not folded into `just test-hurl`**: the
  release-build invariant is safety-critical (an unauthenticated test-only
  control surface must never ship) and is orthogonal to whether the Hurl
  suite itself passes; keeping it a separate, explicitly-named CI step makes
  a future regression there fail with an unambiguous message instead of
  getting lost inside a Hurl step's output.

## Verification

- `just test-hurl` — ran locally, passed (`hurl/listing_status.hurl`, 8
  requests, 100% success).
- Simulated the release-guard step locally: `cargo build --release --bin
  concert-web --features test-control` fails, and its stderr contains
  `"test-control must not be compiled into release builds"`.
- CI run on the PR itself is the first real test of both new steps end-to-end
  (Ubuntu runner, fresh install of `hurl`/`just`/Node) — see the linked PR for
  the run.

## Next

The rest of this slice (a follow-up PR, tracked as a separate sub-issue under
#84) migrates more of `web_integration.rs`'s state-only public-HTTP tests to
Hurl, adding a new state-only lifecycle seed method to the Test Control API.
See the slice-2 issue for the audit table of which tests migrate and why.
