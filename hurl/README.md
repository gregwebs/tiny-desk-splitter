# Hurl web integration tests

Black-box HTTP tests against a real `concert-web` process, as opposed to
`concert-tracker/tests/web_integration.rs`, which links directly into the Rust
web implementation and calls the axum router in-process via
`tower::ServiceExt::oneshot`. See
[`docs/change/2026-07-11-hurl-web-integration-tests.md`](../docs/change/2026-07-11-hurl-web-integration-tests.md)
for the full design (architecture, decisions, Test Control API contracts) and
[`docs/adr/0001-jsonrpsee-for-test-control-api.md`](../docs/adr/0001-jsonrpsee-for-test-control-api.md)
for why the Test Control API is JSON-RPC.

This is optional local tooling, not yet part of CI (see "Known gaps" below).

## Setup

Install the [Hurl CLI](https://hurl.dev/docs/installation.html), e.g.:

```sh
brew install hurl   # macOS
```

Nothing else is required — `just test-hurl` builds the test-control
`concert-web` binary itself.

## Running

```sh
just test-hurl
```

This checks `hurl` is on `PATH`, then runs `scripts/hurl-test.js`, which:

1. Builds `concert-web` with `--features test-control`.
2. Starts it against a scratch DB/workdir in a fresh temp directory, with
   `--port 0 --test-control-port 0` (both ephemeral).
3. Parses the `Listening on ...` and `Test control listening on ...` lines
   from its stdout to learn both bound ports.
4. Runs `hurl --test --jobs 1 --glob 'hurl/*.hurl'` against them, passing
   `{{base_url}}` and `{{test_control_url}}` as variables.
5. Tears the server down and removes the scratch directory — including on
   `Ctrl-C` or a startup failure, not just a clean exit.

`--jobs 1` is required, not cosmetic: every `.hurl` file in this directory
shares the *same* `concert-web` process, DB, and workdir for the whole
`just test-hurl` invocation (one server is started, then every file runs
against it). `hurl --test` parallelizes across files by default, which would
let two files race each other's seed/assert calls against that shared state.

To run against a subset of files: `node scripts/hurl-test.js --glob 'hurl/some_file.hurl'`.

Each invocation gets its own fresh scratch DB, so files don't need to call
`test.reset` for isolation *between separate `just test-hurl` runs* — but
scenarios *within* one file, or across files in the same run, do share state
and should pick distinguishing `source_url`s (see `hurl/listing_status.hurl`)
rather than relying on `test.reset` between every scenario.

## Three ways to check something, and when to use each

The spec's "Decisions" section covers this in depth; the short version:

1. **Public HTTP assertions** (`GET`/`POST` against `{{base_url}}`, checking
   status/body) — the default. Use these whenever the behavior is actually
   visible through a route or fragment a browser would see. Every case in
   `hurl/listing_status.hurl` today uses only this.
2. **Seed Results** (the JSON-RPC `result` of a `test.<snake_case>` seed
   call, e.g. `test.seed_listing`) — for arranging fixture data and capturing
   a stable handle (an id, or public fields) to use in later steps. Never
   read a raw DB row or assume an id — capture it from the Seed Result with
   `[Captures]`, as every case in `hurl/listing_status.hurl` does.
3. **Assertion API methods** (`test.assert_concert_state` today) — only when
   a postcondition is internal-only and no public route exposes it. No case
   in `hurl/listing_status.hurl` currently needs this — every postcondition
   the first slice checks (a listing appears, the ignored badge/filter, the
   scraped-status fragment) is already public. The method exists so a future
   slice touching download/split/archive state doesn't have to invent new
   Test Control surface mid-slice; see its doc comment in
   `concert-tracker/src/test_control.rs`.

## Verification commands for future agents

```sh
cargo check -p concert-tracker --features test-control
cargo check -p concert-tracker
cargo build --bin concert-web --features test-control
just test-hurl
cargo nextest run -p concert-tracker --test web_integration
just lint
```

Release-guard check (expected to **fail to compile**, confirming the
`test-control` feature can't leak into a release build):

```sh
cargo build --release --bin concert-web --features test-control
```

## Why the remaining `web_integration.rs` tests are still Rust-only

As of this first slice, `concert-tracker/tests/web_integration.rs` still has
~60 tests. None were left out by oversight — each falls into one of these
categories, matching the spec's "Out Of Scope For First Slice" list plus a
couple of pre-existing areas this slice never touched:

- **Job command stubbing for download/split chains** (spec: explicitly out of
  scope). The largest group: everything that injects a fake download/split
  command via `state_with_chain`/`JobConfig` to control success, failure, or
  retry behavior deterministically and instantly, without a real subprocess —
  e.g. `download_endpoint_spawns_job_and_returns_row`,
  `prepare_endpoint_runs_download_then_split_chain`,
  `download_auto_split_retries_on_split_error`,
  `download_double_click_does_not_drop_split_edge`, the `delete_download_*`
  and `delete_split_*` family. The Test Control API has no equivalent for
  "run a job with an injected outcome" yet — that needs its own design (a
  later migration slice), not a bolt-on to the first slice's seed methods.
- **Scrape queue timing** (spec: explicitly out of scope).
  `pending_card_shows_loading_then_thumbnail` injects a stub scrape item and
  release/done channels to deterministically observe the pending → thumbnail
  transition mid-flight. No Test Control equivalent exists for controlling
  worker timing.
- **Opener command injection** (spec: explicitly out of scope).
  `watch_uses_injected_opener_and_succeeds`,
  `watch_returns_500_when_opener_fails` inject a success/failure closure for
  the "open in system player" command.
- **Filesystem/media-fixture-heavy tests** (spec: "broad filesystem assertion
  helpers beyond what the first suite needs" is out of scope). Track
  navigation, media-info, watch/like, playback-reconstruction, and
  split-timestamps tests generate real tiny playable media with `ffmpeg`
  (`create_test_audio`) and assert on exact filesystem/track state. Seeding
  that through Test Control would need file-producing seed methods this slice
  doesn't have a concrete consumer for yet.
- **Playlist API and HTML pages** (not part of the "listing and status
  basics" first slice). `playlist_api_crud_and_resolution`,
  `playlist_api_validation_status_codes`, `playlists_html_pages_render`, and
  related tests cover a distinct feature area with its own JSON API — a
  reasonable target for a *future* migration slice, not this one.
- **Router/build-internals checks that aren't user-facing HTTP behavior**.
  `prod_router_serves_embedded_js_without_livereload` distinguishes dev vs.
  prod router wiring (an internal construction detail, not something a
  black-box HTTP client observes differently). `served_openapi_spec_matches_built_api_doc`
  compares two in-process Rust values (the built OpenAPI doc vs. what's
  served) — a pure Rust-internal consistency check with no black-box
  equivalent.
- **In-scope but not yet migrated**: `available_concert_row_shows_want_and_ignore_buttons`
  checks a pure public-HTTP fragment (no job stubbing) similar in spirit to
  what's already migrated, but wasn't in the first slice's five-bullet
  "Initial Hurl coverage" list from the spec. It's a reasonable candidate for
  the *next* slice, not a defect in this one — the spec's own decision is to
  migrate in slices and delete Rust duplicates as each Hurl equivalent lands,
  not to migrate everything reachable in the first pass.

## Known gaps

- **Not wired into CI.** `.github/workflows/ci.yml`'s `rust` job runs
  `cargo nextest run --tests` only — `just test-hurl` is not part of any
  workflow. This matches the spec's explicit "CI enforcement of Hurl tests"
  being Out Of Scope For First Slice, but it means the behaviors this slice
  migrated (a listing appearing on `GET /`, the ignore endpoint's badge
  markup, the ignored filter, the scraped-status fragment) are currently
  **not covered by anything CI runs** — only by `hurl/listing_status.hurl`,
  which a human or agent has to remember to run locally. Wiring `just
  test-hurl` into CI is the natural next step once this workflow has proven
  itself, but is a deliberate, explicit decision to make separately (it
  changes what blocks a PR), not something to add silently.
- **No Assertion API consumer yet.** `test.assert_concert_state` exists (see
  above) but nothing in `hurl/listing_status.hurl` calls it — by design, per
  the "when to use each" section above.
