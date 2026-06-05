use anyhow::Result;
use leptess::{LepTess, Variable};
use stringmetrics::LevWeights;

use crate::ocr::{
    parse_tesseract_output, weights_for_greedy_extractor, weights_for_stingy_extractor, OcrEngine,
};
use crate::ocr_backend::{OcrBackend, OcrBackendOptions, OcrCandidate, OcrPhase};

/// PSM modes the detection pass feeds tesseract (sparse text / default / uniform block).
/// Tuned for the title overlays; see the OCR evaluation in docs/change.
const DETECTION_PSMS: &[Option<&str>] = &[Some("11"), None, Some("6")];
/// PSM modes for the refinement back-search — a wider sweep than detection.
const REFINE_PSMS: &[Option<&str>] = &[Some("11"), None, Some("6"), Some("12"), Some("10")];

pub struct LeptessOcr {
    lt: LepTess,
}

impl LeptessOcr {
    pub fn new(psm: Option<&str>) -> Result<Self> {
        let mut lt = LepTess::new(None, "eng")
            .map_err(|e| anyhow::anyhow!("Failed to initialize tesseract: {}", e))?;
        suppress_tesseract_logging(&mut lt);

        if let Some(psm_value) = psm {
            lt.set_variable(Variable::TesseditPagesegMode, psm_value)
                .map_err(|_| anyhow::anyhow!("Failed to set PSM to {}", psm_value))?;
        }

        Ok(Self { lt })
    }
}

impl OcrEngine for LeptessOcr {
    fn ocr_text(&mut self, image_path: &str) -> Result<String> {
        self.lt
            .set_image(image_path)
            .map_err(|e| anyhow::anyhow!("Failed to set image {}: {}", image_path, e))?;

        let text = self
            .lt
            .get_utf8_text()
            .map_err(|e| anyhow::anyhow!("Failed to get OCR text: {}", e))?;

        Ok(text)
    }
}

/// Tesseract [`OcrBackend`]: fans out over a phase-specific PSM set, each PSM engine
/// paired with the match-leniency weights to use for its output (refinement). Detection
/// additionally asks the pipeline for a B/W fallback pass via [`OcrBackendOptions`].
pub struct TesseractBackend {
    engines: Vec<LeptessOcr>,
    /// Per-engine match weights, aligned 1:1 with `engines`.
    weights: Vec<LevWeights>,
    options: OcrBackendOptions,
}

impl TesseractBackend {
    pub fn new(phase: OcrPhase) -> Result<Self> {
        let (psms, weights, black_and_white): (&[Option<&str>], Vec<LevWeights>, bool) = match phase
        {
            // Detection uses the default matcher, so weights are placeholders; the B/W
            // fallback pass is enabled here (it is load-bearing for tesseract).
            OcrPhase::Detection => (
                DETECTION_PSMS,
                DETECTION_PSMS
                    .iter()
                    .map(|_| weights_for_stingy_extractor())
                    .collect(),
                true,
            ),
            // Refinement: each PSM is paired with a tuned leniency (11/None strict,
            // 6/12/10 lenient), and there is no B/W pass (operates on the cropped frame).
            OcrPhase::Refine => (
                REFINE_PSMS,
                vec![
                    weights_for_stingy_extractor(), // psm 11
                    weights_for_stingy_extractor(), // psm None (default)
                    weights_for_greedy_extractor(), // psm 6
                    weights_for_greedy_extractor(), // psm 12
                    weights_for_greedy_extractor(), // psm 10
                ],
                false,
            ),
        };
        let engines = psms
            .iter()
            .map(|psm| LeptessOcr::new(*psm))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            engines,
            weights,
            options: OcrBackendOptions { black_and_white },
        })
    }
}

impl OcrBackend for TesseractBackend {
    fn ocr_image_path(&mut self, image_path: &str, artist: &str) -> Vec<Result<OcrCandidate>> {
        let mut out = Vec::new();
        for (engine, weights) in self.engines.iter_mut().zip(self.weights.iter()) {
            match engine.ocr_text(image_path) {
                // Drop None (empty/too-short) parses, as the old per-PSM loop did.
                Ok(text) => {
                    if let Some(parse) = parse_tesseract_output(&text, artist) {
                        out.push(Ok(OcrCandidate {
                            parse,
                            weights: weights.clone(),
                        }));
                    }
                }
                Err(e) => out.push(Err(e)),
            }
        }
        out
    }

    fn options(&self) -> OcrBackendOptions {
        self.options
    }
}

pub fn create_tesseract_instance(psm: Option<&str>) -> Result<LepTess> {
    let mut lt = LepTess::new(None, "eng")
        .map_err(|e| anyhow::anyhow!("Failed to initialize tesseract: {}", e))?;
    suppress_tesseract_logging(&mut lt);

    if let Some(psm_value) = psm {
        lt.set_variable(Variable::TesseditPagesegMode, psm_value)
            .map_err(|_| anyhow::anyhow!("Failed to set PSM to {}", psm_value))?;
    }

    Ok(lt)
}

fn suppress_tesseract_logging(lt: &mut LepTess) {
    if !log::log_enabled!(log::Level::Debug) {
        let _ = lt.set_variable(Variable::DebugFile, "/dev/null");
    }
}

pub fn run_ocr(lt: &mut LepTess, image_path: &str) -> Result<String> {
    lt.set_image(image_path)
        .map_err(|e| anyhow::anyhow!("Failed to set image {}: {}", image_path, e))?;

    let text = lt
        .get_utf8_text()
        .map_err(|e| anyhow::anyhow!("Failed to get OCR text: {}", e))?;

    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr_backend::{OcrBackend, OcrPhase};

    #[test]
    fn options_are_phase_scoped() {
        // The B/W fallback pass is for detection only; refinement never binarizes.
        assert!(
            TesseractBackend::new(OcrPhase::Detection)
                .unwrap()
                .options()
                .black_and_white
        );
        assert!(
            !TesseractBackend::new(OcrPhase::Refine)
                .unwrap()
                .options()
                .black_and_white
        );
    }

    #[test]
    fn detection_fans_out_and_reads_artist_overlay() {
        // Committed fixture: a binarized "Bloc Party / Blue" overlay frame. The detection
        // backend (PSM 11/None/6) should detect the artist overlay on at least one PSM.
        // Needs tesseract + the `eng` traineddata (already required to build/run leptess).
        let mut backend = TesseractBackend::new(OcrPhase::Detection).unwrap();
        let candidates: Vec<_> = backend
            .ocr_image_path("testdata/blue_back_search/74bw.png", "bloc party")
            .into_iter()
            .map(|r| r.expect("tesseract OCR failed (tesseract + eng traineddata installed?)"))
            .collect();
        assert!(!candidates.is_empty(), "expected per-PSM candidates");
        assert!(
            candidates.iter().any(|c| c.parse.1),
            "expected the artist overlay to be detected on the B/W fixture"
        );
    }
}
