# Hurl web integration tests

Black-box HTTP tests against a real `concert-web` process, as opposed to
`concert-tracker/tests/web_integration.rs`, which links directly into the Rust
web implementation and calls the axum router in-process via
`tower::ServiceExt::oneshot`. See
[`docs/change/2026-07-11-hurl-web-integration-tests.md`](../docs/change/2026-07-11-hurl-web-integration-tests.md)
for the full design (architecture, decisions, Test Control API contracts) and
[`docs/adr/0001-jsonrpsee-for-test-control-api.md`](../docs/adr/0001-jsonrpsee-for-test-control-api.md)
for why the Test Control API is JSON-RPC. Seed defaulting is captured in
[`docs/adr/0003-test-control-seed-defaults.md`](../docs/adr/0003-test-control-seed-defaults.md)
and
[`docs/change/2026-07-12-test-control-seed-defaults-spec.md`](../docs/change/2026-07-12-test-control-seed-defaults-spec.md).

It runs locally via `just test-hurl` and is a blocking step in CI (see "CI"
below).

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
and should rely on Test Control's generated seed defaults or explicit
distinguishing `source_url`s rather than relying on `test.reset` between every
scenario.

## Test Control seed defaults

The seed methods accept an empty flat params object when a scenario does not
care about fixture identity fields:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "test.seed_lifecycle_concert",
  "params": {}
}
```

Keep the JSON-RPC envelope as-is for now: Hurl requests still send `jsonrpc`,
`id`, `method`, and `params`. The `params` value remains a flat map; do not
wrap seed fields under a nested `params.params` object.

Explicit flat-map params override generated defaults:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "test.seed_lifecycle_concert",
  "params": {
    "title": "Downloaded Filter Fixture",
    "downloaded": true
  }
}
```

Generated fixture URLs use `https://example.test/`, and generated identities
come from a server-local monotonic counter. `test.reset` clears database and
workdir state but does not reset that counter, so defaults remain
conflict-free within one `just test-hurl` run.

Scraped and lifecycle seeds default to a three-track set list. Lifecycle seeds
default to inert state: `downloaded = false`, `split = false`, no timestamps,
and no media duration. Pass explicit `null` only for nullable domain fields
(`concert_date`, `teaser`, `set_list`, `auto_timestamps`, `user_timestamps`,
`media_duration`). Identity fields (`source_url`, `title`, `artist`, `album`)
must be omitted or strings; explicit `null` is rejected.

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

After the state-only public HTTP and playlist slices,
`concert-tracker/tests/web_integration.rs` still has 44 tests. This groups
them by *why*, matching the migration specs'
out-of-scope lists plus a couple of pre-existing areas these slices never
touched. It's a categorization of the shape of the remaining suite, not an
exhaustive per-test audit; several examples are named under each bucket, but
the buckets cover more tests than are named here.

- **Job command stubbing for download/split chains** (spec: explicitly out of
  scope). The largest group: everything that injects a fake download/split
  command via `state_with_chain`/`JobConfig` to control success, failure, or
  retry behavior deterministically and instantly, without a real subprocess —
  e.g. `download_endpoint_spawns_job_and_returns_row`,
  `prepare_endpoint_runs_download_then_split_chain`,
  `download_auto_split_retries_on_split_error`,
  `download_double_click_does_not_drop_split_edge`, and the delete cases that
  require a stubbed job outcome or real file deletion. The Test Control API has
  no equivalent for "run a job with an injected outcome" yet — that needs its
  own design (a later migration slice), not a bolt-on to the seed methods.
- **Scrape queue timing** (spec: explicitly out of scope).
  `pending_card_shows_loading_then_thumbnail` injects a stub scrape item and
  release/done channels to deterministically observe the pending → thumbnail
  transition mid-flight. No Test Control equivalent exists for controlling
  worker timing.
- **Opener command injection** (spec: explicitly out of scope).
  `watch_uses_injected_opener_and_succeeds`,
  `watch_returns_500_when_opener_fails` inject a success/failure closure for
  the "open in system player" command.
- **Filesystem/media-fixture-heavy tests**. Track navigation, media-info,
  watch/like, playback-reconstruction, and split-timestamps happy paths
  generate real tiny playable media with `ffmpeg` (`create_test_audio`) or
  write source/track/`timestamps.json` files and assert on exact filesystem
  state. Seeding that through Test Control would need file-producing seed
  methods and a separate design.
- **Router/build-internals checks that aren't user-facing HTTP behavior**.
  `prod_router_serves_embedded_js_without_livereload` distinguishes dev vs.
  prod router wiring (an internal construction detail, not something a
  black-box HTTP client observes differently). `served_openapi_spec_matches_built_api_doc`
  compares two in-process Rust values (the built OpenAPI doc vs. what's
  served) — a pure Rust-internal consistency check with no black-box
  equivalent.
- **State-only public HTTP tests are migrated.** The second Hurl slice added
  `test.seed_lifecycle_concert` and moved the remaining pure public-HTTP,
  no-files/no-stub cases into Hurl: available status actions, notes/detail,
  downloaded filtering, missing-file media errors, prepare 404/422 responses,
  split-timestamp 404/409/422/read/reset state cases, delete-split timestamp
  preservation, and empty concert playback 404.
- **Playlist API and HTML pages are migrated.** The third Hurl slice
  (`hurl/playlists.hurl`) moved `playlist_api_crud_and_resolution`,
  `playlist_api_validation_status_codes`, `playlists_html_pages_render`, and
  `playlist_detail_page_unknown_id_is_404` — no new Test Control surface was
  needed, since `test.seed_lifecycle_concert` already covers the fixture
  shape those tests seeded by hand. Two markup-internal assertions (the
  `data-playlist-id` attribute, the nav `href="/playlists"` link) were
  intentionally dropped from the Hurl port rather than translated
  byte-for-byte; that coverage now lives in `e2e/playlists.spec.js`'s
  drag-drop reorder test, which drives the real DOM attribute. See
  [`docs/change/2026-07-12-playlist-hurl-migration.md`](../docs/change/2026-07-12-playlist-hurl-migration.md).
- **Still intentionally Rust-only after the state-only slice.**
  `detail_page_auto_scrape_failure_still_renders` exercises an outbound
  scrape failure and proxy disabling. `track_details_returns_200_without_album`
  and the like/unavailable cases rely on raw-SQL or presence states no real
  lifecycle produces without files. `set_split_timestamps_returns_422_on_count_mismatch`
  requires a source file to pass the earlier 409 check. The remaining prepare,
  playback reconstruction, lazy-backfill, source-present playback, and
  play-button tests depend on real files or injected jobs.

## CI

`.github/workflows/ci.yml`'s `rust` job runs `just test-hurl` as a **blocking**
step, after `cargo nextest run --tests` and before the working-tree-clean
check. It installs `hurl` from the official `.deb` release artifact (pinned
`HURL_VERSION`, bumped deliberately) and `just` via `taiki-e/install-action`,
then runs the same command a contributor runs locally — `scripts/hurl-test.js`
builds `concert-web --features test-control` itself. A failing `.hurl` case
fails the PR.

The same job also runs a **release-safety guard** step:
`cargo build --release --bin concert-web --features test-control` is expected
to *fail*, and the step further asserts the failure comes from the
`compile_error!` in `concert-tracker/src/test_control.rs` (not some unrelated
build break masking a missing guard). This exists because `just test-hurl`
only builds in debug mode and would not notice if that guard were ever removed
or bypassed — the CI step catches that regression independently of the Hurl
suite passing.

## Known gaps

- **No Assertion API consumer yet.** `test.assert_concert_state` exists (see
  above) but nothing in `hurl/listing_status.hurl` calls it — by design, per
  the "when to use each" section above.
