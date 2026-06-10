# Fix video/audio desync in split tracks

> **Update 2026-06-10:** a third mode, `smart`, is now the default — it removes
> copy mode's keyframe lead-in at near-copy speed. See
> `2026-06-10-smart-video-cut-mode.md`.

## Symptom

When playing split tracks in concert-web, the **video lagged the audio**. The first
track was in sync, but every later track was progressively off. The desync amount
varied by track.

## Root cause

`extract_segment` in `live-set-song-splitter/src/main.rs` cut each track with:

```
ffmpeg -i INPUT -c copy -ss START -to END ... OUTPUT
```

Because `-ss`/`-to` were placed **after** `-i`, they were *output-side* seeks.
Combined with `-c copy` (stream copy, no re-encode), ffmpeg:

- kept the **audio** from the exact cut point, but
- could only keep the **video** from the next keyframe **at or after** the cut.

The source has a 4-second keyframe interval. So for a track cut mid-GOP, the video
stream started up to ~4s after the audio, baked into the file as a positive video
`start_time`.

### Measured example (Floetry concert)

Keyframes at 0.046, 4.046, 8.046, … (every 4s). "Say Yes" cut at `START=434.337`:

- next keyframe ≥ start = **436.046**
- audio kept from **434.337**
- gap = 436.046 − 434.337 = **1.709s**

…which exactly matched the `video start_time=1.708` measured in the produced file.

| Track | source start | old video lag |
|---|---|---|
| Big Ben | 0.000 (on a keyframe) | ~0.0s (in sync) |
| Butterflies | 258.504 | 1.542s |
| Say Yes | 434.337 | 1.708s |

Track 1 starts at 0.0 (a keyframe), which is why only it was in sync.

## Fix

Seek on the **input** side (`-ss` before `-i`) so audio and video start together.
A new `--video-cut-mode` flag exposes two strategies (both keep A/V in sync):

### `copy` (default) — fast, lossless

```
ffmpeg -ss START -i INPUT -to END -c copy -copyts -avoid_negative_ts make_zero ... OUTPUT
```

- Input seeking snaps the start back to the preceding keyframe.
- `-copyts` keeps the original timeline so `-to` still ends at the true `END`
  (without it, the start snapping back also truncates the song's tail).
- `-avoid_negative_ts make_zero` shifts the first packet to ~0 so both streams begin
  together.

Residual A/V offset measured at **15–22ms** (sub-frame), down from up to 1.7s. The
produced files report container `start_time ≈ 0` (the rebase fully zeroes the
timeline), so concert-web's percentage-based scrubber (`currentTime / duration`) stays
correct.

Trade-off: because the start snaps back to the preceding keyframe, a copy-mode track
may begin up to one GOP (a few seconds) early — its head can contain a few audible
seconds of the previous song's tail. No content is lost (consecutive tracks overlap),
but listeners will hear the lead-in; use `reencode` to avoid it. Tracks whose start
already lands on a keyframe (including track 1 at 0.0) get an exact cut with no
lead-in — this is also why track 1 was the only track in sync under the old bug.

### `reencode` — slow, frame-accurate

```
ffmpeg -ss START -i INPUT -t (END-START) -c:v libx264 -preset veryfast -crf 18 -c:a copy ... OUTPUT
```

- Input seeking lands on the preceding keyframe (fast); ffmpeg then decodes and
  discards up to `START`, so the encoded output begins exactly at the detected cut.
- `-t` (duration) is used rather than `-to` because the accurate seek resets output
  timestamps to 0.

## Code

- `VideoCutMode` enum + `--video-cut-mode` CLI flag (default `copy`).
- `build_cut_args(mode, input, start, end) -> Vec<String>` builds the ordered ffmpeg
  args; unit-tested (`tests_cut_args`) without invoking ffmpeg.
- `extract_segment` now takes the mode and assembles the command from `build_cut_args`.
- The flag is threaded through `process_segments`.

The audio-only (`.m4a`) path was already correct — audio copy has no keyframe
constraint — and is unchanged.

## Verification

- Unit tests: `cargo test -p live-set-splitter tests_cut_args` (4 tests).
- End-to-end on the Floetry concert (output to a temp dir, real `concerts.db` and
  source files untouched):
  - `copy` mode: all 6 tracks show a 15–22ms video/audio `start_time` offset.
  - `reencode` mode: all 6 tracks start exactly at the detected cut, in sync.
