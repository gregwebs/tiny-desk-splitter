# Playback, listen, and like behaviour behind Concerts

Issue [#162](https://github.com/gregwebs/tiny-desk-splitter/issues/162) moves
playback resolution, track navigation, explicit listen recording, and track-like
changes behind `Concerts`. It continues
[Concert Catalog browsing and Concert Status commands](2026-07-25-concert-catalog-and-status.md).

## Changes

- Added presentation-neutral media resolution: `Concerts::playback`,
  `source_media`, `track_media`, `next_track_media`, `prev_track_media`,
  `reconstruction_items`, `track_details`, and `media_file`.
- Added `ConcertPlaybackError`, which separates an absent concert, nothing to
  play, and "recorded as downloaded but the file is gone".
- Moved the source-file decision into `playback::source_file`, so watch,
  media-info, and whole-concert playback all report the missing-file case the
  same way. `playback::track_file` does the same for one track.
- Added explicit `Concerts::record_listen` and `Concerts::record_watch` taking a
  `ListenTarget` (whole concert or one track). Resolving media never records a
  listen; a listen or watch whose media is absent is rejected and records
  nothing.
- Added `Concerts::set_track_liked`, a desired-state command replacing
  `db::split_timestamps::toggle_track_liked`; repeating the state already stored
  records no event.
- Migrated the HTMX playback, media-info, next/prev, watch, listen, like,
  tracks, and track-details handlers to the interface. They now only translate
  requests into interface calls and interface results into JSON/HTML.
- Removed the handler's `locate_full_concert_file` duplicate of the source-file
  policy, the `tracks_busy`/`split_queued` helpers, and the archived
  `tracks_from_events` fallback (unreachable: the projection it replaced always
  had one row per set-list entry).

## Preserved behaviour

Routes, JSON shapes, markup, event names, and event payloads are unchanged. In
particular:

- `POST /concerts/:id/watch` still answers 500 for a concert recorded as
  downloaded whose file is missing, and 404 when there is nothing to download.
- The listen buttons still render their own `success`/`error` state instead of
  an HTTP error when the media is absent.
- `POST /concerts/:id/tracks/:idx/like` still toggles: the markup makes it a
  toggle, so the adapter turns the click into the desired like state.
- Whole-concert listens still record a listen event with no payload; track
  listens still record `track_index` and `track_title`.

## Tests

- `concerts.rs` covers source-vs-reconstruction playback, navigation that skips
  tracks without files, track details probed from persisted state, `media_file`
  resolution for source and track, listen/watch recording with their payloads,
  rejection when media is absent (no event recorded), like-state storage,
  idempotent likes, and typed rejections for absent concerts, absent tracks, and
  unavailable tracks.
- `db/split_timestamps.rs` covers the desired-state like write, its idempotency,
  its events, and out-of-range rejection.
- The black-box HTTP coverage in `hurl/concert_playback.hurl`,
  `hurl/media_info_navigation.hurl`, and `hurl/media_state_errors.hurl` is the
  compatibility test for the migrated routes and status codes.
