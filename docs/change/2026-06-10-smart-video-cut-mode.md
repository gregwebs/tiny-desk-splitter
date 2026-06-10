# Smart video cut mode: frame-accurate track starts at near-copy speed

## Motivation

The 2026-06-06 sync fix (`docs/change/2026-06-06-video-audio-sync-fix.md`) left a
trade-off in the default `copy` cut mode: each track's cut snaps back to the
preceding keyframe, so a track could begin with up to one GOP (~4s) of the previous
song. `reencode` mode avoids that but re-encodes the entire video.

This change adds a third mode, `smart` (now the **default**), that re-encodes only
the head of each track — from the cut point to the next keyframe, at most one GOP —
and stream-copies everything else.

## Benchmark (Floetry concert, 6 tracks, ~22 min of 1080p24 video)

| mode     | total time | start of track                            |
|----------|-----------:|-------------------------------------------|
| copy     |       1.4s | up to ~4s early (previous song's tail)    |
| smart    |       ~6s  | frame-accurate                            |
| reencode |       192s | frame-accurate                            |

Verified on all 6 tracks: video and audio `start_time = 0`, durations match the
detected track lengths, clean full decode, SSIM 0.96–0.99 against the full
re-encode through the splice region.

## How smart mode cuts a track `[start, end]`

```
probe next keyframe kf >= start (ffprobe -skip_frame nokey -read_intervals)
      |
      |- source not h264 ............................. reencode whole segment
      |- no keyframe in (start, end) .................. reencode whole segment
      |- kf within half a frame of start .............. plain copy cut (already exact)
      |- otherwise:
           head  = re-encode  [start, kf)   video only   (x264, source-matched params)
           tail  = stream-copy [kf, end]    video only
           audio = stream-copy [start, end] exact
           concat head+tail video, mux with audio, apply track metadata
```

Intermediate files live in `<output>.mp4.work/` and are removed afterwards.

### State of the output file

| phase                     | on disk                                   |
|---------------------------|-------------------------------------------|
| during head/tail/audio    | `<output>.mp4.work/` partial pieces       |
| after successful concat   | `<output>.mp4`, work dir removed          |
| on any failure            | error propagated, work dir removed        |

## ffmpeg pitfalls this implementation works around

All of these were hit on the real Floetry source during development; each has a
named constant and/or unit test in `live-set-song-splitter/src/cut.rs`:

1. **Input seeking is DTS-based and can overshoot.** When the cut lands within a
   few frames *before* a keyframe, `-ss <start> -i` jumps onto the keyframe itself
   (B-frame DTS runs ahead of PTS) and the head encodes zero frames. Fix: two-stage
   seek — fast input `-ss` to `HEAD_SEEK_REWIND_SECS` (1s) before the cut, then an
   accurate output-side `-ss` for the remainder.
2. **The audio must not trust the input seek either.** With `-c copy`, an input
   seek rewinds the *demuxer* — audio packets included — to the preceding **video**
   keyframe. Cutting audio that way would bake a multi-second A/V desync into the
   spliced output. Fix: `-copyts` keeps the original timeline so an output-side
   `-ss` at the same absolute time discards exactly the early packets.
3. **`-t`'s end boundary is not exact.** A head window containing no real frames
   (cut directly before a keyframe, e.g. a track starting on the source's first
   frame) still emitted the keyframe, duplicating the tail's first frame. Fix: trim
   the head's `-t` by a quarter frame (`HEAD_END_GUARD_FRAME_FRACTION`); legitimate
   head frames sit at least a full frame before the keyframe. A head that still
   comes out empty is dropped from the concat list (tail-only splice).
4. **The head's container duration can come out short**, sliding the tail back and
   stacking two frames on one timestamp at the splice. Fix: the concat list pins
   the head with an explicit `duration <kf - start>` directive so the tail always
   lands exactly at the keyframe's position on the track timeline.
5. **ffprobe CSV quirks.** Frame lines that carry side data get a trailing comma,
   and `-read_intervals` starts listing at the keyframe *preceding* the seek point.
   `parse_next_keyframe` handles both.
6. **Concat needs stream-compatible parts.** The head is encoded with the source's
   probed properties (`-profile:v`, `-level`, `-pix_fmt`, `-video_track_timescale`;
   `probe_source_video_params`, run once per input). ffprobe reports the integer
   `level_idc` (e.g. `40`), which x264 accepts directly.

## Known limitations

- The NPR sources contain ~20% duplicated frames (near-identical PTS pairs;
  effectively VFR content padded to 24fps). The re-encoded head deduplicates them —
  exactly as full `reencode` mode does — while the stream-copied tail keeps them.
  Net effect: the splice can show a single-frame (≤83ms) presentation gap. Audio is
  continuous throughout.
- `reencode` mode itself can start a track up to ~2 frames late because of pitfall
  1 (measured `v_start=0.083` on a track cut 2 frames before a keyframe). Smart
  mode's two-stage seek avoids this; `reencode` was left unchanged.

## Code

- New module `live-set-song-splitter/src/cut.rs` (also exported from `lib.rs` so
  `cargo test --lib` runs its tests):
  - `VideoCutMode` (+ `Smart` variant, new default), `build_cut_args`, and
    `extract_segment` moved from `main.rs` unchanged apart from a guard rejecting
    `Smart` in the single-command path.
  - Pure, unit-tested planning/arg-building: `plan_smart_cut` → `SmartCutPlan`
    (`CopyWhole` / `ReencodeWhole` / `Spliced`), `build_smart_{head,tail,audio}_args`,
    `build_smart_concat_args`, `build_concat_list`, `concat_list_entry`,
    `parse_next_keyframe`, `x264_profile_for`, `parse_frame_rate`.
  - ffprobe/ffmpeg executors: `probe_source_video_params`, `probe_next_keyframe`,
    `probe_video_frame_count`, `extract_segment_smart`, `splice_segment` (fails
    loudly if the tail has no video frames).
- `process_segments` probes `SourceVideoParams` once per run and dispatches to
  `extract_segment_smart` for smart mode.
- concert-tracker invokes the splitter without `--video-cut-mode`, so it picks up
  the new default with no tracker changes.
- The audio-only `.m4a` path is unchanged.

## Verification

- 19 unit tests in `cut.rs` (`cargo test -p live-set-splitter --lib`).
- End-to-end on the Floetry concert via `--timestamps-file` into a temp output dir
  (real `concerts.db` and source files untouched): all 6 tracks spliced in ~6s,
  in sync, frame-accurate starts, clean decode, SSIM 0.98+ vs `reencode` output.
  Edge cases exercised: track 1 cut on the source's first frame (tail-only splice)
  and a track cut 2 frames before a keyframe (two-stage seek path).
