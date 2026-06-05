//! Shared OCR-benchmark primitives used by the `ab_ocr` and `ocr_bench` examples.
//!
//! Included into each example via `#[path = "common/mod.rs"] mod common;` (it is not
//! itself an example target — cargo only auto-builds `examples/*.rs` and subdirs with a
//! `main.rs`). Keeping the scoring core here means the two harnesses can't drift (e.g.
//! `TESS_PSMS`). Both examples enable `leptess-ocr` + `paddle-ocr`, so this module can
//! reference both engines unconditionally.
#![allow(dead_code)] // each example uses a subset of these helpers

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use live_set_splitter::image::write_black_and_white;
use live_set_splitter::ocr::{
    matches_song_title, parse_tesseract_output, song_title_candidate_lines, OcrEngine,
};
use live_set_splitter::ocr_leptess::LeptessOcr;
use live_set_splitter::ocr_paddle::PaddleOcr;

/// Page-segmentation modes the production splitter feeds tesseract for detection.
pub const TESS_PSMS: &[Option<&str>] = &[Some("11"), None, Some("6")];

/// Parsed OCR output: (lines, per-run `is_overlay`) from `parse_tesseract_output`.
pub type Runs = Vec<(Vec<String>, bool)>;

/// Both OCR engines, created once and reused across many frames.
pub struct Engines {
    tess: Vec<LeptessOcr>,
    paddle: PaddleOcr,
    scratch: PathBuf,
}

impl Engines {
    /// Build the tesseract PSM engines + the PaddleOCR engine, and ensure the B/W
    /// scratch dir exists. `scratch_dir` should live under `target/` (gitignored).
    /// When `paddle_only`, the tesseract engines are skipped entirely so `tesseract_runs`
    /// is a fast no-op (returns no runs) — used by the harnesses' `--paddle-only` mode.
    pub fn new(scratch_dir: &str, paddle_only: bool) -> Result<Self> {
        let tess = if paddle_only {
            Vec::new()
        } else {
            TESS_PSMS
                .iter()
                .map(|psm| LeptessOcr::new(*psm))
                .collect::<Result<_>>()
                .context("creating tesseract engines (is tesseract installed?)")?
        };
        let paddle = PaddleOcr::new().context("creating PaddleOCR engine")?;
        std::fs::create_dir_all(scratch_dir)
            .with_context(|| format!("creating scratch dir {}", scratch_dir))?;
        Ok(Self {
            tess,
            paddle,
            scratch: PathBuf::from(scratch_dir),
        })
    }

    /// Run every tesseract PSM engine over each path; returns the parsed runs plus a
    /// compact joined text for display. `artist` only affects the per-run overlay flag.
    pub fn tesseract_runs(&mut self, paths: &[&Path], artist: &str) -> Result<(Runs, String)> {
        let mut runs = Vec::new();
        let mut texts = Vec::new();
        for path in paths {
            let p = path.to_str().context("non-utf8 path")?;
            for engine in self.tess.iter_mut() {
                let text = engine.ocr_text(p)?;
                if let Some(parsed) = parse_tesseract_output(&text, artist) {
                    runs.push(parsed);
                }
                texts.push(text);
            }
        }
        Ok((runs, compact(&texts.join(" ¦ "))))
    }

    /// PaddleOCR single pass over a color frame: parsed runs + compact text.
    pub fn paddle_runs(&mut self, color: &Path, artist: &str) -> Result<(Runs, String)> {
        let text = self
            .paddle
            .ocr_text(color.to_str().context("non-utf8 path")?)?;
        let runs: Runs = parse_tesseract_output(&text, artist).into_iter().collect();
        Ok((runs, compact(&text)))
    }

    /// Threshold a color frame to B/W in the scratch dir; returns the temp path.
    /// Caller may delete it when done.
    pub fn make_bw(&self, color: &Path) -> Result<PathBuf> {
        let stem = color.file_stem().and_then(|s| s.to_str()).unwrap_or("frame");
        // Include parent dir name so identically-numbered frames from different
        // concerts don't collide in the shared scratch dir.
        let parent = color
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let bw = self
            .scratch
            .join(format!("{}_{}_bw.png", sanitize(parent), stem));
        write_black_and_white(color, &bw)?;
        Ok(bw)
    }
}

/// Did any run detect the artist overlay (line[0] fuzzy-matched the artist)?
pub fn overlay_detected(runs: &Runs) -> bool {
    runs.iter().any(|(_, o)| *o)
}

/// Does any run match `song`? `is_overlay` controls the overlay tolerance bonus; each
/// run's own overlay flag drives artist-line exclusion (mirrors the production detection
/// path — `song_title_candidate_lines` drops line 0 when that run detected the artist).
pub fn song_matched(runs: &Runs, song: &str, is_overlay: bool) -> bool {
    runs.iter().any(|run| {
        matches_song_title(song_title_candidate_lines(run), song, is_overlay).is_some()
    })
}

/// Production-semantics score: overlay derived from the runs, then song matched using
/// that overlay flag. Returns (artist_overlay_detected, song_matched).
pub fn score(runs: &Runs, song: &str) -> (bool, bool) {
    let overlay = overlay_detected(runs);
    (overlay, song_matched(runs, song, overlay))
}

/// Collapse whitespace/newlines and truncate for one-line table display.
pub fn compact(s: &str) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > 48 {
        format!("{}…", one.chars().take(48).collect::<String>())
    } else {
        one
    }
}

pub fn yn(b: bool) -> char {
    if b {
        'Y'
    } else {
        '.'
    }
}

/// Minimal filename sanitizer for scratch paths (keep alnum, dash, dot; rest -> '_').
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
        .collect()
}
