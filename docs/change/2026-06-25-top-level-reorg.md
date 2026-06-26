# Top-level directory tidy-up

Moved three items out of the repository root to reduce clutter and put each next
to its only consumer. No behavioural change to the build, CI, or the scripts.

## What moved

| From | To | Why |
|---|---|---|
| `vendor/` (the `ocr-rs` vendored fork) | `live-set-song-splitter/vendor/` | `ocr-rs` is a path dependency of **only** `live-set-song-splitter`; it's self-contained (no external path deps, not a submodule), so it lives under its consumer. |
| `extract.sh` | `scripts/extract.sh` | Workflow helper script; belongs with the other `scripts/`. |
| `download.sh` (symlink → `scraper/download.sh`) | `scripts/download.sh` (symlink → `../scraper/download.sh`) | Only the top-level convenience symlink moved; the real script stays in `scraper/` (used by `scraper/justfile` and `scraper/README.md`). |

## References updated

- **Path dependency**: `live-set-song-splitter/Cargo.toml` — `path = "../vendor/ocr-rs"`
  → `path = "vendor/ocr-rs"`.
- **Workspace membership / lints**: nesting `ocr-rs` inside the `live-set-song-splitter`
  member makes cargo resolve it as a **workspace member** — a root `exclude` cannot drop a
  member's nested path dependency, and giving `ocr-rs` its own `[workspace]` table errors
  with "multiple workspace roots". So the root `exclude = ["vendor"]` was dropped, and
  `ocr-rs/Cargo.toml` gained `[lints.clippy] all = "allow"` to keep this third-party code
  out of our `clippy -D warnings` gate (it stays `fmt`-clean and adds no tests). The
  workspace `Cargo.lock` gains `ocr-rs`'s optional-dependency nodes (`futures`/`tokio`,
  behind its unused `async` feature) — recorded only, never compiled.
- **gitignore**: the `ocr-rs` build-cache paths (`3rd_party/`, `models/`, `target/`)
  now sit under `live-set-song-splitter/vendor/ocr-rs/`.
- **CI** (`.github/workflows/ci.yml`): the OCR-asset cache `path:` and `hashFiles(...)`
  key point at the new `live-set-song-splitter/vendor/ocr-rs/...` location.
- **`scripts/extract.sh`**: now resolves its sibling `download.sh` via the script's own
  directory (`$(dirname "$0")`) instead of the CWD-relative `./download.sh`, so it works
  no matter where it's invoked from. It still reads/writes `*.json` and `*.mp4` in the CWD.
- Comment-only touch-ups in `Containerfile`, `.dockerignore`, `.containerignore`, and
  `concert-tracker/src/model.rs`.

The container build is unaffected: `Containerfile` does `COPY . .`, so the vendored
crate moves with the build context, and the `target/` ignore still covers the nested
build directory. The canonical vendoring notes live in
`live-set-song-splitter/vendor/ocr-rs/VENDORING.md`, which moved with the directory.
