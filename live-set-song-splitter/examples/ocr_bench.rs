//! Large OCR benchmark: tesseract (leptess) vs PaddleOCR over real concert data.
//!
//! Five variants per frame, scored through the real parse + match pipeline (`common`):
//!   0 tess(color)  1 tess(full=color+B/W)  2 paddle(color)  3 paddle(bw)  4 paddle(color+bw)
//! (paddle(bw)/paddle(c+b) exist because Paddle's DETECTOR can miss low-contrast title
//! lines on the raw color crop; binarizing recovers detection at some recognition cost.)
//!
//! Datasets:
//! 1. POSITIVES = `analysis/images/*.png` — frames a PRIOR tesseract `--analyze_images`
//!    run already matched (`<phase>_<sanitized_song>_<frame>.png`). Tesseract-discovered,
//!    so tesseract "recall" is ~100% by construction → we measure AGREEMENT / REGRESSION
//!    vs tesseract, plus PER-SONG recall (does ANY frame of a song match — what actually
//!    matters, since the splitter only needs one overlay frame per song).
//! 2. NEGATIVES = sampled non-overlay frames from `temp_frames/<Concert>/` → false-positive
//!    rate (artist-overlay FP + song-match FP), per concert.
//!
//! Artist/setlist ground truth: `testdata/setlists.json` (read-only export of concerts.db),
//! joined to song labels in `normalize_text` space.
//!
//! Run: cargo run --example ocr_bench --features paddle-ocr -- [--limit N] [--neg-per-concert N]

#[path = "common/mod.rs"]
mod common;

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use common::{compact, overlay_detected, song_matched, Engines, Runs};
use live_set_splitter::ocr::normalize_text;

const SETLISTS: &str = "testdata/setlists.json";
const ANALYSIS_DIR: &str = "analysis/images";
const TEMP_FRAMES_DIR: &str = "temp_frames";
const SCRATCH_DIR: &str = "target/ocr_bench_tmp";
const CONCERT_SUFFIX: &str = " - Tiny Desk Concert";
const LIST_CAP: usize = 30;

const NV: usize = 5;
const LABELS: [&str; NV] = [
    "tess(color)",
    "tess(full) ",
    "paddle(clr)",
    "paddle(bw) ",
    "paddle(c+b)",
];

#[derive(Deserialize)]
struct ConcertSet {
    artist: String,
    #[allow(dead_code)]
    album: String,
    songs: Vec<String>,
}

#[derive(Default, Clone, Copy)]
struct Tally {
    n: u32,
    artist: u32,
    song: u32,
}

/// Per-song "did ANY frame of this song …" flags, per variant.
#[derive(Default)]
struct SongRec {
    title: String,      // display title (first seen)
    song: [bool; NV],   // any frame matched the song title
    artist: [bool; NV], // any frame detected the artist overlay (known-artist songs only)
    known_artist: bool,
}

fn main() -> Result<()> {
    env_logger::init();
    let opts = Opts::parse();

    let raw = std::fs::read_to_string(SETLISTS)
        .with_context(|| format!("reading {} (generate it from concerts.db, see plan)", SETLISTS))?;
    let concerts: Vec<ConcertSet> = serde_json::from_str(&raw).context("parsing setlists.json")?;

    let mut song_index: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut artist_songs: HashMap<String, Vec<String>> = HashMap::new();
    for c in &concerts {
        for s in &c.songs {
            let key = normalize_text(s);
            if key.is_empty() {
                continue;
            }
            song_index.entry(key).or_default().push((c.artist.clone(), s.clone()));
        }
        artist_songs.entry(c.artist.clone()).or_default().extend(c.songs.iter().cloned());
    }

    let mut engines = Engines::new(SCRATCH_DIR, opts.paddle_only)?;
    run_positives(&mut engines, &song_index, &opts)?;
    println!();
    run_negatives(&mut engines, &artist_songs, &opts)?;
    Ok(())
}

/// Compute the 5 variant runs for one frame. Returns the runs plus paddle(color) text
/// (used for the negatives mining display).
fn variant_runs(engines: &mut Engines, file: &Path, artist: &str) -> Result<([Runs; NV], String)> {
    let bw = engines.make_bw(file)?;
    let (color, _) = engines.tesseract_runs(&[file], artist)?;
    let (tbw, _) = engines.tesseract_runs(&[bw.as_path()], artist)?;
    let (pc, pc_text) = engines.paddle_runs(file, artist)?;
    let (pbw, _) = engines.paddle_runs(&bw, artist)?;
    let _ = std::fs::remove_file(&bw);

    let mut full = color.clone();
    full.extend(tbw.iter().cloned());
    let mut pboth = pc.clone();
    pboth.extend(pbw.iter().cloned());
    Ok(([color, full, pc, pbw, pboth], pc_text))
}

// ------------------------------- positives -------------------------------

fn run_positives(
    engines: &mut Engines,
    song_index: &HashMap<String, Vec<(String, String)>>,
    opts: &Opts,
) -> Result<()> {
    let mut files = list_pngs(Path::new(ANALYSIS_DIR))?;
    files.sort();
    let total_found = files.len();
    let files = sample(files, opts.limit);

    let mut song_all = [Tally::default(); NV];
    let mut song_initial = [Tally::default(); NV];
    let mut song_refined = [Tally::default(); NV];
    let mut artist_tally = [Tally::default(); NV];
    // Per-song recall: key -> per-variant "did any frame …".
    let mut song_seen: HashMap<String, SongRec> = HashMap::new();

    let (mut n_scored, mut n_skip, mut n_unresolved, mut n_ambiguous) = (0u32, 0u32, 0u32, 0u32);
    // Residual regressions/wins comparing tess(full)=idx1 vs paddle(c+b)=idx4.
    let mut regressions: Vec<(String, String)> = Vec::new();
    let mut paddle_only: Vec<(String, String)> = Vec::new();

    for file in &files {
        let name = file.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let Some((phase, label_song)) = parse_analysis_name(name) else {
            n_skip += 1;
            continue;
        };
        let key = normalize_text(&label_song);
        let (match_song, artist): (String, Option<String>) = match song_index.get(&key) {
            None => {
                n_unresolved += 1;
                (label_song.clone(), None)
            }
            Some(v) if v.len() == 1 => (v[0].1.clone(), Some(v[0].0.clone())),
            Some(_) => {
                n_ambiguous += 1;
                (label_song.clone(), None)
            }
        };
        let parse_artist = artist.as_deref().unwrap_or("");

        let (runs, _) = variant_runs(engines, file, parse_artist)?;

        // Per-variant: artist-overlay detected, and song matched. Song match uses the
        // derived overlay flag when the artist is known (production semantics); for
        // unknown/ambiguous artist we grant the overlay bonus (these ARE overlay frames).
        let mut overlay_hit = [false; NV];
        let mut song_hit = [false; NV];
        for i in 0..NV {
            overlay_hit[i] = overlay_detected(&runs[i]);
            let is_overlay = if artist.is_some() { overlay_hit[i] } else { true };
            song_hit[i] = song_matched(&runs[i], &match_song, is_overlay);
        }

        for i in 0..NV {
            song_all[i].n += 1;
            let bucket = if phase == Phase::Initial { &mut song_initial } else { &mut song_refined };
            bucket[i].n += 1;
            if song_hit[i] {
                song_all[i].song += 1;
                bucket[i].song += 1;
            }
        }
        if artist.is_some() {
            for i in 0..NV {
                artist_tally[i].n += 1;
                if overlay_hit[i] {
                    artist_tally[i].artist += 1;
                }
            }
        }

        let song_key = format!("{}|{}", parse_artist, key);
        let seen = song_seen.entry(song_key).or_default();
        if seen.title.is_empty() {
            seen.title = match_song.clone();
        }
        seen.known_artist |= artist.is_some();
        for i in 0..NV {
            seen.song[i] |= song_hit[i];
            if artist.is_some() {
                seen.artist[i] |= overlay_hit[i];
            }
        }

        // These compare tess(full)=1 vs paddle(c+b)=4; meaningless without tesseract.
        if !opts.paddle_only {
            if song_hit[1] && !song_hit[4] && regressions.len() < LIST_CAP {
                regressions.push((name.to_string(), match_song.clone()));
            }
            if song_hit[4] && !song_hit[1] && paddle_only.len() < LIST_CAP {
                paddle_only.push((name.to_string(), match_song.clone()));
            }
        }
        n_scored += 1;
    }

    // Per-song recall totals.
    let total_songs = song_seen.len() as u32;
    let known_songs = song_seen.values().filter(|r| r.known_artist).count() as u32;
    let mut per_song = [0u32; NV]; // any frame matched the song title
    let mut per_song_artist = [0u32; NV]; // any frame detected the artist overlay
    let mut artist_not_song = [0u32; NV]; // overlay found but song never matched (refine targets)
    for seen in song_seen.values() {
        for i in 0..NV {
            if seen.song[i] {
                per_song[i] += 1;
            }
            if seen.known_artist {
                if seen.artist[i] {
                    per_song_artist[i] += 1;
                }
                if seen.artist[i] && !seen.song[i] {
                    artist_not_song[i] += 1;
                }
            }
        }
    }

    let shown = shown_variants(opts.paddle_only);
    if opts.paddle_only {
        println!("=== POSITIVES (analysis/images): PADDLE-ONLY recall ===");
    } else {
        println!("=== POSITIVES (analysis/images): AGREEMENT / REGRESSION vs tesseract ===");
    }
    println!("  CAVEAT: positive frames were discovered by a prior tesseract run, so tesseract");
    println!("  'recall' here is ~100% by construction; absolute recall is NOT measured.");
    println!(
        "  scored {} of {} frames  (skipped: {} unparseable, {} song-unresolved, {} ambiguous-artist)",
        n_scored, total_found, n_skip, n_unresolved, n_ambiguous
    );
    println!();
    println!("  PER-SONG SONG recall (>=1 frame matched the title) over {} songs:", total_songs);
    for &i in shown {
        println!("    {}   {:>4}/{:<4}  ({:.0}%)", LABELS[i], per_song[i], total_songs, pct(per_song[i], total_songs));
    }
    // Songs missed entirely (0 frames matched) by the key paddle configs.
    let mut miss_clr: Vec<&str> = song_seen.values().filter(|r| !r.song[2]).map(|r| r.title.as_str()).collect();
    let mut miss_cb: Vec<&str> = song_seen.values().filter(|r| !r.song[4]).map(|r| r.title.as_str()).collect();
    miss_clr.sort();
    miss_cb.sort();
    println!("  PER-SONG MISSES paddle(clr): {:?}", miss_clr);
    println!("  PER-SONG MISSES paddle(c+b): {:?}", miss_cb);
    println!();
    println!("  PER-SONG ARTIST-overlay recall (>=1 frame found the overlay) over {} known-artist songs:", known_songs);
    println!("  [the anchor for refinement: if we find the overlay we can refine to read the title]");
    for &i in shown {
        println!(
            "    {}   {:>4}/{:<4}  ({:.0}%)   [overlay-found-but-title-unread: {}]",
            LABELS[i], per_song_artist[i], known_songs, pct(per_song_artist[i], known_songs), artist_not_song[i]
        );
    }
    println!();
    println!("  PER-FRAME song-match (matched / scored):   all          initial      refined");
    for &i in shown {
        println!(
            "    {}   {:>4}/{:<4}   {:>3}/{:<3}     {:>4}/{:<4}",
            LABELS[i], song_all[i].song, song_all[i].n,
            song_initial[i].song, song_initial[i].n, song_refined[i].song, song_refined[i].n,
        );
    }
    println!();
    println!("  artist-overlay detected (over {} frames with known artist):", artist_tally[0].n);
    for &i in shown {
        println!("    {}   {:>4}/{:<4}  ({:.0}%)", LABELS[i], artist_tally[i].artist, artist_tally[i].n, pct(artist_tally[i].artist, artist_tally[i].n));
    }
    // Tesseract-vs-paddle residual comparisons are meaningless without tesseract.
    if !opts.paddle_only {
        println!();
        println!("  RESIDUAL REGRESSIONS — tess(full) matched, paddle(c+b) did NOT (up to {}):", LIST_CAP);
        if regressions.is_empty() {
            println!("    (none in this sample)");
        }
        for (frame, song) in &regressions {
            println!("    {}  ->  {}", frame, song);
        }
        println!("  PADDLE-ONLY — paddle(c+b) matched, tess(full) did NOT (up to {}):", LIST_CAP);
        if paddle_only.is_empty() {
            println!("    (none in this sample)");
        }
        for (frame, song) in &paddle_only {
            println!("    {}  ->  {}", frame, song);
        }
    }
    Ok(())
}

// ------------------------------- negatives -------------------------------

fn run_negatives(
    engines: &mut Engines,
    artist_songs: &HashMap<String, Vec<String>>,
    opts: &Opts,
) -> Result<()> {
    let shown = shown_variants(opts.paddle_only);
    let analysis_hashes = hash_dir(Path::new(ANALYSIS_DIR))?;
    let mut totals = [Tally::default(); NV];
    let mut excluded_overlap = 0u32;
    let mut paddle_candidates: Vec<(String, String)> = Vec::new();

    let shown_labels: Vec<&str> = shown.iter().map(|&i| LABELS[i].trim()).collect();
    println!("=== NEGATIVES (temp_frames sample): FALSE POSITIVES ===");
    println!("  per concert: frames | artist-FP/song-FP for [{}]", shown_labels.join(" "));

    let mut concert_dirs = list_subdirs(Path::new(TEMP_FRAMES_DIR))?;
    concert_dirs.sort();

    for dir in &concert_dirs {
        let dir_name = dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let artist = dir_name.strip_suffix(CONCERT_SUFFIX).unwrap_or(dir_name).to_string();
        let Some(songs) = artist_songs.get(&artist) else {
            println!("  [{}] no setlist in DB for artist {:?}; skipping", dir_name, artist);
            continue;
        };

        let mut frames: Vec<PathBuf> = list_pngs(dir)?.into_iter().filter(|p| is_source_frame(p)).collect();
        frames.sort_by_key(|p| frame_num(p).unwrap_or(u64::MAX));

        let mut candidates = Vec::new();
        for f in frames {
            if analysis_hashes.contains(&hash_file(&f)?) {
                excluded_overlap += 1;
            } else {
                candidates.push(f);
            }
        }
        let sampled = sample(candidates, opts.neg_per_concert);

        let mut t = [Tally::default(); NV];
        for f in &sampled {
            let (runs, pc_text) = variant_runs(engines, f, &artist)?;
            for i in 0..NV {
                t[i].n += 1;
                let overlay = overlay_detected(&runs[i]);
                if overlay {
                    t[i].artist += 1;
                }
                let song_fp = songs.iter().any(|s| song_matched(&runs[i], s, overlay));
                if song_fp {
                    t[i].song += 1;
                }
                // mining on paddle(c+b)=idx4: flagged something on a not-saved frame.
                if i == 4 && (overlay || song_fp) && paddle_candidates.len() < LIST_CAP {
                    let hit = songs
                        .iter()
                        .find(|s| song_matched(&runs[4], s, overlay))
                        .cloned()
                        .unwrap_or_else(|| format!("(artist overlay) {}", compact(&pc_text)));
                    let fname = f.file_name().and_then(|s| s.to_str()).unwrap_or("?");
                    paddle_candidates.push((format!("{}/{}", artist, fname), hit));
                }
            }
        }

        print!("  [{}] {} frames |", artist, t[0].n);
        for &i in shown {
            print!(" {}/{}", t[i].artist, t[i].song);
        }
        println!();
        for i in 0..NV {
            totals[i].n += t[i].n;
            totals[i].artist += t[i].artist;
            totals[i].song += t[i].song;
        }
    }

    println!("  TOTALS (excluded {} frames that md5-match analysis overlays):", excluded_overlap);
    println!("    variant       frames   artist-FP   song-FP");
    for &i in shown {
        println!("    {}   {:>5}    {:>4}       {:>4}", LABELS[i], totals[i].n, totals[i].artist, totals[i].song);
    }
    println!("  PADDLE-ONLY CANDIDATES — paddle(c+b) flagged overlay/song on a frame tesseract did");
    println!("  NOT save (real overlay tesseract missed = paddle win, else paddle FP; up to {}):", LIST_CAP);
    if paddle_candidates.is_empty() {
        println!("    (none in this sample)");
    }
    for (frame, hit) in &paddle_candidates {
        println!("    {}  ->  {}", frame, hit);
    }
    Ok(())
}

// ------------------------------- helpers -------------------------------

fn pct(num: u32, den: u32) -> f64 {
    if den == 0 {
        0.0
    } else {
        100.0 * num as f64 / den as f64
    }
}

struct Opts {
    limit: Option<usize>,
    neg_per_concert: Option<usize>,
    /// Skip the tesseract variants (engine + scoring + tess-vs-paddle comparisons) and
    /// report only the paddle variants — for verifying the production (paddle) path.
    paddle_only: bool,
}

impl Opts {
    fn parse() -> Self {
        let mut limit = None;
        let mut neg_per_concert = Some(100);
        let mut paddle_only = false;
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--limit" => {
                    limit = args.get(i + 1).and_then(|v| v.parse().ok());
                    i += 1;
                }
                "--neg-per-concert" => {
                    neg_per_concert = args.get(i + 1).and_then(|v| v.parse().ok());
                    i += 1;
                }
                "--paddle-only" => paddle_only = true,
                other => eprintln!("ignoring unknown arg: {}", other),
            }
            i += 1;
        }
        Opts { limit, neg_per_concert, paddle_only }
    }
}

/// Variant indices to compute/report. Paddle-only drops tess(color)=0, tess(full)=1.
fn shown_variants(paddle_only: bool) -> &'static [usize] {
    if paddle_only {
        &[2, 3, 4]
    } else {
        &[0, 1, 2, 3, 4]
    }
}

#[derive(PartialEq)]
enum Phase {
    Initial,
    Refined,
}

fn parse_analysis_name(name: &str) -> Option<(Phase, String)> {
    let stem = name.strip_suffix(".png")?;
    let (phase, rest) = if let Some(r) = stem.strip_prefix("initial_") {
        (Phase::Initial, r)
    } else if let Some(r) = stem.strip_prefix("refined_") {
        (Phase::Refined, r)
    } else {
        return None;
    };
    let digits: String = rest.chars().rev().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let song = rest[..rest.len() - digits.len()].trim_end_matches('_');
    if song.is_empty() {
        return None;
    }
    Some((phase, song.to_string()))
}

/// Deterministic strided sample so the subset spreads across the (sorted) input.
fn sample<T>(items: Vec<T>, limit: Option<usize>) -> Vec<T> {
    match limit {
        Some(k) if k > 0 && k < items.len() => {
            let n = items.len();
            items
                .into_iter()
                .enumerate()
                .filter(move |(i, _)| i * k / n != (i + 1) * k / n)
                .map(|(_, x)| x)
                .collect()
        }
        _ => items,
    }
}

fn list_pngs(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading dir {}", dir.display()))? {
        let p = entry?.path();
        if p.extension().and_then(|e| e.to_str()) == Some("png") {
            out.push(p);
        }
    }
    Ok(out)
}

fn list_subdirs(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading dir {}", dir.display()))? {
        let p = entry?.path();
        if p.is_dir() {
            out.push(p);
        }
    }
    Ok(out)
}

fn is_source_frame(p: &Path) -> bool {
    p.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false)
}

fn frame_num(p: &Path) -> Option<u64> {
    p.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse().ok())
}

fn hash_file(p: &Path) -> Result<u64> {
    let bytes = std::fs::read(p).with_context(|| format!("hashing {}", p.display()))?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    Ok(h.finish())
}

fn hash_dir(dir: &Path) -> Result<HashSet<u64>> {
    let mut set = HashSet::new();
    for p in list_pngs(dir)? {
        set.insert(hash_file(&p)?);
    }
    Ok(set)
}
