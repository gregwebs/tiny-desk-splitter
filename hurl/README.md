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
Hurl requests use the Test Control HTTP Adapter's concise routes rather than
raw JSON-RPC envelopes; see
[`docs/adr/0004-test-control-http-adapter.md`](../docs/adr/0004-test-control-http-adapter.md)
and
[`docs/change/2026-07-13-test-control-http-adapter-spec.md`](../docs/change/2026-07-13-test-control-http-adapter-spec.md).

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
scenario. This also means no `.hurl` file should call the adapter's
`/test/reset` route mid-run: `just test-hurl` shares one process/DB across
every file, so a reset from one file would wipe fixtures another file still
depends on.

## Test Control HTTP Adapter

Hurl requests use the **Test Control HTTP Adapter**'s concise routes — plain
`POST {{test_control_url}}/test/...` calls with just the method's params as
the JSON body, no JSON-RPC envelope to write by hand:

```hurl
POST {{test_control_url}}/test/seed/listing
Content-Type: application/json
{
  "title": "Example"
}
```

The adapter translates that into a single in-process JSON-RPC call (id
`"default"`) and returns the JSON-RPC response envelope unchanged, so
`[Captures]`/`[Asserts]` still read from `$.result...`:

```json
{
  "jsonrpc": "2.0",
  "result": {
    "id": 1,
    "source_url": "https://example.test/tiny-desk/test-control-1",
    "title": "Example",
    "concert_date": "2026-01-01"
  },
  "id": "default"
}
```

| Adapter route | JSON-RPC method |
|---|---|
| `/test/reset` | `test.reset` |
| `/test/seed/{name}` | `test.seed_{name}` |
| `/test/assert/{name}` | `test.assert_{name}` |

Raw JSON-RPC (the full `jsonrpc`/`id`/`method`/`params` envelope, posted to
`{{test_control_url}}` with no path suffix) still works — it's the
implementation debug fallback for verifying the underlying JSON-RPC layer
directly, not something new `.hurl` scenarios should reach for. Raw JSON-RPC
seed calls use jsonrpsee's generated request-object shape, so the flat adapter
body appears under `params.params` at the root endpoint. See
[`docs/adr/0004-test-control-http-adapter.md`](../docs/adr/0004-test-control-http-adapter.md)
and
[`docs/change/2026-07-13-test-control-http-adapter-spec.md`](../docs/change/2026-07-13-test-control-http-adapter-spec.md)
for the full route/translation/error contract, and
[`hurl/test_control_adapter.hurl`](test_control_adapter.hurl) for a worked
example of both, including the adapter's invalid-JSON HTTP 400 response.

## Test Control seed defaults

The seed methods accept an empty JSON object when a scenario does not care
about fixture identity fields:

```hurl
POST {{test_control_url}}/test/seed/lifecycle_concert
Content-Type: application/json
{}
```

Explicit params override generated defaults:

```hurl
POST {{test_control_url}}/test/seed/lifecycle_concert
Content-Type: application/json
{
  "title": "Downloaded Filter Fixture",
  "downloaded": true
}
```

Generated fixture URLs use `https://example.test/`, and generated identities
come from a server-local monotonic counter (`db::seeds::FixtureIds`, held for
the lifetime of the `concert-web` process). `test.reset` clears database and
workdir state but does not reset that counter, so defaults remain
conflict-free within one `just test-hurl` run.

Scraped and lifecycle seeds default to a three-track set list. Lifecycle seeds
default to inert state: `downloaded = false`, `split = false`, no timestamps,
no media duration, and no `tracks_present`.

Fixture defaulting follows one rule: a field's Rust `Default` applies when the
field is **omitted**. For every field typed as optional (all of the seed
fields except `downloaded`/`split`), explicit JSON `null` always deserializes
to `None`. For most of those fields (`source_url`, `title`, `artist`,
`album`, `set_list`, `auto_timestamps`, `user_timestamps`, `media_duration`,
`tracks_present`) the `Default` is already `None`, so omitting the field and
sending `null` behave identically — both generate a fixture value (identity
fields, `set_list`) or leave the field absent (timestamps, media duration,
tracks-present). Pass `"set_list": []` for an explicitly *empty* set list;
`null`/omitted both mean "generate the three-track default".

`tracks_present` (lifecycle seeds only) writes a raw `Vec<bool>` verbatim via
`db::split_timestamps::set_tracks_present` with no length check against
`set_list` — the `/like` handler and media-info routes already tolerate a
short array (`.get(idx).unwrap_or(false)`), so seeding a mismatched length is
valid and exercises that same defensive path. This is the only Test Control
knob for "this track is available" that doesn't require writing an actual
file to the scratch workdir; see
[`docs/change/2026-07-14-state-only-stragglers-hurl-migration.md`](../docs/change/2026-07-14-state-only-stragglers-hurl-migration.md).

Two fields are the exception, because their `Default` is `Some(...)` rather
than `None`: `concert_date` (default `"2026-01-01"`) and `teaser` (default
`"Test listing teaser"`, listing seeds only). For these, omitting the field
takes the default text, while explicit `null` stores a real SQL `NULL` — the
only way to seed a concert with no date or no teaser. Sending
`"source_url": null` (or `null` for `title`/`artist`/`album`) is valid and
behaves exactly like omitting the field — Test Control no longer rejects
explicit `null` for identity fields.

`downloaded`/`split` are plain booleans, not optional: omitting them takes the
`false` default, but `"downloaded": null` is invalid params, the same as
sending any other non-boolean value for them would be.

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
3. **Assertion API methods** (`test.assert_concert_state` today, reached via
   its adapter route `/test/assert/concert_state`) — only when a
   postcondition is internal-only and no public route exposes it. No case in
   `hurl/listing_status.hurl` needs this — every postcondition the first
   slice checks (a listing appears, the ignored badge/filter, the
   scraped-status fragment) is already public.
   [`hurl/test_control_adapter.hurl`](test_control_adapter.hurl) does use it,
   as adapter-route coverage. See its doc comment in
   `concert-tracker/src/test_control.rs`.

## Verification commands for future agents

```sh
cargo check -p concert-tracker --features test-control
cargo check -p concert-tracker
cargo build --bin concert-web --features test-control
node scripts/hurl-test.js --glob 'hurl/test_control_adapter.hurl'
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

After the state-only public HTTP, playlist, and state-only-stragglers slices,
`concert-tracker/tests/web_integration.rs` still has 40 tests. This groups
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
  `delete_interlude_removes_file_records_event_returns_fragment` is in this
  bucket for its file fixture, but its internal-events postcondition (an
  `interlude_delete` event recorded, no `track_delete`) has no public HTTP
  surface either — when this bucket's design lands, add a
  `test.assert_concert_events` Assertion API method (decided during the
  state-only-stragglers slice review, not implemented there since no
  migrating test at the time consumed it) rather than dropping that coverage.
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
- **State-only stragglers are migrated.** The fourth Hurl slice added a
  `tracks_present` param to `test.seed_lifecycle_concert` (see "Test Control
  seed defaults" above) and moved
  `delete_download_force_clears_state_when_file_missing`,
  `delete_split_clears_state`, `like_track_toggles_state_and_renders_star`,
  and `like_track_unavailable_returns_404` — the last four remaining tests
  that needed neither files nor job stubs, blocked only by the seed API's
  inability to set the `tracks_present` DB column the `/like` handler reads.
  See
  [`docs/change/2026-07-14-state-only-stragglers-hurl-migration.md`](../docs/change/2026-07-14-state-only-stragglers-hurl-migration.md).
- **Still intentionally Rust-only after the state-only-stragglers slice.**
  `detail_page_auto_scrape_failure_still_renders` exercises an outbound
  scrape failure and proxy disabling. `track_details_returns_200_without_album`
  relies on a raw-SQL `NULL` album no real lifecycle (or the seed API's null
  semantics) produces. `set_split_timestamps_returns_422_on_count_mismatch`
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

- **`listing_status.hurl` still has no Assertion API case.** `test.assert_concert_state`
  is exercised by `hurl/test_control_adapter.hurl`, but no case in
  `hurl/listing_status.hurl` needs it — by design, per the "when to use each"
  section above; every postcondition it checks is already public.
