//! Provision the PaddleOCR model files.
//!
//! When built with the `paddle-ocr` feature, download the default model files into the
//! crate's `models/` dir (gitignored) if they're missing — so a fresh checkout / CI gets
//! them without committing ~24MB of binaries. Mirrors how the vendored `ocr-rs` build.rs
//! fetches prebuilt MNN. Leptess-only builds do nothing here (no network needed).
//!
//! At runtime the binary finds these via `ocr_paddle::resolve_model_dir` (env override ->
//! models/ beside the exe -> this source `models/`). For an installed binary, copy
//! `models/` next to it.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Pinned to the same upstream commit as the vendored `ocr-rs` crate, so the tuned models
/// can't silently change under us.
const MODELS_BASE_URL: &str =
    "https://raw.githubusercontent.com/zibo-chen/rust-paddle-ocr/b7141e7/models";

/// The default model files `PaddleOcr` loads (see `src/ocr_paddle.rs`). The `en_*` override
/// files are NOT auto-provisioned.
const MODEL_FILES: &[&str] = &[
    "PP-OCRv5_mobile_det.mnn",
    "PP-OCRv5_mobile_rec.mnn",
    "ppocr_keys_v5.txt",
];

fn main() {
    // Re-run only when this script changes (never key on the downloaded files — that would
    // loop). First build of the target always runs, which is when provisioning happens.
    println!("cargo:rerun-if-changed=build.rs");

    // Models are only needed for the PaddleOCR backend.
    if std::env::var_os("CARGO_FEATURE_PADDLE_OCR").is_none() {
        return;
    }

    let models_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models");
    std::fs::create_dir_all(&models_dir)
        .unwrap_or_else(|e| panic!("failed to create {}: {e}", models_dir.display()));

    for file in MODEL_FILES {
        let dest = models_dir.join(file);
        if dest.exists() {
            continue;
        }
        let url = format!("{MODELS_BASE_URL}/{file}");
        // NOTE: use eprintln!, not `cargo:warning=` — cargo caches and REPLAYS build-script
        // warnings on every build even when the script doesn't re-run, which made this
        // one-time message appear on every build. eprintln! goes to the build-script log
        // (visible with `cargo build -vv` / when the script actually runs) and isn't replayed.
        eprintln!("downloading PaddleOCR model {file} from {url}");
        download_atomic(&url, &dest);
    }
}

/// Download `url` to `dest` atomically: fetch to `dest` + ".part", then rename on success,
/// so a truncated/interrupted transfer is never mistaken for a complete model file.
fn download_atomic(url: &str, dest: &Path) {
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let _ = std::fs::remove_file(&part); // clear any stale partial

    download_file(url, &part);

    if let Err(e) = std::fs::rename(&part, dest) {
        let _ = std::fs::remove_file(&part);
        panic!("failed to finalize download of {}: {e}", dest.display());
    }
}

/// Fetch `url` to `dest`, trying in turn: curl (`-f` fails on HTTP errors), then wget (a
/// working fallback when a curl install has broken TLS), then PowerShell on Windows.
/// Mirrors `vendor/ocr-rs/build.rs` plus the wget fallback.
fn download_file(url: &str, dest: &Path) {
    let dest_str = dest.to_str().expect("non-utf8 model path");

    if try_status(Command::new("curl").args(["-L", "-f", "-s", "-o", dest_str, url])) {
        return;
    }

    // Fallback: wget. Useful when curl is present but its TLS is broken.
    if try_status(Command::new("wget").args(["-q", "-O", dest_str, url])) {
        return;
    }

    if cfg!(target_os = "windows") {
        let ps = format!("Invoke-WebRequest -Uri '{url}' -OutFile '{dest_str}' -UseBasicParsing");
        if try_status(Command::new("powershell").args(["-NoProfile", "-Command", &ps])) {
            return;
        }
    }

    panic!(
        "failed to download PaddleOCR model from {url}. Ensure `curl` or `wget` is available \
         (and has working TLS), or download it manually to {dest_str}"
    );
}

/// Run a command and report whether it exited successfully (false if it can't be spawned).
fn try_status(cmd: &mut Command) -> bool {
    matches!(cmd.status(), Ok(s) if s.success())
}
