//! CLI adapter over the [`live_set_splitter::concert_split`] library interface:
//! translates command-line arguments into a [`ConcertSplitRequest`], renders
//! progress to stdout/stderr, and maps the outcome to a process exit code.
//! See `docs/concert-split.md` for the interface and state diagram.

use live_set_splitter::concert_split::{
    self, ConcertSplitOptions, ConcertSplitOutcome, ConcertSplitProgress, ConcertSplitRequest,
    NoOutputReason, OutputFormat, SplitPhase, TrackKind,
};
use live_set_splitter::cut::VideoCutMode;
use live_set_splitter::ocr_backend::OcrChoice;

use concert_types::{ConcertInfo, TimestampsFile};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

/// Tool for splitting live music recordings into individual songs
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Input video file (mp4)
    #[arg(long)]
    input_file: Option<String>,

    concert_file: String,

    /// Don't save individual song files (analysis only)
    #[arg(long)]
    no_save_songs: bool,

    /// Use timestamps from a previously generated JSON file
    #[arg(long)]
    timestamps_file: Option<String>,

    #[arg(long)]
    refine_timestamps: bool,

    /// Output format: video, audio, or both
    #[arg(long, value_enum, default_value_t = OutputFormat::Both)]
    output_format: OutputFormat,

    /// How to cut the video stream: `smart` (frame-accurate at the detected start,
    /// near-copy speed; re-encodes only up to the first keyframe of each track),
    /// `copy` (fastest, lossless; snaps each cut back to the nearest preceding
    /// keyframe so a track can start a few seconds early), or `reencode`
    /// (frame-accurate, re-encodes the whole video, much slower). All keep
    /// audio/video in sync.
    #[arg(long, value_enum, default_value_t = VideoCutMode::Smart)]
    video_cut_mode: VideoCutMode,

    /// Custom output directory for generated audio/video files
    #[arg(long)]
    output_dir: Option<String>,

    /// Save successfully matched images to ./analysis/images directory
    #[arg(long)]
    analyze_images: bool,

    /// Reuse previously extracted frames if they exist
    #[arg(long)]
    reuse_frames: bool,

    /// Keep the extracted temp_frames/ directory after the run instead of deleting it.
    /// Useful for building OCR test data (frames + the --analyze_images matches).
    #[arg(long)]
    keep_frames: bool,

    /// OCR backend: `tesseract` or `paddle`. Defaults to paddle when built with
    /// `--features paddle-ocr`, otherwise tesseract. Choosing a backend that wasn't
    /// compiled in is an error.
    #[arg(long, value_enum)]
    ocr_engine: Option<OcrChoice>,

    /// Cut and save interlude (gap) files for every span between song tracks that
    /// is not covered by a song. Interlude files are named `interlude_NN.mp4|.m4a`
    /// and share the output directory with song tracks. Any previously written
    /// `interlude_NN.*` files in the output directory are removed before writing.
    /// Requires either `--media-duration` or that the source file is present for
    /// ffprobe-based duration detection.
    #[arg(long)]
    emit_interludes: bool,

    /// Total duration of the source media in seconds, used when `--emit-interludes`
    /// is set. When omitted, the splitter ffprobes the input file.
    #[arg(long)]
    media_duration: Option<f64>,
}

/// Translate CLI arguments into a typed [`ConcertSplitRequest`]: parse the
/// concert JSON, resolve the input/output paths, and load
/// `--timestamps-file` if given. Input *validation* (OCR availability,
/// non-empty set list/timestamps, input-file existence) is owned by the
/// library's `run` — this only handles CLI-specific transport concerns.
fn build_request(cli: &Cli) -> Result<ConcertSplitRequest> {
    let concert_path = &cli.concert_file;

    let concert_file = File::open(concert_path)
        .with_context(|| format!("Failed to open setlist file: {}", concert_path))?;
    let concert_reader = BufReader::new(concert_file);
    let concert: ConcertInfo = serde_json::from_reader(concert_reader)
        .with_context(|| format!("Failed to parse setlist JSON from {}", concert_path))?;

    println!("Artist: {}", concert.artist);
    println!("Expected number of songs: {}", concert.set_list.len());
    println!("Songs:");
    for (i, song) in concert.set_list.iter().enumerate() {
        println!("  {}. {}", i + 1, song.title);
    }

    let input_file = match &cli.input_file {
        Some(file) => file.clone(),
        None => {
            if concert.album.is_empty() {
                return Err(anyhow!("No album found in concert metadata file. Please specify a --input-path to the mp4 file for the concert."));
            }
            let album = concert.album.replace(':', "");
            let input_dir = match Path::new(concert_path).parent() {
                Some(dir) => dir.to_str().unwrap(),
                None => ".",
            };
            if input_dir.is_empty() {
                format!("{}.mp4", album)
            } else {
                format!("{}/{}.mp4", input_dir, album)
            }
        }
    };
    println!("Analyzing file: {}", input_file);

    // When `--output-dir` is supplied, use it verbatim — the caller (e.g.
    // concert-tracker) has already computed the per-concert directory. When
    // omitted, default to a sibling directory named after the concert.
    let output_dir = if let Some(custom_dir) = &cli.output_dir {
        println!("Using custom output directory: {}", custom_dir);
        custom_dir.clone()
    } else {
        concert_split::folder_name(&concert)
    };

    let timestamps = if let Some(timestamps_path) = &cli.timestamps_file {
        println!("Reading song timestamps from file: {}", timestamps_path);
        let timestamps_file = File::open(timestamps_path)
            .with_context(|| format!("Failed to open timestamps file: {}", timestamps_path))?;
        let timestamps_reader = BufReader::new(timestamps_file);
        let timestamps_data: TimestampsFile = serde_json::from_reader(timestamps_reader)
            .with_context(|| format!("Failed to parse timestamps JSON from {}", timestamps_path))?;
        println!(
            "Loaded {} song segments from timestamps file",
            timestamps_data.songs.len()
        );
        Some(timestamps_data.songs)
    } else {
        None
    };

    let options = ConcertSplitOptions {
        no_save_songs: cli.no_save_songs,
        refine_timestamps: cli.refine_timestamps,
        output_format: cli.output_format,
        video_cut_mode: cli.video_cut_mode,
        analyze_images: cli.analyze_images,
        reuse_frames: cli.reuse_frames,
        keep_frames: cli.keep_frames,
        ocr_engine: cli.ocr_engine,
        emit_interludes: cli.emit_interludes,
        media_duration: cli.media_duration,
    };

    Ok(ConcertSplitRequest {
        concert,
        input_file: PathBuf::from(input_file),
        output_dir: PathBuf::from(output_dir),
        timestamps,
        options,
    })
}

/// `concert.json` is a byte-for-byte copy of the caller's input file (only
/// written if not already present) — a transport artifact the CLI owns because
/// only it has the original file path; the library only produces
/// `timestamps.json` (its own computed artifact).
fn copy_concert_json(output_dir: &str, concert_path: &str) -> Result<()> {
    let canonical_path = format!("{}/concert.json", output_dir);
    if !Path::new(&canonical_path).exists() {
        std::fs::copy(concert_path, &canonical_path)
            .with_context(|| format!("Failed to copy {} -> {}", concert_path, canonical_path))?;
    }
    Ok(())
}

fn render_progress(event: ConcertSplitProgress) {
    match event {
        ConcertSplitProgress::PhaseStarted(phase) => {
            let label = match phase {
                SplitPhase::Validate => "Validating input",
                SplitPhase::Inspect => "Inspecting source media",
                SplitPhase::Detect => {
                    "Attempting to detect song boundaries using text overlays..."
                }
                SplitPhase::RecoverSilence => {
                    "Text overlay detection missing some songs; extracting audio for silence-based recovery..."
                }
                SplitPhase::RefineAudio => "Refining song boundaries using audio analysis...",
                SplitPhase::WriteMetadata => "Writing timestamps metadata",
                SplitPhase::Cut => "Cutting tracks",
                SplitPhase::ValidateOutput => "Validating split output",
                SplitPhase::Publish => "Publishing split output",
                SplitPhase::Cleanup => "Cleaning up",
            };
            println!("{}", label);
        }
        ConcertSplitProgress::CutPlanned { total } => {
            println!("Processing {} planned track(s)...", total);
        }
        ConcertSplitProgress::TrackCompleted { index, title, kind } => match kind {
            TrackKind::Song => println!("Completed song {}: {}", index, title),
            TrackKind::Interlude => println!("Completed interlude {}: {}", index, title),
        },
        ConcertSplitProgress::Warning(message) => eprintln!("Warning: {}", message),
        ConcertSplitProgress::Diagnostic(message) => println!("{}", message),
    }
}

/// Preserves the CLI's historical exit-code behavior: `Complete` and
/// `NoOutput::AnalysisOnly` mirror past success (exit 0); `NoOutput::NothingDetected`
/// mirrors the past hard error (exit 1). `Partial` is reserved for a later ticket
/// and never produced by `run` today.
fn exit_code_for(outcome: &ConcertSplitOutcome) -> i32 {
    match outcome {
        ConcertSplitOutcome::Complete(_) => 0,
        ConcertSplitOutcome::NoOutput {
            reason: NoOutputReason::AnalysisOnly,
        } => 0,
        ConcertSplitOutcome::NoOutput {
            reason: NoOutputReason::NothingDetected { .. },
        } => 1,
        ConcertSplitOutcome::Partial(_) => 1,
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Fail fast if an explicitly-chosen OCR backend wasn't compiled into this
    // build. `run` re-validates this too (a library caller may skip this CLI),
    // but checking before any file I/O gives the fastest possible failure here.
    if let Some(choice) = cli.ocr_engine {
        live_set_splitter::ocr_backend::ensure_ocr_choice_available(choice)?;
    }

    // Mirrors `run`'s own refine/write condition — only an explicit
    // `--timestamps-file` defaults to skipping the write; embedded timestamps
    // still get a fresh `timestamps.json` (and thus a `concert.json` copy).
    let refine_now = cli.timestamps_file.is_none() || cli.refine_timestamps;
    let concert_path = cli.concert_file.clone();

    let request = build_request(&cli)?;
    let output_dir = request.output_dir.to_string_lossy().to_string();

    let mut sink = render_progress;
    let outcome = concert_split::run(request, &mut sink)?;

    let wrote_metadata = refine_now
        && !matches!(
            outcome,
            ConcertSplitOutcome::NoOutput {
                reason: NoOutputReason::NothingDetected { .. }
            }
        );
    if wrote_metadata {
        copy_concert_json(&output_dir, &concert_path)?;
    }

    match &outcome {
        ConcertSplitOutcome::Complete(_)
        | ConcertSplitOutcome::NoOutput {
            reason: NoOutputReason::AnalysisOnly,
        } => match cli.output_format {
            OutputFormat::Video => println!("Video splitting complete!"),
            OutputFormat::Audio => println!("Audio extraction complete!"),
            OutputFormat::Both => println!("Video and audio extraction complete!"),
        },
        ConcertSplitOutcome::NoOutput { reason } => {
            eprintln!("Error: {}", reason);
        }
        ConcertSplitOutcome::Partial(_) => {
            eprintln!(
                "Error: unexpected Partial outcome (reserved variant not produced by this build)"
            );
        }
    }

    std::process::exit(exit_code_for(&outcome));
}
