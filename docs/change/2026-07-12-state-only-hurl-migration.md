# State-only Hurl migration

Implemented issue #94, the second Hurl migration slice for public HTTP tests
that need only database state.

## Changes

- Added `test.seed_lifecycle_concert` to the feature-gated Test Control API.
  It seeds listing, metadata, downloaded/split lifecycle state, optional
  split timestamps, and optional media duration through existing `db::concerts`,
  `db::lifecycle`, and `db::split_timestamps` functions.
- The lifecycle seed is state-only: it never writes media files, thumbnails,
  `timestamps.json`, or track-presence flags. Tests that depend on real media
  remain in Rust.
- Added Hurl coverage for the migrated list/status, notes/detail/prepare,
  media missing-file, split-timestamp, delete-split, like bounds, and concert
  playback 404 cases.
- Removed the corresponding duplicate Rust integration tests after the Hurl
  suite passed.
- Updated `hurl/README.md` so the remaining Rust bucket lists tests that need
  job stubbing, scrape timing, opener injection, raw-SQL-only states, or real
  filesystem media.

## Verification

- `cargo test -p concert-tracker test_control --features test-control`
- `just test-hurl`
- `cargo test -p concert-tracker --test web_integration --no-run`
