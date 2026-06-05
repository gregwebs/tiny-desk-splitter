# Adopt PaddleOCR as a selectable OCR backend (tesseract retained)

Builds on the evaluation (`2026-06-04-paddle-ocr-evaluation.md`) and the vendoring
(`2026-06-04-vendor-ocr-rs.md`). Makes PaddleOCR a runtime-selectable backend while keeping
tesseract the default, since Paddle needs a C/C++ toolchain to build (vendored MNN).

## Abstraction: `OcrBackend` over `OcrEngine`

`OcrEngine` (`ocr_text(path) -> Result<String>`) stays as the low-level per-engine OCR call.
A new higher-level `OcrBackend` (`src/ocr_backend.rs`) owns the OCR fan-out and *declares* its
preprocessing needs, so the splitter pipeline is backend-agnostic:

```
trait OcrBackend {
    fn ocr_image_path(&mut self, image_path, artist) -> Vec<Result<OcrCandidate>>;
    fn options(&self) -> OcrBackendOptions;   // { black_and_white }
}
struct OcrCandidate { parse: OcrParse, weights: LevWeights }
```

- `TesseractBackend` (feature `leptess-ocr`): one `LeptessOcr` per PSM. Phase-built —
  Detection: PSM `[11,None,6]`, `black_and_white = true`; Refine: PSM `[11,None,6,12,10]` each
  paired with its tuned leniency (11/None stingy, 6/12/10 greedy), `black_and_white = false`.
- `PaddleBackend` (feature `paddle-ocr`): one `PaddleOcr` (title-crop ON by default). Detection:
  one candidate; Refine: the single parse offered under both stingy+greedy. `black_and_white = false`.

Selection: `--ocr-engine tesseract|paddle`. Factory `create_ocr_backend(choice, phase)` returns a
`Result` and errors clearly if the chosen backend's feature wasn't compiled. Default: paddle when
built `--features paddle-ocr`, else tesseract. Default cargo build stays tesseract-only (no cmake).

## Detection flow (union-of-passes — preserved exactly)

The detection loop accumulates OCR parses across the color and (tesseract-only) B/W passes into one
collection, then matches the **union** with a single overlay flag. This is load-bearing: when the
color pass finds no artist overlay, the B/W pass's overlay detection still grants the overlay match
bonus to color-pass title lines.

```
 for each frame:
   all_parses = []
   passes = [color] + ([bw] if options.black_and_white)   # paddle => [color] only
   for pass in passes:
     if pass == bw: write_black_and_white(frame)
     all_parses += backend.ocr_image_path(frame_pass, artist)   # propagate first Err
     has_overlay = any(parse.is_overlay for parse in all_parses) # over the UNION
     if not has_overlay and pass == color and options.black_and_white:
         continue                       # try B/W before matching
     for parse in take(all_parses):     # match once; reset for any later pass
         match_song_titles(parse.lines, overlay = has_overlay, ...)
         if overlay match: record; break out of passes
```

Refinement: per frame, `backend.ocr_image_path` once; match each `OcrCandidate` with its own
`weights` (preserving the historical PSM↔weights pairing without a positional zip); track the
earliest matching frame (no per-frame break; the back-search stops when a frame stops matching).

## Notes / removed

- Removed the dead `SubprocessOcr` engine and the cfg-priority `create_ocr_engines`; the runtime
  factory replaces them. `run_tesseract_ocr`/`create_tesseract_instance`/`run_ocr` are retained
  only for the criterion benchmark (`benches/ocr_benchmark.rs`).
- `normalize_text` already folds diacritics (separate fix); unaffected here.

## Tests / verification

- Unit: `TesseractBackend` options are phase-scoped; detection fans out over PSMs and detects the
  artist overlay on a committed B/W fixture (`testdata/blue_back_search/74bw.png`). `cargo test
  -p live-set-splitter` (36 tests) green.
- No-regression bar for tesseract is validated end-to-end: split a vendored video with
  `--ocr-engine tesseract` and confirm `timestamps.json` is unchanged vs. before the refactor;
  then `--ocr-engine paddle` for the new path.
- Build matrix: `cargo build` (default, no cmake) → tesseract; `cargo build --features paddle-ocr`
  → both, default paddle.
