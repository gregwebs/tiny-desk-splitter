# Vendor `ocr-rs` to control its dependencies

Follows the PaddleOCR evaluation (see `2026-06-04-paddle-ocr-evaluation.md`). The
`paddle-ocr` feature depended on `ocr-rs` as a **git dependency on a moving branch**, and
that crate pulled the `image` crate with **default features** — dragging in the full codec
set (avif via `rav1e`, exr, tiff, gif, webp) plus image's `rayon` feature. That bloated
the build (compiling `rav1e`, a large AV1 encoder, for no reason) and caused a cascade of
workspace version conflicts (exr, rayon, weezl).

## Change

Vendored `ocr-rs` (upstream `next` branch, commit `b7141e7`) into `vendor/ocr-rs/` and
switched the dependency from git to a path dependency. `vendor/` is excluded from the
workspace; the prebuilt-MNN build cache (`vendor/ocr-rs/3rd_party/`) is gitignored.

Two dependency edits in the vendored `Cargo.toml` (see `vendor/ocr-rs/VENDORING.md`):
- `image` → `default-features = false, features = ["png", "jpeg"]`.
- `imageproc` → `default-features = false` (its `default` includes `image/default`, which
  was the real source of the avif/rav1e pull — even with the image edit above, imageproc's
  default re-enabled every codec).

## Result

`image` 0.25 now resolves to `[png, jpeg]` only; `ravif`/`rav1e`/`exr`/`avif-serialize`
are gone from the build graph. The vendored crate builds and runs identically (verified:
links MNN, reads overlays incl. the title-crop path). The default (tesseract) build is
unaffected. `gif`/`weezl` remain in the lock but come from plotters' separate `image` 0.24
backend, unrelated to OCR.

Vendoring also pins the exact upstream commit instead of tracking a branch that can move.
