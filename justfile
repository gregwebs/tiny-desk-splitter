# Lint targets
fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

# Default-features clippy (paddle-ocr on; leptess-ocr skipped — needs Tesseract system libs).
# This is the standard lint gate used by the pre-push hook and CI.
clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# Opt-in: also lints the leptess-ocr code path (ocr_leptess.rs + #[cfg(...leptess-ocr)] arms).
# Run this before touching any OCR / leptess backend code.
# Requires Tesseract/leptonica system libraries: brew install tesseract leptonica
clippy-all:
    cargo clippy --workspace --all-targets --features leptess-ocr -- -D warnings

# Run fmt-check + clippy (the full standard lint suite).
lint: fmt-check clippy

# Wire up the version-controlled git hooks (one-time per clone).
install-hooks:
    git config core.hooksPath .githooks
    @echo "Git hooks installed from .githooks/"

# Hot-reload dev server. Requires: cargo install cargo-watch
# Recompiles + restarts on src/templates (incl. inline CSS) changes; on
# static/*.js changes, cargo finds nothing to compile so it just restarts the
# binary, which still triggers a livereload refresh. See --dev in concert_web.rs.
dev *ARGS:
    cargo watch -w concert-tracker/src -w concert-tracker/templates -w concert-tracker/static \
        -x 'run --bin concert-web -- --dev {{ARGS}}'
