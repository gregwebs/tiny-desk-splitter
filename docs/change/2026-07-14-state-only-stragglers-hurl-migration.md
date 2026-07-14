# State-only stragglers Hurl migration

Fourth Hurl migration slice: moved the last four `web_integration.rs` tests
that needed neither files nor job stubs into `hurl/media_state_errors.hurl`
and `hurl/split_timestamps_state.hurl`.

## Changes

- Added a `tracks_present: Option<Vec<bool>>` param to
  `db::seeds::SeedLifecycleConcert` / `test.seed_lifecycle_concert`. Omitted
  or explicit `null` leaves the column `NULL` (inert, matching every other
  optional field); an explicit array is written verbatim via the existing
  `db::split_timestamps::set_tracks_present`, with no length check against
  `set_list` — the `/like` and media-info handlers already tolerate a short
  array (`.get(idx).unwrap_or(false)`), so permissive seeding exercises that
  same defensive path rather than fighting it. `reset_fixture_lifecycle_state`
  already NULLs `tracks_present` on a reused `source_url`; no change was
  needed there.
- **`tracks_present` is not echoed on `SeedLifecycleConcertResult`.** An
  initial draft added it as a request-echo field, matching the existing
  `downloaded`/`split` result booleans; a Codex adversarial review of the
  plan flagged that a request-echo test oracle would let a broken seed write
  pass its own unit test undetected, since `downloaded`/`split` are actually
  *re-derived from the persisted row*, not echoed from the request. No Hurl
  case needs the field on the result today, so it was dropped entirely
  rather than implemented as a request-echo; add it (persisted-row-derived)
  if a future Hurl case needs to capture it.
- Added Hurl coverage:
  - `hurl/media_state_errors.hurl`: `delete-download?force=true` on a
    concert whose downloaded file was never seeded (mirrors the existing
    non-force confirm-fragment case just above it).
  - `hurl/split_timestamps_state.hurl`: plain `delete-split` on a downloaded
    + split concert, asserting via `test/assert/concert_state` that split
    state clears while download state is untouched — this assertion call is
    the direct replacement for the old Rust test's in-process DB reads, not
    optional coverage.
  - `hurl/split_timestamps_state.hurl`: the two `/like` toggle cases (star
    renders on an available track, 404 on an unavailable one), seeded via
    the new `tracks_present` param next to the existing out-of-range case.
- Removed the four migrated tests from `web_integration.rs`
  (`delete_download_force_clears_state_when_file_missing`,
  `delete_split_clears_state`, `like_track_toggles_state_and_renders_star`,
  `like_track_unavailable_returns_404`), leaving "migrated to hurl/…"
  comment breadcrumbs in place of each. `web_integration.rs` goes from 44
  tests to 40.
- Updated `hurl/README.md`: documented `tracks_present` in the seed-defaults
  section, added this slice to the "why the remaining tests are Rust-only"
  breakdown, fixed the stale test count, and recorded the decision to add a
  `test.assert_concert_events` Assertion API method in the future
  filesystem/media-fixture migration slice (for
  `delete_interlude_removes_file_records_event_returns_fragment`'s internal
  events postcondition) rather than implementing it now, since no test in
  this slice consumes it.

## Verification

- `cargo check -p concert-tracker --features test-control`
- `cargo test -p concert-tracker db::seeds:: --features test-control` —
  covers persistence, and reseed-clears-the-column for both omitted and
  explicit `null`
- `cargo test -p concert-tracker test_control:: --features test-control` —
  RPC-dispatch test asserting the param reaches the DB through the generated
  method path (against the persisted row, not the request)
- `node scripts/hurl-test.js --glob 'hurl/media_state_errors.hurl'`
- `node scripts/hurl-test.js --glob 'hurl/split_timestamps_state.hurl'`
- `just test-hurl` — all six `.hurl` files together against one shared
  server/DB
- `cargo nextest run -p concert-tracker --test web_integration` — 40 tests,
  all passing
- `just lint`, plus `cargo clippy -p concert-tracker --all-targets --features
  test-control -- -D warnings` (the default `just lint` clippy invocation
  doesn't enable `test-control`, so the feature-gated seed/test-control code
  needs its own pass)
- `cargo build --release --bin concert-web --features test-control` —
  confirmed to still fail via the `compile_error!` release guard
