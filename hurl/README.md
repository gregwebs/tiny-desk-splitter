# Hurl web integration tests

Black-box HTTP tests against a real `concert-web` process, as opposed to
`concert-tracker/tests/web_integration.rs`, which links directly into the Rust
web implementation and calls the axum router in-process via
`tower::ServiceExt::oneshot`. This guide is the canonical Test Control API
contract. See
[`docs/adr/0001-jsonrpsee-for-test-control-api.md`](../docs/adr/0001-jsonrpsee-for-test-control-api.md)
for why the Test Control API is JSON-RPC. Seed defaulting is captured in
[`docs/adr/0003-test-control-seed-defaults.md`](../docs/adr/0003-test-control-seed-defaults.md).
Hurl requests use the Test Control HTTP Adapter's concise routes rather than
raw JSON-RPC envelopes; see
[`docs/adr/0004-test-control-http-adapter.md`](../docs/adr/0004-test-control-http-adapter.md).

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
| `/test/job/{name}` | `test.job_{name}` |
| `/test/scrape/{name}` | `test.scrape_{name}` |

Raw JSON-RPC (the full `jsonrpc`/`id`/`method`/`params` envelope, posted to
`{{test_control_url}}` with no path suffix) still works — it's the
implementation debug fallback for verifying the underlying JSON-RPC layer
directly, not something new `.hurl` scenarios should reach for. Raw JSON-RPC
seed calls use jsonrpsee's generated request-object shape, so the flat adapter
body appears under `params.params` at the root endpoint. See
[`docs/adr/0004-test-control-http-adapter.md`](../docs/adr/0004-test-control-http-adapter.md)
for the adapter decision, and
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

Scraped, lifecycle, and media seeds default to a three-track set list.
Lifecycle seeds default to inert state: `downloaded = false`, `split = false`,
no timestamps, no media duration, no `tracks_present`, and no `tracks_liked`.

Fixture defaulting follows one rule: a field's Rust `Default` applies when the
field is **omitted**. For every field typed as optional (all of the seed
fields except `downloaded`/`split`/`source_file`), explicit JSON `null` always
deserializes to `None`. For most of those fields (`source_url`, `title`,
`artist`, `album`, `set_list`, `auto_timestamps`, `user_timestamps`,
`media_duration`, `tracks_present`, `tracks_liked`, media seed file-extension
fields) the `Default` is already `None`, so omitting the field and sending
`null` behave identically — both generate a fixture value (identity fields,
`set_list`) or leave the field absent (timestamps, media duration,
tracks-present/liked, media-file extension overrides). Pass `"set_list": []`
for an explicitly *empty* set list;
`null`/omitted both mean "generate the three-track default".

`tracks_present` (lifecycle/media seeds only) writes a raw `Vec<bool>` verbatim via
`db::split_timestamps::set_tracks_present` with no length check against
`set_list` — the `/like` handler and media-info routes already tolerate a
short array (`.get(idx).unwrap_or(false)`), so seeding a mismatched length is
valid and exercises that same defensive path. This is the only Test Control
knob for "this track is available" that doesn't require writing an actual
file to the scratch workdir; see `hurl/media_info_navigation.hurl` for a
representative consumer.

`tracks_liked` (lifecycle/media seeds only) writes a raw `Vec<bool>` verbatim
via `db::split_timestamps::set_tracks_liked`, with the same permissive length
semantics. Use it when a black-box route needs to observe liked metadata
without first driving the `/like` endpoint.

`test.seed_media_concert` accepts the same lifecycle fields plus optional
dummy media-file controls. `"track_files": [0, 2]` writes dummy `.mp3` files
for the selected set-list indices under the scratch workdir; override the one
extension for all track files with `"track_file_extension": "mp4"` when a case
needs video-file extension behavior. `"source_file": true` writes a dummy
album source file, defaulting to `.mp3` and overrideable with
`"source_file_extension"`. These files are intentionally tiny sentinel bytes,
not valid audio/video; use them only for routes that check existence or
extension.

Four more `test.seed_media_concert` fields, all `false`/absent by default,
name a domain artifact rather than a raw file write:

- **`"preview_image": true`** — writes a sentinel `preview.jpg` in the
  concert directory (for routes that delete or serve the cached scrape
  thumbnail).
- **`"interlude_files": true`** — writes sentinel interlude track files for
  every gap `derive_interludes` finds between the seeded `user_timestamps`
  (falling back to `auto_timestamps`) and `media_duration` — both of which
  must also be present on the same seed request, or the seed call errors.
- **`"legacy_timestamps_json": true`** — writes an on-disk `timestamps.json`
  in the pre-DB-column `ConcertInfo` shape (readable by
  `jobs::split::read_analysis_timestamps`), generated from the set list via
  the same deterministic fake-analysis timestamps the Job Driver uses for a
  completed `SplitMode::Analyze`. Conflicts with `auto_timestamps`: a
  genuinely legacy concert has no auto column by definition.
- **`"source_file_kind": "real_audio"`** (default `"sentinel"`) — generates a
  genuinely playable ~5s sine-wave `.m4a` with `ffmpeg` instead of a sentinel
  byte file, for the one class of route whose public behavior depends on real
  `ffprobe` output (the split-timestamps POST bounds-checks proposed end
  times against the source's real duration). Requires `"source_file": true`
  on the same request — `source_file_kind` alone does not create a file — and
  requires `ffmpeg` on `PATH` (present in CI; the seed call fails loudly,
  not silently, if it's missing). Every other Test Control file stays a
  sentinel; this is deliberately the only exception.

`test.seed_album_null_concert` is a separate seed, not a `seed_media_concert`
variant: it seeds a concert with track state but a NULL `album` column — a
historical/defensive shape no current product write path produces
(`update_metadata` requires a real `album: String`, and `album: null` on the
other seeds means "generate a default", not "store NULL"). Accepts
`source_url`, `title`, `set_list`, `tracks_present`, `tracks_liked` (all
optional, same defaulting rules as the lifecycle seed).

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

## Job Driver

`hurl/job_chain.hurl` drives download/split/opener behavior deterministically
through the **Job Driver** instead of injecting fake shell commands. See
[`docs/jobs.md`](../docs/jobs.md) and
[`docs/adr/0005-typed-job-runner-for-test-control.md`](../docs/adr/0005-typed-job-runner-for-test-control.md)
for the lasting design. A test-control build only uses the Job Driver when
`--test-control-port` is actually passed — running `concert-web --features
test-control` without that flag behaves exactly like a production build.

Three adapter routes, all under `/test/job/{name}` → `test.job_{name}` (flat
params, same passthrough as `/test/assert/{name}`, not `/test/seed/{name}`'s
request-object wrapping):

- **`/test/job/set_plan`** — `{"concert_id": <id or omitted>, "download":
  "succeed"|"fail"|"block", "split": ..., "open": ...}`. Omitting
  `concert_id` sets the process-wide default plan (starts as all-`succeed`);
  passing it sets a per-concert override, materialized from the current
  default the first time it's set for that concert and updated independently
  of later default changes. Only present fields change. **Prefer per-concert
  overrides over touching the default** — every `.hurl` file in this
  directory shares one `concert-web` process for the whole `just test-hurl`
  run (see "Running" above), so a case that changes the default plan and
  doesn't restore it before the file ends can affect later files/cases.
  `open` never accepts `"block"` — see below.
- **`/test/job/release`** — `{"concert_id": <id>, "kind":
  "download"|"split"|"open", "outcome": "succeed"|"fail"}`. Releases a step
  currently blocked at `(concert_id, kind)`. **Errors if the step hasn't
  registered as blocked yet** — poll `test.assert_job_observation` for
  `blocked=1` first (Hurl's `[Options] retry: N retry-interval: Xms` on the
  assert request), the same poll-then-act idiom used throughout this
  directory for async completion. Racing a release against an unregistered
  block is a caller bug, not a race the Job Driver smooths over.
- **`/test/assert/job_observation`** — `{"concert_id": <id>, "kind": ...,
  "started": <n>, "completed": <n>, "failed": <n>, "blocked": <n>,
  "released": <n>}`. Same shape as `test.assert_concert_state`: only present
  fields are checked, every mismatch is reported together, and a call with
  every count field omitted is rejected. Use it for concurrency/dependency-edge
  facts with no public HTTP surface (e.g. "exactly one split ran," "no split
  ran at all").

**`open` cannot be blocked.** `watch`/`watch_track` await the opener
synchronously inline in the HTTP handler (unlike download/split, which run in
a detached spawned task and return `200` immediately) — a blocked `open`
would hang the response itself, and Hurl executes requests strictly
sequentially within a file, so nothing could ever call `job_release` while an
earlier request is still awaiting its response. `job_set_plan`/`job_release`
reject `open = "block"` outright.

A `Succeed` outcome writes the same sentinel files (source video, `.m4a`
track files via `sanitize_filename`, `timestamps.json`, interlude files when
timestamp gaps require them) the real download/split subprocess would have
created, since the existing job lifecycle code reads the filesystem
immediately after a step succeeds. Sentinels are tiny non-media bytes — same
convention as `test.seed_media_concert`'s dummy files.

## Scrape Driver

`hurl/scrape_pending.hurl` drives the background metadata-scrape queue's
timing deterministically through the **Scrape Driver**, instead of injecting
a stub scrape item in Rust. See [`docs/jobs.md`](../docs/jobs.md) for the
lasting runner-boundary design. The scrape queue is **not** a
download/split/open job —
it has its own in-memory `pending` set and injectable per-item function
(`jobs::scrape_queue::ScrapeItemFn`), not a `JobRunner` — so the Scrape
Driver is a separate control surface from the Job Driver above, not another
`JobStepKind`. Same activation rule as the Job Driver: a test-control build
only uses it when `--test-control-port` is actually passed.

Three adapter routes under `/test/scrape/{name}` → `test.scrape_{name}` (flat
params, same passthrough as `/test/job/{name}`), plus one assertion route:

- **`/test/scrape/set_plan`** — `{"concert_id": <id>, "scrape":
  "succeed"|"block"}`. **Per-concert only — there is no process-wide default
  plan.** A concert with no plan set always scrape-succeeds deterministically,
  so (unlike the Job Driver's default plan) there is no "restore it before the
  file ends" discipline to follow here.
- **`/test/scrape/enqueue`** — `{"concert_id": <id>}` → `{"ok": true,
  "enqueued": <bool>}`. Looks up the concert's `source_url` and queues it on
  the app's real `ScrapeQueue` (the same queue `/sync/:year/:month` uses).
  `enqueued: false` means the concert was already queued/in-flight — a normal
  dedupe no-op, not an error.
- **`/test/scrape/release`** — `{"concert_id": <id>}`. Releases a scrape
  currently blocked for `concert_id`. **Errors if it hasn't registered as
  blocked yet** — poll `test.assert_scrape_observation` for `blocked=1` first,
  the same poll-then-act idiom the Job Driver uses. Unlike
  `/test/job/release`, there's no `outcome` to pass: a release always resolves
  to the deterministic success fixture below.
- **`/test/assert/scrape_observation`** — `{"concert_id": <id>, "started":
  <n>, "completed": <n>, "blocked": <n>, "released": <n>}`. Same shape as
  `test.assert_job_observation`: only present fields are checked, every
  mismatch is reported together, and a call with every count field omitted is
  rejected.

A released (or unblocked-default) scrape writes deterministic fixtures, not a
network fetch: `update_metadata` with artist `"Scrape Driver Artist {id}"` and
album `"Scrape Driver Album {id}"` (which sets `metadata_scraped_at`, flipping
the card from loading to a thumbnail), plus a **real, tiny decodable JPEG**
(not a text sentinel) written to the listing thumbnail path. The card's
`<img onerror="this.style.display='none'">` would silently hide a
non-image file, so `hurl/scrape_pending.hurl` asserts the JPEG magic bytes on
`GET /thumbnails/...`, not just a `200`.

`test.reset` drops any blocked scrape's release channel, waking it to return
without writing fixtures — but, like the Job Driver, this is best-effort, not
a queue quiescence boundary (a request already queued but not yet picked up
still runs after `reset` returns). No `.hurl` file calls `/test/reset`
mid-run regardless (see "Running" above), so this has not needed a stronger
guarantee in practice.

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
3. **Assertion API methods** (`test.assert_concert_state`,
   `test.assert_job_observation`, `test.assert_concert_events`, reached via
   their `/test/assert/{name}` adapter route) — only when a postcondition is
   internal-only and no public route exposes it. No case in
   `hurl/listing_status.hurl` needs this — every postcondition the first
   slice checks (a listing appears, the ignored badge/filter, the
   scraped-status fragment) is already public.
   [`hurl/test_control_adapter.hurl`](test_control_adapter.hurl) does use it,
   as adapter-route coverage. See its doc comment in
   `concert-tracker/src/test_control.rs`.

   `test.assert_concert_events` checks the internal event log for facts with
   no public HTTP surface: `{"concert_id": <id>, "present": [<event names>],
   "absent": [<event names>]}`. At least one of `present`/`absent` must be
   non-empty, and every listed name must be a real event (see
   `crate::events::Event::parse`) — same "reject a vacuous call" shape as
   `assert_concert_state`. First (and so far only) consumer: interlude
   deletion in `hurl/concert_playback.hurl` asserts an `interlude_delete`
   event was recorded and no `track_delete` event was — deleting an
   interlude is not deleting a song track, and this is the only way to
   observe that distinction, since both actions return a similar sidebar
   fragment.
4. **Job Driver control actions** (`test.job_set_plan`, `test.job_release`,
   reached via `/test/job/{name}`) — imperative, not a check: configuring
   deterministic download/split/opener behavior and releasing a blocked step.
   See "Job Driver" above.
5. **Scrape Driver control actions** (`test.scrape_set_plan`,
   `test.scrape_enqueue`, `test.scrape_release`, reached via
   `/test/scrape/{name}`) — same imperative shape as the Job Driver, for the
   background metadata-scrape queue specifically. See "Scrape Driver" above.

## Verification commands for future agents

```sh
cargo check -p concert-tracker --features test-control
cargo check -p concert-tracker
cargo build --bin concert-web --features test-control
node scripts/hurl-test.js --glob 'hurl/job_chain.hurl'
node scripts/hurl-test.js --glob 'hurl/media_files_lifecycle.hurl'
node scripts/hurl-test.js --glob 'hurl/split_timestamps_flow.hurl'
node scripts/hurl-test.js --glob 'hurl/concert_playback.hurl'
node scripts/hurl-test.js --glob 'hurl/scrape_pending.hurl'
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

After the state-only public HTTP, playlist, state-only-stragglers,
media-info navigation, Job Driver, Scenario Seeds, and Scrape Driver slices,
`concert-tracker/tests/web_integration.rs` has 3 tests left, and all 3 are
intentionally staying Rust-only:

- **`detail_page_auto_scrape_failure_still_renders`** — exercises a real
  outbound scrape failure (a connection-refused request) and proxy
  disabling; deliberately kept as the one real failing-scrape regression
  rather than a Job-Driver-style deterministic stand-in.
- **`prod_router_serves_embedded_js_without_livereload`** — distinguishes
  dev vs. prod router wiring (an internal construction detail, not something
  a black-box HTTP client observes differently).
- **`served_openapi_spec_matches_built_api_doc`** — compares two in-process
  Rust values (the built OpenAPI doc vs. what's served) — a pure
  Rust-internal consistency check with no black-box equivalent.

Current black-box coverage is organized by product boundary:

- listing, filtering, detail, notes, and state errors:
  `listing_status.hurl`, `detail_prepare_notes.hurl`, and
  `media_state_errors.hurl`;
- playlists and media navigation: `playlists.hurl` and
  `media_info_navigation.hurl`;
- download/split/opener orchestration: `job_chain.hurl`;
- filesystem lifecycle and timestamp workflows:
  `media_files_lifecycle.hurl`, `split_timestamps_state.hurl`, and
  `split_timestamps_flow.hurl`;
- playback reconstruction and interlude deletion: `concert_playback.hurl`;
- background scrape timing: `scrape_pending.hurl`;
- Test Control transport and adapter contracts: `test_control_adapter.hurl`.

The completed migration history is recorded in
[`docs/change/2026-07-17-hurl-migration-sweep.md`](../docs/change/2026-07-17-hurl-migration-sweep.md).

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
