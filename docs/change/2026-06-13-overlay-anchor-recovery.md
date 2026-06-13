# Overlay-anchor recovery for songs with unreadable titles

## Symptom

Auto-splitting the **yeule Tiny Desk Concert** (set list: Dudu, VV, dazies, sulky
baby) produced wrong boundaries: the first track **Dudu ran 0 → 534.9s (8:54)** when
it should end around 4:21. The short second song **"VV" was placed at 534.9s**,
nowhere near its true ~261s start.

## Root cause

Boundaries are detected by OCR'ing the lower-left title overlay of each song
(artist on line 0, title on line 1) and fuzzy-matching the title against the set
list. For VV:

- The artist overlay **"yeule" was correctly detected** at frames 262–265, but
  PaddleOCR read the stylized 2-character title **"VV" as `''` / `w` / `yu`** —
  never "vv".
- The matcher cannot rescue a 2-char title: in `check_line_match`
  (`ocr.rs`), `levenshtein_limit = floor(len/3) = 0` and the hard cap
  `min(line, title)/2 = 0`, i.e. **zero** error tolerance. Loosening it is not an
  option — short titles would then match OCR noise (see the Bloc Party regression
  test `test_short_overlay_noise_does_not_match_short_title`).
- VV was therefore dropped from text detection and handed to
  `recover_missing_songs_from_silence`, which placed it at the loudest-gap silence
  (534.9s) — the wrong boundary.

The key fact: **the title card's location was known** (the artist overlay fired at
frame 262); only the title text was unreadable.

## Fix

Capture frames where the **artist overlay is detected but no title matches**
("unmatched overlay clusters") and use them as the *preferred* boundary anchor for
still-missing songs, ahead of silence-based recovery.

- `detect_song_boundaries_from_text` (`main.rs`) now records such frames and returns
  them (clustered to one earliest timestamp per consecutive run via the pure
  `cluster_overlay_frames`) alongside the segments, via a new `TextDetection` struct.
- `recover_missing_songs_from_silence` was renamed to **`recover_missing_songs`** and
  generalized: within each interior gap it fills missing-song slots from candidates in
  order of preference — **(1) overlay clusters, (2) audio silences, (3) equal-split** —
  using one shared slot-assignment helper (`fill_slots_by_proximity` +
  `candidates_in_gap`) so the silence path is unchanged when no clusters exist.
- An overlay-sourced boundary is marked `start_from_overlay = true`, so it gets the
  same `OVERLAY_DELAY_SECONDS` (3s) audio pullback every detected overlay receives.
  We intentionally do **not** run the frame-accurate refinement pass on recovered
  overlays: the cluster frame + pullback matches how a detected overlay that can't be
  refined earlier is already handled (e.g. dazies 580 → 577).

`check_line_match` is left unchanged except for a comment pointing here as the rescue
path for sub-threshold titles.

## Result

```
Detected 1 unmatched artist-overlay cluster(s) (title unreadable) at: [262.0]
Recovered missing song 'VV' at 262.00s (title overlay, between 'Dudu' and 'dazies')
Segment 1: 0.00s   to 259.00s  - Dudu
Segment 2: 259.00s to 577.00s  - VV
Segment 3: 577.00s to 870.59s  - dazies
Segment 4: 870.59s to 1200.41s - sulky baby
```

Dudu now ends at 259s (4:19) instead of 8:54.

## Recovery candidate preference

```
For each interior gap between two matched songs, per missing slot:

  overlay cluster in gap?  ──yes──▶  anchor here (start_from_overlay = true)
        │ no
        ▼
  audio silence in gap?    ──yes──▶  anchor here (start_from_overlay = false)
        │ no
        ▼
  equal-split the gap                anchor here (start_from_overlay = false)

(spacing ≥ MIN_SONG_GAP_SECONDS from gap endpoints and from already-chosen slots)
```

## Tests

- `cluster_overlay_frames`: run collapsing, run separation, sort/dedup, threshold edge,
  empty/single (`tests_cluster_overlay_frames`).
- `overlay_cluster_preferred_over_silence`: the yeule case in miniature — overlay
  cluster wins over an in-gap silence; recovered song is `start_from_overlay`.
- `overlay_cluster_outside_gap_is_ignored`: out-of-gap cluster falls back to silence.
- `candidates_in_gap_filters_endpoints_and_outside`.
- All pre-existing silence-recovery tests pass unchanged (behavior identical with no
  clusters).

## Known limitation

The per-frame detection loop skips frames within 30s of the last matched song start,
so a missing song whose overlay falls inside that window records no cluster. Songs
shorter than 30s are already unsupported, so this is acceptable.
