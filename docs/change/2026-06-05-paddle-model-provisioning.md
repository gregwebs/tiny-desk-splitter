# PaddleOCR model provisioning + cwd-independent model path

## Problem
With `paddle-ocr` the default feature, splits failed at model load when concert-tracker spawned the
splitter from its own workdir:
```
Error: loading paddle detection model models/PP-OCRv5_mobile_det.mnn: ... No such file or directory
```
Root cause: `ocr_paddle.rs` loaded models from `"models"` — a path relative to the **current working
directory**. The splitter binary inherits concert-tracker's cwd (`jobs/mod.rs` sets no `current_dir`),
so the relative path missed even though the files existed in `live-set-song-splitter/models/`.

## Fix

### Runtime: resolve the model dir by absolute candidates (`src/ocr_paddle.rs`)
`resolve_model_dir()` tries, in priority order, and picks the first dir that actually contains the
detection model:
1. `$PADDLE_OCR_MODEL_DIR` (explicit override),
2. `models/` **beside the running executable** — survives a spawn from another cwd and
   `cargo install` / a moved binary. Mirrors `concert-tracker`'s `default_splitter_bin`, which locates
   the splitter binary the same way.
3. `<CARGO_MANIFEST_DIR>/models` — dev fallback (source tree present for `cargo run`/`cargo build`).

If none has the model, it errors listing every path tried. The pick logic is a pure function with unit
tests for precedence. Verified by running the paddle example from `cwd=/tmp` — models still resolve.

### Build: auto-download models (`live-set-song-splitter/build.rs`)
Gated on `CARGO_FEATURE_PADDLE_OCR` (leptess-only builds do nothing, need no network). Downloads the
three default model files into the source `models/` dir if missing — so a fresh checkout/CI is
provisioned without committing ~24MB of binaries (`models/` stays gitignored). Mirrors the vendored
`ocr-rs` build.rs MNN download.
- **Downloader**: tries curl, then wget (works around a curl with broken TLS), then PowerShell on
  Windows; panics with guidance if all are unavailable.
- **Atomic**: fetches to `<file>.part`, renames on success → a truncated transfer never looks complete.
- **Pinned**: URLs use commit `b7141e7` (same as the vendored crate), not the moving `next` branch, so
  the tuned rec model can't change silently.
- Only ensures the 3 defaults (`PP-OCRv5_mobile_det.mnn`, `PP-OCRv5_mobile_rec.mnn`,
  `ppocr_keys_v5.txt`); never touches/deletes the `en_*` override files.
- Emits only `cargo:rerun-if-changed=build.rs` (provisioning runs on first build; no rebuild loop).
  To force a re-download after deleting a model, `touch build.rs` or `cargo clean`.

## Deployment notes
- `cargo build` / `cargo run` / concert-tracker: fully automatic (build.rs downloads to source
  `models/`; resolver candidate #3 finds it regardless of cwd).
- **`cargo install`**: build.rs can't write to the install dir, so copy `models/` next to the installed
  binary (resolver candidate #2) or set `PADDLE_OCR_MODEL_DIR`.
- Overriding to the English-only model (`PADDLE_OCR_REC_MODEL=en_… PADDLE_OCR_KEYS=…`) requires those
  files present in the resolved dir (not auto-downloaded).
- With `paddle-ocr` as the default feature, the default `cargo build` now needs build-time network
  (model + MNN download) and the MNN toolchain. Build `--no-default-features --features leptess-ocr`
  for an offline, tesseract-only build.
