# Vendored fork of `ocr-rs`

This is a vendored copy of **`ocr-rs`** (PaddleOCR via MNN), used by
`live-set-song-splitter` as a path dependency for the `paddle-ocr` feature.

- **Upstream:** https://github.com/zibo-chen/rust-paddle-ocr — `next` branch, commit `b7141e7`.
- **License:** Apache-2.0 (see `LICENSE`).

## Why vendored

1. Pin the version (upstream is git-only and we depended on a moving branch).
2. Control its `image`/`imageproc` features — upstream pulled `image`'s full default
   codec set (avif/rav1e, exr, tiff, gif, webp), which bloated builds and caused
   exr/rayon/weezl version conflicts across our workspace.

## Local modifications (vs upstream `Cargo.toml`)

- `image = { version = "0.25", default-features = false, features = ["png", "jpeg"] }`
  (was `image = "0.25"`).
- `imageproc = { version = "0.25", default-features = false }` (was `imageproc = "0.25"`).
  imageproc's `default` feature is `["rayon", "image/default"]`, and `image/default`
  re-enables all image codecs — this was the actual source of the avif/rav1e pull.
- Removed upstream `[dev-dependencies]`, `[[example]]` entries, and `[profile.dev]`
  (we vendor only `src/`, `cpp/`, and `build.rs`).

Net effect: `image` resolves to `[png, jpeg]` only; `ravif`/`rav1e`/`exr`/`avif-serialize`
are dropped from the build.

## What is vendored vs generated

Vendored (committed): `src/`, `cpp/` (the C wrapper `build.rs` compiles), `build.rs`,
`Cargo.toml`, `Cargo.lock`, `LICENSE`. NOT vendored: upstream `models/` (we keep our own
under `live-set-song-splitter/models/`), `examples/`, `tests/`, `benches/`, `res/`.

`build.rs` downloads a prebuilt MNN at build time into `3rd_party/prebuilt/` (gitignored,
~22MB) — no cmake/source build on supported platforms (incl. macOS).

## Updating from upstream

Re-copy `src/`, `cpp/`, `build.rs`, `Cargo.toml` from the desired upstream commit, then
re-apply the two dependency edits above (and re-trim the example/profile sections).
