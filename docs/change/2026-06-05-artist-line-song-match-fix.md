# Stop the artist line from matching a song title

## Problem
Splitting concert 630 (Floetry) failed:
```
Error: Text overlay detection didn't find all songs and silence-based recovery couldn't fill in: Big Ben
```
Setlist: `Big Ben, SupaStar, Butterflies, Say Yes, Getting Late, Floetic`. OCR was fine; the
**song matcher** mis-assigned songs because the artist line was eligible to match a song title,
and the artist **"Floetry"** is Levenshtein-2 from the song **"Floetic"**:

| Frame | OCR lines | Outcome |
|------|-----------|---------|
| 17 | `["Floetry","Big",…]` (artist / *Big Ben*) | artist "Floetry"→"floetic" (lev 2) outranked the true "Big"→"big ben" (starts_with). Segment 1 mislabeled *Floetic* @0s; "floetic" removed from the unmatched list. |
| 1181 | `["Floetry","Floetic","Ptoetry"]` (real *Floetic*) | "floetic" already consumed → only *Big Ben* left to match → no match. |

Net: *Big Ben* never matched, silence recovery had no anchor → the split aborted.

## Root cause
`match_song_titles` (detection) fed **all** OCR lines — including the artist line — to
`matches_song_title`. But `parse_tesseract_output` (`src/ocr.rs`) defines
`is_overlay = fuzzy_match_artist(lines[0], artist)`, so **whenever `is_overlay` is true,
line 0 is the artist by construction** (true for both the tesseract and paddle backends —
both parse through this function). The artist line should never be a song-title candidate.

## Fix
Shared, tested helper in `src/ocr.rs`:
```rust
pub fn song_title_candidate_lines(parse: &OcrParse) -> &[String] {
    let (lines, is_overlay) = parse;
    if *is_overlay && !lines.is_empty() { &lines[1..] } else { &lines[..] }
}
```
For an overlay it drops line 0 (the artist); non-overlay parses have no identified artist
line, so all lines stay eligible. `matches_song_title*` and the fuzzy/overlay bonus are
unchanged — only the candidate lines change.

Used at two places so production and the benchmark can't drift:
- **Detection** — `src/main.rs::match_song_titles` matches against
  `song_title_candidate_lines(ocr_parse)`.
- **Benchmark scorer** — `examples/common/mod.rs::song_matched` applies it per run (each run's
  own overlay flag drives the exclusion).

The **refine path** (`src/main.rs`, the `*overlay || matches_song_title_weighted(...)` check)
is intentionally **unchanged**: it short-circuits on `*overlay` before song matching, and
non-overlay refine frames have no identified artist line, so stripping line 0 there would be
wrong.

### Tradeoff
If OCR ever merges the artist and title onto one physical line, excluding line 0 drops that
title. This is rare for NPR's two-line overlays, and the title-crop preprocessing keeps the
lines separate, so the win (no artist↔song collisions) dominates.

## `--paddle-only` for the OCR benchmarks
`paddle-ocr` is the production default, so the bench tools gained a `--paddle-only` flag to
re-verify matching on the paddle path without the slow tesseract passes:
- `common::Engines::new(scratch, paddle_only)` skips creating the tesseract engines (so
  `tesseract_runs` is a no-op).
- `ocr_bench` / `ab_ocr` report only the paddle variants and skip the tesseract-vs-paddle
  comparison sections.

The examples still require the `leptess-ocr` feature to compile/link (tesseract just isn't run):
```
cargo run --release --example ocr_bench --features leptess-ocr -- --paddle-only
cargo run --release --example ab_ocr   --features leptess-ocr -- --paddle-only
```

## Verification
- `cargo test -p live-set-splitter` — 73 tests pass, incl. four `song_title_candidate_lines`
  tests: artist line excluded → *Big Ben* wins and *Floetic* still matches on its own line;
  non-overlay passthrough; empty-lines safety; and the documented merged-line drop.
- Full-set paddle-only `ocr_bench` over all 2017 `analysis/images` positives (279 songs) +
  500 `temp_frames` negatives:
  ```
  cargo run --release --example ocr_bench --features leptess-ocr -- --paddle-only
  ```
  - PER-SONG SONG recall: paddle(clr) **278/279 (100%)**, paddle(c+b) **278/279 (100%)** —
    identical to the pre-fix figure; lone miss is `"Sing"` (a non-overlay Muppet frame).
  - NEGATIVES song-FP: **2/500**, and both (`Sean Shibe/392`, `Yunchan Lim/719`) are *real*
    overlays paddle correctly read (Thomas Adès / Tchaikovsky titles tesseract had missed),
    not true false positives.
  So excluding the artist line removed the artist↔song collision (Floetry) with **no recall
  regression and no new false positives**.
