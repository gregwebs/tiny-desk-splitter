# Album "Play" skips a deleted first track

## Problem

The "Play" button next to the track count (the card's tracks-row, `row.html`)
hardcoded track index 0: `Player.playTrack(this, id, 0)`. When the first track
had been deleted, `tracks/0/media-info` 404s, and the button just showed
"Error" instead of playing the concert.

```
WARN concert_tracker::web: request failed method=GET uri=/concerts/630/tracks/0/media-info status=404
```

`delete_track` removes the per-track file (m4a/mp4) from disk, so afterwards
`find_track_file` returns `None` and `track_media_info` returns 404 for that
index — even though later tracks are still present and playable.

## Fix

The album "Play" now starts from the **first track that still exists** rather
than always index 0.

- `concert-tracker/templates/row.html`: the tracks-row Play button calls
  `Player.playTracks(this, id)` (was `playTrack(this, id, 0)`).
- `concert-tracker/static/player.js`:
  - `firstAvailableTrackIndex(concertId)` resolves the starting index: try
    `tracks/0/media-info`; if it 404s, fall back to `tracks/0/next-media-info`,
    which reuses the server's existing `find_next_playable_track` (the same
    skip-deleted logic that drives auto-advance) and returns the resolved
    `track_index`. Returns `null` when the concert has no playable track.
  - `playTracks(btn, concertId)` resolves that index, then delegates to the
    existing `playTrack` (preserving its toggle-pause / enqueue-while-playing
    semantics). With no playable track it shows "Error".

This is scoped to the album Play button. Individual track buttons
(`tracks.html`) keep strict behavior — they are only rendered for available
tracks, and silently substituting a different track for a specific click would
be surprising.

In the common case (track 0 present) `playTracks` makes one extra `media-info`
GET before delegating; acceptable for an explicit button click.

## Why the fixture needed a new concert

The e2e fixture generates `wav`/`webm` media (Chromium can't decode H.264/AAC),
but `delete_track` only removes `m4a`/`mp4`. So "deleting" a fixture track
records the delete event (the track list shows it unavailable) but leaves the
file on disk, and `media-info` still returns 200 — it cannot reproduce the
production 404.

`make_test_fixture.rs` now supports a `present: false` track (`deleted_audio`):
it stays in the set list, gets no file on disk, is marked
`tracks_present[idx] = false`, and records a `track_delete` event — exactly the
post-`delete_track` production state. New concert **id=5 "Deleted-First
Concert"**: `Gone Opener` (deleted), `Survivor One`, `Survivor Two`.

## Verification

`e2e/player-queue.spec.js`: new test "the tracks-row Play button skips a deleted
first track and plays the next" clicks Play on concert 5 and asserts
`Survivor One` (#2) plays. The existing "card's tracks-row Play button plays the
split tracks" test still passes (track 0 present → plays it). `thumbnails.spec`
(iterates every listing card) passes with the added concert. `cargo check`
clean.
