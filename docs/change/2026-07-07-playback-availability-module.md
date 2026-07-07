# Playback Availability Module

This refactor moves backend playback availability decisions out of HTTP
handlers and into `concert-tracker/src/playback.rs`.

## Scope

- Source playback lookup.
- Source-vs-reconstruction playback planning.
- Per-track media lookup.
- Next/previous playable track lookup.
- Track detail availability facts.

Card rendering policy, `source_redundant`, `can_play_concert`, queued split
state, and `tracks_busy` remain outside the playback module.

## State Changes

None. This is a refactor only.

| Request condition | Before | After |
|---|---|---|
| Source file exists | Source playback response | Same |
| Source absent, reconstruction playable | Reconstruction response | Same |
| Source absent, no reconstruction items | 404 | Same |
| DB downloaded state but source missing on `/media-info` | 500 | Same |
| No next/previous playable track | 404 | Same |
| Track details requested while jobs are busy | Handler adds `tracks_busy` | Same |

## Verification

- `cargo check -p concert-tracker`
- `cargo test -p concert-tracker playback --lib`
- `cargo test -p concert-tracker --test web_integration`
- `just lint`
- Manual backend smoke with a separate copied fixture database/workdir and an
  ephemeral-port `concert-web` server:
  - `/concerts/2/media-info` returned source playback metadata.
  - `/concerts/2/concert-playback` returned source playback metadata.
  - `/concerts/2/tracks/0/media-info` returned track playback metadata.
  - `/concerts/2/tracks/0/next-media-info` returned the next playable track.
  - `/concerts/2/track-details` returned availability facts for all tracks.
  - After removing the copied source file, `/concerts/2/concert-playback`
    returned reconstruction playback metadata.
- Playwright smoke was attempted for player queue and concert reconstruction
  scenarios, but local Chromium exited with `SIGTRAP` before any application
  assertions ran. This matches the host-level failure mode documented in
  `docs/playwright.md`.
