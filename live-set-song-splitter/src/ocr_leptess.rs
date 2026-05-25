use anyhow::Result;
use leptess::{LepTess, Variable};

use crate::ocr::{parse_tesseract_output, OcrEngine, OcrParse};

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

pub fn run_ocr_parse(
    engine: &mut LeptessOcr,
    image_path: &str,
    artist_cmp: &str,
) -> Result<Option<OcrParse>> {
    let text = engine.ocr_text(image_path)?;
    Ok(parse_tesseract_output(&text, artist_cmp))
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
