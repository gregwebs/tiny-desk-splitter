# Rust linting standardization

Added a standard, enforced Rust linting setup to the workspace.

## What was added

| File | Purpose |
|---|---|
| `rust-toolchain.toml` | Pins Rust to **1.87** (matches `Containerfile`); ensures `rustfmt` + `clippy` components are present |
| `justfile` (root) | Single entry point for lint commands; see below |
| `.githooks/pre-commit` | Runs `cargo fmt --check` on every commit (fast) |
| `.githooks/pre-push` | Runs `just clippy` before any push (gates what leaves the machine) |
| `Cargo.toml` (`[workspace.lints.clippy]`) | Declares `all = "warn"` for IDE/rust-analyzer alignment |
| Each member `Cargo.toml` (`[lints]`) | `workspace = true` so members inherit the policy |

## Standard commands

```sh
just fmt          # cargo fmt --all
just fmt-check    # cargo fmt --all -- --check
just clippy       # cargo clippy --workspace --all-targets -- -D warnings
just clippy-all   # same + --features leptess-ocr  (needs Tesseract/leptonica)
just lint         # fmt-check + clippy  (the full standard suite)
just install-hooks  # one-time: git config core.hooksPath .githooks
```

## Hook design rationale

Clippy on every commit is slow enough to cause `git commit --no-verify` habituation, so
the hooks are split by granularity:
- `pre-commit` → `fmt --check` only (near-instant, high-signal)
- `pre-push` → `just clippy` (runs once per push, mirrors a future CI job, single source of truth)

The pre-push hook delegates to the justfile recipe so the flags aren't duplicated.

## Known limitation: leptess-ocr code path

`--features leptess-ocr` gates real source files (`ocr_leptess.rs`, cfg arms in
`ocr_backend.rs`/`main.rs`/`lib.rs`). The default-features clippy run **never type-checks
this code** — Tesseract/leptonica system libs aren't installed in the standard dev
environment or container. Use `just clippy-all` before touching OCR / leptess backend code.

## One-time hook install

```sh
just install-hooks
```

Requires: `just` (`cargo install just` or `brew install just`).
