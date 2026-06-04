//! Smoke test for the PaddleOCR engine: runs OCR on one or more image files and
//! prints the recognized text, so we can eyeball quality vs. tesseract.
//!
//! Build/run (skip tesseract entirely so you don't need it installed):
//!
//!   cargo run --example paddle_smoke --no-default-features --features paddle-ocr -- \
//!     testdata/blue_back_search/73.png testdata/blue_back_search/74.png
//!
//! Models are read from `$PADDLE_OCR_MODEL_DIR` (default `models/`). Run from the
//! `live-set-song-splitter/` directory so the default path resolves.

use live_set_splitter::ocr::OcrEngine;
use live_set_splitter::ocr_paddle::PaddleOcr;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let images: Vec<String> = std::env::args().skip(1).collect();
    if images.is_empty() {
        eprintln!("usage: paddle_smoke <image>...");
        std::process::exit(2);
    }

    let mut engine = PaddleOcr::new()?;
    for path in &images {
        println!("=== {} ===", path);
        match engine.ocr_text(path) {
            Ok(text) if text.is_empty() => println!("<no text detected>"),
            Ok(text) => println!("{}", text),
            Err(e) => eprintln!("error: {:#}", e),
        }
    }
    Ok(())
}
