# Media-info navigation Hurl migration

Fifth Hurl migration slice: moved the media-info track navigation and liked
metadata cases from `concert-tracker/tests/web_integration.rs` into
`hurl/media_info_navigation.hurl`.

## Changes

- Added `test.seed_media_concert`, a Test Control seed method for filesystem
  fixtures that only need path existence and file-extension behavior. It wraps
  the lifecycle seed shape and can write dummy track files (`track_files`) and
  a dummy source file (`source_file`) under the scratch workdir. These files
  are intentionally not valid audio/video and must not be used for ffprobe
  paths.
- Added `tracks_liked` to `SeedLifecycleConcert` so Hurl can arrange liked
  metadata directly when the product behavior under test is a read-only
  media-info route, not the `/like` toggle itself.
- Added Rust seed/API coverage:
  - `db::seeds` tests for dummy track/source file creation, out-of-range track
    file rejection, and `tracks_liked` persistence.
  - `test_control` dispatch coverage proving `test.seed_media_concert` writes
    files and liked state through the same API path Hurl uses.
- Added `hurl/media_info_navigation.hurl` coverage for:
  - next track media-info happy path, skip-missing-track path, and last-track
    404
  - previous track media-info happy path, skip-missing-track path, and
    first-track 404
  - track media-info `has_prev`
  - liked true/false metadata, including the empty `tracks_liked` default
  - next media-info carrying liked state to the target track
- Removed the ten now-duplicated Rust tests from `web_integration.rs`, leaving
  comment breadcrumbs. The file now has 30 tests remaining.
- Updated `hurl/README.md` to document `test.seed_media_concert`,
  `tracks_liked`, dummy media-file limitations, and the new remaining-test
  count.

## Verification

- `cargo test -p concert-tracker db::seeds:: --features test-control`
- `cargo test -p concert-tracker test_control:: --features test-control`
- `cargo check -p concert-tracker --features test-control`
- `node scripts/hurl-test.js --glob 'hurl/media_info_navigation.hurl'`
- `cargo check -p concert-tracker`
- `cargo nextest run -p concert-tracker --test web_integration`
- `just test-hurl`
- `just lint`
- `cargo clippy -p concert-tracker --all-targets --features test-control -- -D warnings`
- `cargo build --release --bin concert-web --features test-control` — expected
  failure from the `test-control` release-build guard
