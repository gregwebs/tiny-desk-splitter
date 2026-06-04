//! A/B OCR accuracy harness: tesseract (leptess) vs PaddleOCR on a small JSON list of
//! labeled overlay frames, scored through the *real* parse + match pipeline.
//!
//! For each labeled frame we report three variants:
//!   - tess(color): tesseract on the raw color frame (multi-PSM)
//!   - tess(full) : tesseract's production path (color + B/W fallback, multi-PSM)
//!   - paddle     : PaddleOCR, single pass on the raw color frame
//! and whether each (a) detects the artist overlay and (b) matches the song.
//!
//! For the large DB-backed benchmark over analysis/images + temp_frames see `ocr_bench`.
//! The scoring core is shared via `common` so the two can't drift.
//!
//! Needs tesseract installed (for leptess) and the paddle models under `models/`. Run:
//!   cargo run --example ab_ocr --features paddle-ocr -- [cases.json]
//! (default features already include leptess-ocr). Cases file defaults to
//! `testdata/ab_ocr_cases.json`; paths inside are resolved relative to the cwd.

#[path = "common/mod.rs"]
mod common;

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

use common::{compact, score, yn, Engines};

const DEFAULT_CASES: &str = "testdata/ab_ocr_cases.json";
/// Scratch dir (under the build output) for the B/W frames we hand tesseract.
const SCRATCH_DIR: &str = "target/ab_ocr_tmp";

#[derive(Deserialize)]
struct Case {
    file: String,
    artist: String,
    song: String,
}

fn main() -> Result<()> {
    env_logger::init();

    let cases_path = std::env::args().nth(1).unwrap_or_else(|| DEFAULT_CASES.to_string());
    let raw = std::fs::read_to_string(&cases_path)
        .with_context(|| format!("reading cases file {}", cases_path))?;
    let cases: Vec<Case> =
        serde_json::from_str(&raw).with_context(|| format!("parsing cases file {}", cases_path))?;

    let mut engines = Engines::new(SCRATCH_DIR)?;

    // Aggregate (overlay_hits, song_hits) per variant: [tess_color, tess_full, paddle].
    let mut agg = [(0u32, 0u32); 3];
    let labels = ["tess(color)", "tess(full) ", "paddle     "];
    let n = cases.len() as u32;

    for case in &cases {
        let color = PathBuf::from(&case.file);
        if !color.exists() {
            eprintln!("! missing frame, skipping: {}", color.display());
            continue;
        }
        let bw = engines.make_bw(&color)?;

        let (color_runs, color_text) = engines.tesseract_runs(&[color.as_path()], &case.artist)?;
        let (c_overlay, c_song) = score(&color_runs, &case.song);

        let (full_runs, full_text) =
            engines.tesseract_runs(&[color.as_path(), bw.as_path()], &case.artist)?;
        let (f_overlay, f_song) = score(&full_runs, &case.song);

        let (paddle_runs, paddle_text) = engines.paddle_runs(&color, &case.artist)?;
        let (p_overlay, p_song) = score(&paddle_runs, &case.song);

        let outcomes = [
            (c_overlay, c_song, color_text),
            (f_overlay, f_song, full_text),
            (p_overlay, p_song, compact(&paddle_text)),
        ];

        println!(
            "{} — {} / \"{}\"",
            color.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
            case.artist,
            case.song
        );
        for (i, (overlay, song, text)) in outcomes.iter().enumerate() {
            println!("  {}  artist={} song={}  | {}", labels[i], yn(*overlay), yn(*song), text);
            if *overlay {
                agg[i].0 += 1;
            }
            if *song {
                agg[i].1 += 1;
            }
        }
        let _ = std::fs::remove_file(&bw);
        println!();
    }

    println!("SUMMARY  (N = {} frames)", n);
    println!("  variant        artist-overlay   song-matched");
    for (i, label) in labels.iter().enumerate() {
        println!("  {}    {:>3}/{:<3}          {:>3}/{:<3}", label, agg[i].0, n, agg[i].1, n);
    }
    Ok(())
}
