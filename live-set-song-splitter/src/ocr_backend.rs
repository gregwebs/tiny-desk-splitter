//! Higher-level OCR backend abstraction, layered on the per-engine [`crate::ocr::OcrEngine`].
//!
//! A *backend* owns its OCR fan-out — tesseract runs several page-segmentation (PSM)
//! engines, PaddleOCR runs a single detection+recognition pass — and *declares* the
//! preprocessing the shared pipeline must apply via [`OcrBackend::options`]. This keeps
//! tesseract-specific concepts (PSM, B/W binarization) out of the splitter pipeline so a
//! second backend (Paddle, which needs none of them) can be selected at runtime.
//!
//! Backends are built for a specific [`OcrPhase`] (detection vs. refinement), which fixes
//! their PSM set, per-candidate match weights, and options.

use anyhow::Result;
use stringmetrics::LevWeights;

use crate::ocr::OcrParse;

/// One parsed OCR candidate for a frame, paired with the match-leniency to use for it.
///
/// Refinement matches each candidate with its own `weights` (preserving tesseract's
/// historical PSM↔weights pairing without a positional zip). Detection ignores `weights`
/// and uses the default matcher (`matches_song_title`).
pub struct OcrCandidate {
    pub parse: OcrParse,
    pub weights: LevWeights,
}

/// Preprocessing the pipeline should apply for a backend (phase-scoped).
#[derive(Clone, Copy)]
pub struct OcrBackendOptions {
    /// When true, the pipeline additionally tries a binarized (B/W) pass if the color
    /// pass found no artist overlay. tesseract+Detection only; false for refine and Paddle.
    pub black_and_white: bool,
}

/// Which phase a backend is built for. Affects the PSM set, weights, and options.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OcrPhase {
    Detection,
    Refine,
}

/// Which OCR backend to use. Selectable at runtime via `--ocr-engine`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub enum OcrChoice {
    Tesseract,
    Paddle,
}

pub trait OcrBackend {
    /// OCR a single image path and return parsed candidates (tesseract: one per PSM;
    /// Paddle: one or more). Per-element `Result` so the caller can decide how to handle a
    /// failing engine; the pipeline collects to `Result<Vec<_>>` to propagate the first
    /// error (matching the previous abort-on-error behavior). Empty/too-short parses are
    /// simply absent — candidates carry their own weights, so dropping them is safe.
    fn ocr_image_path(&mut self, image_path: &str, artist: &str) -> Vec<Result<OcrCandidate>>;

    fn options(&self) -> OcrBackendOptions;
}

/// The default backend when `--ocr-engine` is not given: Paddle if it was compiled in
/// (you opted into the heavier build), otherwise tesseract.
pub fn default_ocr_choice() -> OcrChoice {
    #[cfg(feature = "paddle-ocr")]
    {
        OcrChoice::Paddle
    }
    #[cfg(not(feature = "paddle-ocr"))]
    {
        OcrChoice::Tesseract
    }
}

/// Fail fast (at startup) if an explicitly-chosen backend's feature was not compiled in,
/// before any frame extraction. Same gating as [`create_ocr_backend`] but builds nothing.
pub fn ensure_ocr_choice_available(choice: OcrChoice) -> Result<()> {
    match choice {
        OcrChoice::Tesseract => {
            #[cfg(feature = "leptess-ocr")]
            {
                Ok(())
            }
            #[cfg(not(feature = "leptess-ocr"))]
            {
                anyhow::bail!("--ocr-engine tesseract requires building with --features leptess-ocr")
            }
        }
        OcrChoice::Paddle => {
            #[cfg(feature = "paddle-ocr")]
            {
                Ok(())
            }
            #[cfg(not(feature = "paddle-ocr"))]
            {
                anyhow::bail!("--ocr-engine paddle requires building with --features paddle-ocr")
            }
        }
    }
}

/// Build the backend for `choice` + `phase`. Errors clearly (no panic) when the chosen
/// backend's cargo feature was not compiled in.
pub fn create_ocr_backend(choice: OcrChoice, phase: OcrPhase) -> Result<Box<dyn OcrBackend>> {
    match choice {
        OcrChoice::Tesseract => {
            #[cfg(feature = "leptess-ocr")]
            {
                Ok(Box::new(crate::ocr_leptess::TesseractBackend::new(phase)?))
            }
            #[cfg(not(feature = "leptess-ocr"))]
            {
                let _ = phase;
                anyhow::bail!("--ocr-engine tesseract requires building with --features leptess-ocr")
            }
        }
        OcrChoice::Paddle => {
            #[cfg(feature = "paddle-ocr")]
            {
                Ok(Box::new(crate::ocr_paddle::PaddleBackend::new(phase)?))
            }
            #[cfg(not(feature = "paddle-ocr"))]
            {
                let _ = phase;
                anyhow::bail!("--ocr-engine paddle requires building with --features paddle-ocr")
            }
        }
    }
}
