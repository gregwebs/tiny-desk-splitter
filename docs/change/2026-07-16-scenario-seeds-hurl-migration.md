# Scenario Seeds and file-heavy Hurl migration

Implemented slice 3 of the remaining web-integration Hurl migration
([#109](https://github.com/gregwebs/tiny-desk-splitter/issues/109), parent
[#106](https://github.com/gregwebs/tiny-desk-splitter/issues/106)): Scenario
Seed extensions and an event assertion API, used to migrate the last
file-heavy black-box HTTP tests out of
`concert-tracker/tests/web_integration.rs`. Slices 1 (#107, typed job
runner) and 2 (#108, Job Driver) landed earlier on this branch.

## What changed

- `concert-tracker/src/db/seeds.rs`: promoted the sentinel/file-writing
  helpers previously private to `test_control::job_driver`
  (`SENTINEL_BYTES`, `write_track_sentinels`, `write_interlude_sentinels`,
  `write_legacy_timestamps_json`, `fake_analysis_timestamps`) to
  `pub(crate)` items here, removing the duplication between the Job Driver's
  fixture-file writers and `seed_media_concert`'s. `write_track_sentinels`
  now takes an explicit extension (`job_driver` always passes `"m4a"`;
  `seed_media_concert`'s track-file writer passes its caller-resolved,
  possibly-overridden extension) instead of each caller hardcoding its own
  loop.
- `SeedMediaConcert` gained four new fields, all `false`/absent by default:
  - `preview_image: bool` — writes a sentinel `preview.jpg`.
  - `interlude_files: bool` — writes sentinel interlude files for every gap
    `derive_interludes` finds between the seeded `user_timestamps` (falling
    back to `auto_timestamps`) and `media_duration`; validation requires
    both to be present.
  - `legacy_timestamps_json: bool` — writes an on-disk `timestamps.json` in
    the pre-DB-column `ConcertInfo` shape, generated from the set list via
    `fake_analysis_timestamps`; validation rejects it alongside
    `auto_timestamps` (a genuinely legacy concert has no auto column).
  - `source_file_kind: SourceFileKind` (`Sentinel` default | `RealAudio`) —
    `RealAudio` shells out to `ffmpeg` to generate a genuine ~5s sine-wave
    `.m4a`, for the one class of route whose public behavior depends on real
    `ffprobe` output. Requires `source_file: true` on the same request and
    an extension of `None`/`"m4a"`; the seed call fails with a clear
    "requires ffmpeg on PATH" message if `ffmpeg` isn't installed, rather
    than silently falling back to a sentinel.
- New named seed `test.seed_album_null_concert` /
  `SeedContext::seed_album_null_concert`: seeds a concert with track state
  but a NULL `album` column — a historical/defensive shape no current
  product write path produces (`update_metadata` requires a real
  `album: String`). Uses direct SQL for `set_list_json` (no product
  function sets it without also requiring an album) and the real
  `db::split_timestamps::set_tracks_present`/`set_tracks_liked` setters for
  the rest.
- New Test Control RPC method `test.assert_concert_events`
  (`{concert_id, present: [event names], absent: [event names]}`, reached
  via the existing `/test/assert/{name}` adapter route — no adapter changes
  needed): checks the internal event log for facts with no public HTTP
  surface. Rejects a vacuous call (both empty) and any name that isn't a
  real event (new `Event::ALL`/`Event::parse` in `src/events.rs` back this
  validation, so a typo in `absent` errors instead of silently never
  matching).
- Three new Hurl files (69 requests total):
  - `hurl/media_files_lifecycle.hurl` — delete-download file removal
    (proved by a before/after `concert-playback` mode change, not a raw
    filesystem check), the prior-split-error delete case (via a Job Driver
    `split: fail` plan), the play-concert button after a successful split,
    ignore-deletes-preview-image, prepare-status filesystem track state,
    track-details with a NULL album, and track-details busy state (arranged
    through a real `split: block` Job Driver plan + `POST /prepare`, not a
    hand-set DB column).
  - `hurl/split_timestamps_flow.hurl` — the legacy `timestamps.json`
    lazy-backfill, the 422 count-mismatch-with-source-present case, the
    real-media split-timestamps POST happy path (the only Hurl case using
    `source_file_kind: real_audio`), and the reset-to-auto happy path.
  - `hurl/concert_playback.hurl` — all three `concert-playback` modes
    (source, reconstruction, reconstruction-with-interlude) and interlude
    deletion (file removal, `assert_concert_events` for
    `interlude_delete` present / `track_delete` absent, and the refreshed
    sidebar fragment).
- Removed the 13 migrated tests (and their now-unused seed/fixture helpers)
  from `concert-tracker/tests/web_integration.rs`, leaving breadcrumb
  comments pointing at the new Hurl files. Four tests remain Rust-only:
  `pending_card_shows_loading_then_thumbnail` (scrape-queue timing, slice 4
  — #110), `detail_page_auto_scrape_failure_still_renders` (real failing
  scrape regression), `prod_router_serves_embedded_js_without_livereload`,
  and `served_openapi_spec_matches_built_api_doc` (both router/build
  internals with no black-box equivalent).
- `hurl/README.md` updated: documents the new seed params, the new named
  seed, and `test.assert_concert_events`; rewrites "Why the remaining tests
  are Rust-only" for the post-slice-3 state (4 tests, all intentional) with
  the prior slices' history kept below it.

## Design decisions

- **Extended `test.seed_media_concert` rather than adding parallel named
  seeds** for the preview/interlude/legacy-timestamps/real-audio shapes.
  Every new field names a domain artifact (a preview image, interlude
  files, a legacy JSON file, real audio) rather than a raw path or bytes, so
  this stays a scenario-seed vocabulary, not a generic filesystem-mutation
  API — which is what the parent spec's "prefer named Scenario Seeds"
  guidance is aimed at ruling out. Parallel seeds would have duplicated the
  whole lifecycle param surface for no semantic gain.
- **`test.seed_album_null_concert` is the one genuinely separate seed**,
  because it is the only shape the lifecycle path structurally cannot
  produce (`album: null` on the other seeds already means "generate a
  default", and `update_metadata` requires a real album string).
- **Real media by shelling out to `ffmpeg`** rather than embedding a
  pre-built `.m4a` in the repo: keeps the fixture in sync with whatever
  `ffprobe` actually accepts on the machine running the tests (the same
  approach the pre-migration Rust test and `scan.rs`'s own test helpers
  used), and CI already installs `ffmpeg` for the Hurl step. An early
  version of this generator wrote real-audio bytes to whatever extension
  `resolved_media_extension` defaulted to (`.mp3`) when the request omitted
  `source_file_extension`, which made `ffmpeg` fail (AAC audio in an MP3
  container); the generator now always targets `.m4a` directly, bypassing
  that unrelated default.
- **Busy state (`tracks_busy`) arranged via a real blocked Job Driver split
  plan**, not a hand-set DB column: `job_set_plan {split: "block"}` +
  `POST /prepare` + poll `assert_job_observation {blocked: 1}` exercises the
  real in-flight-split code path that `tracks_busy` reads, then releases and
  settles before the file's next case runs (this file shares one server
  process with every other `.hurl` file in a run).
- **File-gone proof via public route behavior change**, not a filesystem
  assertion API: deleting the download source is proven by a
  `concert-playback` mode change (source → 404 for an empty set list, or
  source → reconstruction for a split concert) rather than inventing a
  generic "does this file exist" Test Control method, keeping the Assertion
  API surface limited to genuinely internal-only facts.
- **No new ADR.** ADR 0003 (seed defaults) and ADR 0005 (typed job runner)
  already cover the decision space this slice extends.

## Adversarial plan review

An adversarial Codex review (engineering-lead persona) of the implementation
plan found one material issue before coding began: the real-media
happy-path Hurl case, as originally planned, specified
`source_file_kind: "real_audio"` without also passing `source_file: true`.
Since `real_audio` alone would not create a file, the case as planned would
either fail the seed (if validation required `source_file: true`, as it
does) or 409 at the product route (if it didn't). Fixed by requiring
`source_file: true` explicitly in the Hurl case and calling that requirement
out in the plan. During implementation, a second, related issue surfaced
independently (see "Design decisions" above): the ffmpeg-container/extension
mismatch bug. Both are now fixed, and a unit test
(`seed_media_concert_real_audio_produces_a_file_ffprobe_accepts`) proves the
generated audio round-trips through the real `ffprobe`-backed
`probe_media_duration`, not just that a file exists — its ffmpeg-missing
skip condition is deliberately narrow (`"spawning ffmpeg"` only) so a real
generation failure fails the test instead of silently skipping it.

## Code review

A non-adversarial Codex code review (engineering-lead persona) of the
finished implementation found one material issue: `assert_concert_events`
read the event log through `events::list_for_concert`, which swallows every
SQL/row-decoding failure into an empty `Vec` (intentional for its production
rendering callers, so an event-log hiccup degrades to "no events shown"
rather than breaking the page) — but that same swallowing meant a query
failure inside the assertion itself would be misread as "no events" and let
an `absent` expectation vacuously pass. Fixed by adding
`events::try_list_for_concert`, a fallible variant that propagates every
failure mode instead of swallowing it; `list_for_concert` now delegates to
it and keeps its existing swallow-and-warn behavior for its production
callers (`web/handlers.rs`, `lifecycle.rs`), while `assert_concert_events`
calls the fallible variant directly and propagates the error. Two new tests
pin this: `try_list_for_concert_propagates_a_query_failure_instead_of_swallowing_it`
(`events.rs`) and `assert_concert_events_propagates_a_query_failure_instead_of_passing_absent`
(`test_control.rs`, drops the `events` table mid-test and asserts the call
errors rather than reporting `ok: true`). Everything else in the review
matched the plan with no further findings.

A follow-up Codex review of that fix caught a regression the first pass
introduced: the initial `list_for_concert` implementation delegated to
`try_list_for_concert(...).unwrap_or_else(...)`, which changed its
row-decoding-failure behavior — previously a single malformed row was
dropped and every other row was still returned (`filter_map(|r| r.ok())`);
the delegating version turned *any* row failure into a fully empty list,
a production behavior change with no test coverage. Fixed by giving
`list_for_concert` its own independent implementation again (sharing only
the SQL text and row-mapping closure with `try_list_for_concert`, not the
result-collection strategy), so it keeps its original partial-result
behavior while `try_list_for_concert` stays fully fallible. Two new tests
pin the split: `list_for_concert_keeps_well_formed_rows_when_one_row_is_malformed`
and `try_list_for_concert_fails_the_whole_call_when_one_row_is_malformed`
(both insert one well-formed and one row with an undecodable BLOB `json`
value, then assert the two functions' opposite handling of it). A second
follow-up review confirmed this fix closes the gap with no further findings.

## Verification

```sh
cargo check -p concert-tracker --features test-control
cargo check -p concert-tracker
cargo build --bin concert-web --features test-control
node scripts/hurl-test.js --glob 'hurl/media_files_lifecycle.hurl'
node scripts/hurl-test.js --glob 'hurl/split_timestamps_flow.hurl'
node scripts/hurl-test.js --glob 'hurl/concert_playback.hurl'
just test-hurl
cargo nextest run -p concert-tracker --test web_integration
cargo nextest run -p concert-tracker --features test-control
just lint
cargo build --release --bin concert-web --features test-control  # expected to fail (release guard)
```

All of the above pass; the release-guard build fails with the expected
`compile_error!` from `concert-tracker/src/test_control.rs`, confirming
`test-control` still cannot leak into a release build.
