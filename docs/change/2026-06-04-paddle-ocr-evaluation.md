# PaddleOCR evaluation (vs tesseract) for overlay OCR

Status: **evaluation / spike** (not yet wired into the splitter pipeline). Lives behind a
cargo feature so the default build is unchanged.

## Why

Overlay OCR is the core of the splitter. Tesseract is weak on the title overlays, so we
do a lot of compensating work (a multi-PSM sweep + a B/W threshold pass in `image.rs`).
We evaluated `rust-paddle-ocr` (the `ocr-rs` crate, `next` branch — prebuilt MNN, no cmake)
as a replacement.

## What was added (all in `live-set-song-splitter/`)

- `paddle-ocr` cargo feature → `src/ocr_paddle.rs`, a `PaddleOcr` implementing the existing
  `ocr::OcrEngine` trait. Default rec model is the general v5 model; rec model + charset are
  env-overridable (`PADDLE_OCR_REC_MODEL`, `PADDLE_OCR_KEYS`).
  - Experimental `PADDLE_OCR_TITLE_CROP=1` (+ `PADDLE_OCR_TITLE_CROP_FRAC`, default 0.26):
    after the normal pass, crop below the artist line (`min_box_top + frac*height` — the box
    TOP is reliable, the bottom bleeds into the title) and re-detect, to recover faint title
    lines the bold artist line suppresses. `PADDLE_OCR_DEBUG_BOXES=1` dumps box geometry.
- `--keep-frames` CLI flag on the splitter (skips the end-of-run `temp_frames/` cleanup) for
  generating test data.
- `ocr::normalize_text` made `pub`; stray `println!("movement stripped")` → `log::debug!`.
- Benchmark harness: `examples/ocr_bench.rs` (+ shared `examples/common/mod.rs`; `ab_ocr.rs`
  refactored onto it; `examples/paddle_smoke.rs` for raw output).
- Ground truth: `testdata/setlists.json`, generated read-only from `concerts.db` (committed
  for offline repro).

## Results (full run, 2017 confirmed-overlay frames, 279 songs)

PER-SONG recall (≥1 frame of the song matched — the metric that matters; the splitter only
needs one overlay frame per song):

| variant | song recall | artist-overlay recall | per-frame song | false positives |
|---|---|---|---|---|
| tess(color) | 85% | 67% | 1693/2017 | ~0 |
| tess(full, +B/W) | 98% | 87% | 1891/2017 | ~0 |
| paddle(color) | 97% | 94% | 1888/2017 | ~0 |
| paddle(color)+title-crop | **99%** | 94% | 1913/2017 | ~0 |
| paddle(color+bw) | **99%** | 96% | 1926/2017 | ~0 |

Caveats: positives were discovered by a prior tesseract `--analyze_images` run, so this
measures **no-regression vs tesseract** (+ paddle-only finds), not absolute recall. False
positives measured on sampled non-overlay `temp_frames/` (the 2 flagged were leaked real
overlays all engines agreed on). Negatives base is small (6 concerts).

## Conclusion

Adopt PaddleOCR. `paddle(color)+title-crop` reaches 99% per-song recall — beating tesseract's
full pipeline — in a single color pass (no B/W, no PSM sweep), with no false-positive penalty
and far better artist detection. Next: wire Paddle into the pipeline as default and remove the
B/W + multi-PSM scaffolding (separate change, needs engineering-lead review); fork/vendor
`ocr-rs` with `image` `default-features=false` to drop the `rav1e`/codec build bloat.

## Reproducing

The model binaries (~24MB) are gitignored. Fetch them into `live-set-song-splitter/models/`:

```sh
cd live-set-song-splitter/models
base="https://raw.githubusercontent.com/zibo-chen/rust-paddle-ocr/next/models"
for f in PP-OCRv5_mobile_det.mnn PP-OCRv5_mobile_rec.mnn en_PP-OCRv5_mobile_rec_infer.mnn \
         ppocr_keys_v5.txt ppocr_keys_en.txt; do curl -sL -o "$f" "$base/$f"; done
```

Regenerate `testdata/setlists.json` (read-only) if `concerts.db` changes:

```sh
sqlite3 -readonly concerts.db \
 "SELECT json_group_array(json_object('artist',artist,'album',album,'songs',json(set_list_json))) \
  FROM concerts WHERE set_list_json IS NOT NULL AND set_list_json!='' AND artist IS NOT NULL;" \
 > live-set-song-splitter/testdata/setlists.json
```

Run the benchmark (needs tesseract installed; first build downloads prebuilt MNN):

```sh
cd live-set-song-splitter
cargo run --example ocr_bench --features paddle-ocr -- --neg-per-concert 100   # full
cargo run --example ocr_bench --features paddle-ocr -- --limit 300             # quick sample
PADDLE_OCR_TITLE_CROP=1 cargo run --example ocr_bench --features paddle-ocr --   # with title-crop
```
