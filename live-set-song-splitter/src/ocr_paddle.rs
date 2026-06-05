//! PaddleOCR-backed [`OcrEngine`] implementation (via the `ocr-rs` crate / `next`
//! branch, which runs PaddleOCR models through MNN).
//!
//! Unlike tesseract, PaddleOCR runs a *detection* model that finds text regions
//! and a *recognition* model that reads each region. There is no page-segmentation
//! mode (PSM) concept, so the per-PSM fan-out used for tesseract collapses to a
//! single pass here. `PaddleBackend` (bottom of this file) is the [`OcrBackend`]
//! production wrapper; `PaddleOcr` is the low-level engine it builds on.
//!
//! The model directory is resolved (see `resolve_model_dir`) in priority order:
//! `$PADDLE_OCR_MODEL_DIR`, then `models/` beside the running executable (survives
//! `cargo install` / a spawned binary with a different cwd), then the crate's source
//! `models/` (dev fallback). The build script downloads the default models into the
//! source `models/` dir; for an installed binary, place `models/` next to it.
//!
//! NOTE on the `image` crate: this module decodes frames with the same `image`
//! crate (0.25) that `ocr-rs` uses, so the `DynamicImage` we hand to the models is
//! the type they expect. We reach it as `::image` because the binary also has a
//! local `crate::image` module that would otherwise shadow the name.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ocr_rs::{DetModel, RecModel};

use crate::ocr::{
    parse_tesseract_output, weights_for_greedy_extractor, weights_for_stingy_extractor, OcrEngine,
};
use crate::ocr_backend::{OcrBackend, OcrBackendOptions, OcrCandidate, OcrPhase};

const MODELS_SUBDIR: &str = "models";
const DET_MODEL: &str = "PP-OCRv5_mobile_det.mnn";
// Recognition model + charset. Default is the general multilingual v5 model: the
// A/B harness showed it reads our overlays fully ("Blue") where the smaller
// English-only model dropped the leading glyph ("lue"), and it also covers
// non-English/accented artist names. Override both together via env (e.g. back to
// the lighter English-only model) without recompiling:
//   PADDLE_OCR_REC_MODEL=en_PP-OCRv5_mobile_rec_infer.mnn PADDLE_OCR_KEYS=ppocr_keys_en.txt
// The charset MUST match the rec model, hence two separate vars.
const DEFAULT_REC_MODEL: &str = "PP-OCRv5_mobile_rec.mnn";
const DEFAULT_KEYS_FILE: &str = "ppocr_keys_v5.txt";

/// Candidate model directories, highest priority first (see module docs). A candidate
/// is only used if it actually contains the detection model.
fn model_dir_candidates() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(d) = std::env::var("PADDLE_OCR_MODEL_DIR") {
        dirs.push(PathBuf::from(d));
    }
    // `models/` beside the running executable: works when the splitter is spawned from
    // another cwd (e.g. by concert-tracker) and after `cargo install` / moving the binary.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.join(MODELS_SUBDIR));
        }
    }
    // Dev fallback: the crate's source `models/` (present for `cargo run`/`cargo build`,
    // and where the build script downloads the models).
    dirs.push(Path::new(env!("CARGO_MANIFEST_DIR")).join(MODELS_SUBDIR));
    dirs
}

/// Pick the first candidate dir that contains the detection model. Pure (testable) over
/// the candidate list and the "does this dir have the model" predicate.
fn pick_model_dir(candidates: &[PathBuf], has_det_model: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    candidates.iter().find(|d| has_det_model(d)).cloned()
}

/// Resolve the model directory, or a clear error listing every path tried.
fn resolve_model_dir() -> Result<PathBuf> {
    let candidates = model_dir_candidates();
    if let Some(dir) = pick_model_dir(&candidates, |d| d.join(DET_MODEL).exists()) {
        return Ok(dir);
    }
    let tried = candidates
        .iter()
        .map(|d| format!("  {}", d.display()))
        .collect::<Vec<_>>()
        .join("\n");
    anyhow::bail!(
        "PaddleOCR models not found (no {DET_MODEL} in any candidate dir). Set \
         PADDLE_OCR_MODEL_DIR, or place a `models/` dir next to the binary. Tried:\n{tried}"
    )
}

// NOTE on detection tuning: we use the library's default DetOptions on purpose.
// A tuning sweep over `DetOptions::with_box_border` (5/10/12/20/100) on the
// blue_back_search frames made recognition WORSE, not better — extra border padding
// pulls the busy concert background into each crop, so the default (5) is near
// optimal. `with_box_threshold` had no observable effect across 0.1..0.8 (it does
// not appear to be plumbed through), so it is not a usable lever. (An earlier
// English-only rec model dropped the leading glyph, "Blue" -> "lue"; switching the
// default to the general v5 model above fixed that at the recognition stage.)

pub struct PaddleOcr {
    det: DetModel,
    rec: RecModel,
    /// Title-crop second pass (ON by default; disable with `PADDLE_OCR_TITLE_CROP=0`).
    /// After the normal pass, crop below the artist line (`min_box_top + frac*height` —
    /// the box TOP is reliable while the bottom bleeds into the title) and re-detect, to
    /// recover a low-contrast title line that the bold artist line otherwise suppresses.
    /// This took paddle to 100% per-song recall on the eval set; `PADDLE_OCR_TITLE_CROP_FRAC`
    /// tunes the fraction (default 0.26). See docs/change/2026-06-04-paddle-ocr-evaluation.md.
    title_crop: bool,
}

impl PaddleOcr {
    pub fn new() -> Result<Self> {
        let dir = resolve_model_dir()?;

        let rec_model =
            std::env::var("PADDLE_OCR_REC_MODEL").unwrap_or_else(|_| DEFAULT_REC_MODEL.to_string());
        let keys_file =
            std::env::var("PADDLE_OCR_KEYS").unwrap_or_else(|_| DEFAULT_KEYS_FILE.to_string());

        let det_path = dir.join(DET_MODEL);
        let rec_path = dir.join(&rec_model);
        let keys_path = dir.join(&keys_file);

        // `None` config = library defaults (CPU backend, default thread count, and
        // default DetOptions — see the tuning note above).
        let det = DetModel::from_file(&det_path, None).map_err(|e| {
            anyhow::anyhow!("loading paddle detection model {}: {}", det_path.display(), e)
        })?;
        let rec = RecModel::from_file(&rec_path, &keys_path, None).map_err(|e| {
            anyhow::anyhow!("loading paddle recognition model {}: {}", rec_path.display(), e)
        })?;

        // Title-crop is ON by default (the 100%-recall config); opt out with =0/false/no.
        let title_crop = std::env::var("PADDLE_OCR_TITLE_CROP")
            .map(|v| !matches!(v.as_str(), "0" | "false" | "no"))
            .unwrap_or(true);

        Ok(Self { det, rec, title_crop })
    }

    /// Detect + recognize every text region in `img`. Returns (top, left, bottom, text)
    /// per non-empty region, in no particular order.
    fn detect_recognize(&mut self, img: &::image::DynamicImage) -> Result<Vec<(i32, i32, i32, String)>> {
        let dets = self
            .det
            .detect_and_crop(img)
            .map_err(|e| anyhow::anyhow!("paddle detection failed: {}", e))?;
        if dets.is_empty() {
            return Ok(Vec::new());
        }
        let crops: Vec<_> = dets.iter().map(|(crop, _)| crop.clone()).collect();
        let results = self
            .rec
            .recognize_batch(&crops)
            .map_err(|e| anyhow::anyhow!("paddle recognition failed: {}", e))?;
        let debug_boxes = std::env::var("PADDLE_OCR_DEBUG_BOXES").is_ok();
        let mut out = Vec::new();
        for ((_, bbox), r) in dets.iter().zip(results.iter()) {
            let text = r.text.trim().to_string();
            if debug_boxes {
                eprintln!(
                    "  box top={:>3} bottom={:>3} h={:>3} left={:>3} text={:?}",
                    bbox.rect.top(),
                    bbox.rect.top() + bbox.rect.height() as i32,
                    bbox.rect.height(),
                    bbox.rect.left(),
                    text
                );
            }
            if !text.is_empty() {
                let top = bbox.rect.top();
                out.push((top, bbox.rect.left(), top + bbox.rect.height() as i32, text));
            }
        }
        Ok(out)
    }
}

impl OcrEngine for PaddleOcr {
    fn ocr_text(&mut self, image_path: &str) -> Result<String> {
        let img = ::image::open(image_path)
            .with_context(|| format!("opening {} for PaddleOCR", image_path))?;

        let mut items = self.detect_recognize(&img)?;

        // Optional title-crop pass: the bold artist line can suppress detection of a
        // fainter title line below it. Crop below the topmost detected box and re-detect
        // the isolated strip, then merge (offsetting strip coords back to image space).
        if self.title_crop {
            // Crop at the artist line's TOP plus an assumed artist-line height. The box
            // top is reliable (~consistent line position) while the box bottom bleeds into
            // the title, so `min_top + frac*height` isolates the title where `below bottom`
            // clipped it. `frac` is the artist line's height as a fraction of the crop.
            if let Some(min_top) = items.iter().map(|(top, ..)| *top).min() {
                let frac = std::env::var("PADDLE_OCR_TITLE_CROP_FRAC")
                    .ok()
                    .and_then(|v| v.parse::<f32>().ok())
                    .unwrap_or(0.26);
                let y = (min_top.max(0) as u32) + (frac * img.height() as f32) as u32;
                if y < img.height() {
                    let strip = img.crop_imm(0, y, img.width(), img.height() - y);
                    for (top, left, bottom, text) in self.detect_recognize(&strip)? {
                        items.push((top + y as i32, left, bottom + y as i32, text));
                    }
                }
            }
        }

        // The downstream parser treats line[0] as the artist candidate, so order regions
        // top-to-bottom (then left-to-right) to recover reading order.
        items.sort_by_key(|(top, left, ..)| (*top, *left));

        // Merge passes: drop duplicate lines (the title may appear in both passes).
        let mut seen = std::collections::HashSet::new();
        let lines: Vec<String> = items
            .into_iter()
            .map(|(_, _, _, text)| text)
            .filter(|t| seen.insert(t.to_lowercase()))
            .collect();

        Ok(lines.join("\n"))
    }
}

/// PaddleOCR [`OcrBackend`]: a single detection+recognition pass per frame (no PSM
/// fan-out, no B/W). For refinement the one parse is offered with both the stingy and
/// greedy match-leniencies (the analog of tesseract's per-PSM weight sweep); detection
/// uses a single candidate.
pub struct PaddleBackend {
    ocr: PaddleOcr,
    phase: OcrPhase,
}

impl PaddleBackend {
    pub fn new(phase: OcrPhase) -> Result<Self> {
        Ok(Self {
            ocr: PaddleOcr::new()?,
            phase,
        })
    }
}

impl OcrBackend for PaddleBackend {
    fn ocr_image_path(&mut self, image_path: &str, artist: &str) -> Vec<Result<OcrCandidate>> {
        let text = match self.ocr.ocr_text(image_path) {
            Ok(text) => text,
            Err(e) => return vec![Err(e)],
        };
        let Some(parse) = parse_tesseract_output(&text, artist) else {
            return Vec::new(); // empty/too-short: no candidate
        };
        match self.phase {
            OcrPhase::Detection => vec![Ok(OcrCandidate {
                parse,
                weights: weights_for_stingy_extractor(),
            })],
            // Refine: try the single parse under both leniencies.
            OcrPhase::Refine => vec![
                Ok(OcrCandidate {
                    parse: parse.clone(),
                    weights: weights_for_stingy_extractor(),
                }),
                Ok(OcrCandidate {
                    parse,
                    weights: weights_for_greedy_extractor(),
                }),
            ],
        }
    }

    fn options(&self) -> OcrBackendOptions {
        OcrBackendOptions {
            black_and_white: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_candidate_with_model_wins() {
        // Highest-priority (e.g. $PADDLE_OCR_MODEL_DIR) is chosen when it has the model.
        let cands = vec![
            PathBuf::from("/env"),
            PathBuf::from("/exe"),
            PathBuf::from("/src"),
        ];
        assert_eq!(pick_model_dir(&cands, |_| true), Some(PathBuf::from("/env")));
    }

    #[test]
    fn falls_through_to_first_dir_that_has_the_model() {
        let cands = vec![
            PathBuf::from("/env"),
            PathBuf::from("/exe"),
            PathBuf::from("/src"),
        ];
        let got = pick_model_dir(&cands, |d| d == Path::new("/src"));
        assert_eq!(got, Some(PathBuf::from("/src")));
    }

    #[test]
    fn none_when_no_candidate_has_the_model() {
        let cands = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        assert_eq!(pick_model_dir(&cands, |_| false), None);
    }
}
