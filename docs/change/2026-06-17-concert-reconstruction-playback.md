# Concert Reconstruction Playback

## Overview

Once a concert source file is deleted (after splitting), the whole-concert
experience was lost. This change delivers reconstruction playback: stitching the
remaining song + interlude tracks into a seamless sequence when the source is gone.

A **"Play concert"** button is now always present on every downloaded concert card.
It replaces the old clickable Download badge, which becomes a non-clickable status
indicator.

## UX

- **Source file present** → `Play concert` plays the original source file (same as
  before).
- **Source deleted, tracks present** → `Play concert` reconstructs the concert in
  time order from the remaining song + interlude track files.
- **Reconstruction mode sidebar** → interludes appear in the left sidebar alongside
  songs, each with a trash button to delete them individually.
- **Normal per-track play** → interludes are not shown in the sidebar and are
  never auto-advanced to. Unchanged behavior.

## Ordering model

```
merged slots over [0, media_duration], by start_time:
   interlude? · song0 · interlude? · song1 · … · interlude(tail)?

filter for reconstruction:
   song slot i  kept ⟺ tracks_present[i] AND file on disk AND browser-playable
   interlude    kept ⟺ file on disk AND next slot is not a dropped song
                       (tail interlude has no following song → always kept if present)
```

The **deleted-song rule**: the interlude immediately *before* a deleted song is
dropped. The interlude *after* a deleted song is kept. This avoids playing a
transition into a song that no longer exists.

Source: `model::build_reconstruction` (pure function, heavily unit-tested).

## Known limitation: keyframe-snap seam overlap

Smart video cut mode snaps each song's start back to the nearest keyframe. This
means a song track may begin up to one keyframe interval before the beat. When
played in reconstruction sequence, the song overlaps the tail of the interlude
before it by ~1–2 seconds at such seams. Accepted for v1.

## New server endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/concerts/:id/concert-playback` | JSON: `{mode:"source",source:MediaInfo}` or `{mode:"reconstruction",items:[…]}` |
| POST | `/concerts/:id/interludes/:idx/delete` | Delete interlude file, record `interlude_delete` event, return sidebar fragment |
| GET | `/concerts/:id/tracks?playback=concert` | Sidebar fragment with songs + interludes interleaved |

## New event

`Event::InterludeDelete` (`"interlude_delete"`) — distinct from `TrackDelete` to
prevent `tracks_from_events` (which parses `track_index` from every `track_delete`
row to compute deleted-song masks for archived cards) from being corrupted by
interlude index values.

## State contract in player.js

`state.concert = { id, items, pos } | null`

- `play()` sets `state.concert = null` — every non-concert entry path clears
  concert mode.
- `playConcertItem(pos)` saves/restores `state.concert` around `play()` so that
  `play()`'s clearing doesn't lose it.
- A **song** item sets `state.trackIdx = track_index` (per-song like/delete/add
  work for free). An **interlude** item sets `state.trackIdx = null` and is
  highlighted via `[data-interlude-idx]`.
- `skipToNext`, `skipToPrev`, `advanceOrCollapse`, and `onError` branch on
  `state.concert` first.

## Files changed

- `concert-tracker/src/events.rs` — `Event::InterludeDelete` variant
- `concert-tracker/src/model.rs` — `PlaybackItem`, `build_reconstruction`,
  `find_interlude_track_file`; 11 new unit tests
- `concert-tracker/src/web/handlers.rs` — `concert_playback`, `delete_interlude`
  handlers; `can_play_concert` on `RowTemplate`; `compute_can_play_concert`
- `concert-tracker/src/web/mod.rs` — new routes
- `concert-tracker/templates/concert_card.html` — always-on Play concert button;
  badge → non-clickable status
- `concert-tracker/templates/concert_playback_tracks.html` — new sidebar template
- `concert-tracker/static/player.js` — concert mode state, `playConcert`,
  `playConcertItem`, `playConcertFrom`, `advanceConcert`, `sidebarDeleteInterlude`,
  `refreshConcertItems`
