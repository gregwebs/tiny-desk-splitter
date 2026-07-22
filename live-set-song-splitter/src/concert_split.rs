//! The synchronous, library-owned Concert Split interface: the full workflow
//! from source media + concert metadata to cut tracks, interludes, and
//! timestamps.
//!
//! `run` owns validation, inspection (ffprobe), text-overlay detection,
//! silence-based recovery, audio-analysis refinement, cutting, output
//! production, and cleanup. Callers pass typed [`ConcertSplitRequest`] data —
//! no temporary JSON transport files — and receive typed [`ConcertSplitProgress`]
//! events plus a structured [`ConcertSplitOutcome`]. The CLI binary (`main.rs`)
//! is a thin adapter: it translates `Cli` arguments into a request, renders
//! progress to stdout/stderr, and maps the outcome to a process exit code.
//!
//! See `docs/concert-split.md` for the phase state diagram.

use crate::detect::{self, Settings};
use crate::ocr_backend::{default_ocr_choice, ensure_ocr_choice_available, OcrChoice};
use crate::produce::{self, CutContext};
use crate::publication::{self, PublicationRequest};
use crate::recover::{self, RecoveryResult};
use crate::refine;
use crate::video::VideoInfo;
use crate::{audio, cut::VideoCutMode, io};
use concert_types::{derive_interludes, interlude_filename_stem, ConcertInfo, Song, SongTimestamp};

use anyhow::{anyhow, Context, Result};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// A song or gap segment on the source media timeline.
#[derive(Clone, Debug)]
pub(crate) struct AudioSegment {
    pub start_time: f64,
    pub end_time: f64,
    pub is_song: bool,
}

/// A detected/recovered/loaded song boundary.
#[derive(Clone, Debug)]
pub(crate) struct SongSegment {
    pub song: Song,
    pub segment: AudioSegment,
    /// True when `segment.start_time` came from detecting the title overlay. The
    /// overlay appears ~`OVERLAY_DELAY_SECONDS` after the song actually starts, so
    /// these (and only these) starts get the overlay-delay pullback when audio
    /// silence can't relocate them. Recovered/silence-placed and JSON-loaded starts
    /// are not overlay estimates and must not be pulled back.
    pub start_from_overlay: bool,
}

/// Output format for extracted segments.
#[derive(clap::Parser, Debug, Clone, Copy, clap::ValueEnum, Default)]
#[clap(rename_all = "lowercase")]
pub enum OutputFormat {
    /// Output video files (mp4)
    Video,
    /// Output audio files (m4a)
    Audio,
    /// Output both video and audio files
    #[default]
    Both,
}

/// Tuning options mirroring the CLI's existing flags 1:1, so behavior stays
/// compatible whether the caller is the CLI adapter or an in-process library
/// caller (e.g. `concert-web`, wired up in a later ticket).
#[derive(Clone, Debug)]
pub struct ConcertSplitOptions {
    pub no_save_songs: bool,
    pub refine_timestamps: bool,
    pub output_format: OutputFormat,
    pub video_cut_mode: VideoCutMode,
    pub analyze_images: bool,
    pub reuse_frames: bool,
    pub keep_frames: bool,
    pub ocr_engine: Option<OcrChoice>,
    pub emit_interludes: bool,
    pub media_duration: Option<f64>,
}

/// Typed input to a Concert Split. `concert` may already carry embedded
/// `timestamps` (loaded as a fallback when `timestamps` below is `None`,
/// mirroring the CLI's "embedded timestamps in the concert JSON" path).
/// `timestamps`, when present, mirrors `--timestamps-file`: it skips detection
/// entirely and (unless `options.refine_timestamps`) skips audio refinement too.
#[derive(Clone, Debug)]
pub struct ConcertSplitRequest {
    pub concert: ConcertInfo,
    pub input_file: PathBuf,
    pub output_dir: PathBuf,
    pub timestamps: Option<Vec<SongTimestamp>>,
    pub options: ConcertSplitOptions,
}

/// Major phases of the workflow, in order (Detect is skipped when timestamps are
/// supplied; RecoverSilence and RefineAudio have their own skip conditions — see
/// `docs/concert-split.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitPhase {
    Validate,
    Inspect,
    Detect,
    RecoverSilence,
    RefineAudio,
    WriteMetadata,
    Cut,
    ValidateOutput,
    Publish,
    Cleanup,
}

/// Kind of track a [`ProducedTrack`] represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    Song,
    Interlude,
}

/// Typed progress emitted as the workflow advances. Adapters choose how to
/// render or persist these.
#[derive(Clone, Debug)]
pub enum ConcertSplitProgress {
    PhaseStarted(SplitPhase),
    /// Emitted at the start of the Cut phase once the track plan is known, so a
    /// consumer can render "k of total" (interludes make the total otherwise
    /// unknown to the caller ahead of time).
    CutPlanned {
        total: usize,
    },
    TrackCompleted {
        index: usize,
        title: String,
        kind: TrackKind,
    },
    Warning(String),
    Diagnostic(String),
}

/// One track (song or interlude) written to `output_dir`.
#[derive(Clone, Debug)]
pub struct ProducedTrack {
    pub title: String,
    pub kind: TrackKind,
    pub start_time: f64,
    pub end_time: f64,
}

/// A complete Concert Split result.
#[derive(Clone, Debug)]
pub struct ConcertSplitOutput {
    pub timestamps: Vec<SongTimestamp>,
    pub tracks: Vec<ProducedTrack>,
    pub output_dir: PathBuf,
}

/// Why a Concert Split produced no output.
#[derive(Clone, Debug)]
pub enum NoOutputReason {
    /// `--no-save-songs`: analysis (and, when applicable, `timestamps.json`)
    /// ran, but no tracks were cut.
    AnalysisOnly,
    /// Text overlay detection and silence-based recovery could not find all
    /// expected songs. Carries the still-missing set-list titles, in set-list
    /// order, so an adapter can reproduce the CLI's historical error message.
    NothingDetected { missing: Vec<String> },
}

impl fmt::Display for NoOutputReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NoOutputReason::AnalysisOnly => {
                write!(f, "analysis only (--no-save-songs); no tracks were cut")
            }
            NoOutputReason::NothingDetected { missing } => write!(
                f,
                "Text overlay detection didn't find all songs and silence-based recovery couldn't fill in: {}",
                missing.join(", ")
            ),
        }
    }
}

/// Domain outcome of a Concert Split, distinct from infrastructure errors
/// (`Err(anyhow::Error)`, e.g. an ffprobe/ffmpeg/IO failure).
#[derive(Clone, Debug)]
pub enum ConcertSplitOutcome {
    /// Every expected set-list track was produced.
    Complete(ConcertSplitOutput),
    /// RESERVED for a later ticket (Recoverable Partial Split publication).
    /// This ticket's workflow is binary — the missing-songs gate in
    /// [`run`] returns `NoOutput` before any cutting starts — so `run` never
    /// constructs this variant today. Kept in the enum now so the seam is
    /// stable for later tickets.
    Partial(ConcertSplitOutput),
    NoOutput {
        reason: NoOutputReason,
    },
}

fn validate_request(request: &ConcertSplitRequest) -> Result<()> {
    if let Some(choice) = request.options.ocr_engine {
        ensure_ocr_choice_available(choice)?;
    }
    if request.concert.set_list.is_empty() {
        return Err(anyhow!("Concert set list is empty"));
    }
    if let Some(ts) = request
        .timestamps
        .as_ref()
        .or(request.concert.timestamps.as_ref())
    {
        if ts.is_empty() {
            return Err(anyhow!("Timestamps file has no timestamps"));
        }
        anyhow::ensure!(
            ts.len() == request.concert.set_list.len()
                && ts
                    .iter()
                    .zip(&request.concert.set_list)
                    .all(|(timestamp, song)| timestamp.title == song.title),
            "Concert Split timestamps do not match the concert set list"
        );
    }
    let mut stems = BTreeSet::new();
    for song in &request.concert.set_list {
        let stem = io::sanitize_filename(&song.title);
        anyhow::ensure!(
            !stem.is_empty(),
            "song title has an empty canonical filename"
        );
        anyhow::ensure!(
            stems.insert(stem.clone()),
            "song titles collide at canonical filename {:?}",
            stem
        );
    }
    if !request.input_file.exists() {
        return Err(anyhow!(
            "Input file does not exist: {}",
            request.input_file.display()
        ));
    }
    Ok(())
}

fn segments_from_timestamps(timestamps: &[SongTimestamp]) -> Vec<SongSegment> {
    timestamps
        .iter()
        .map(|song_timestamp| SongSegment {
            song: Song {
                title: song_timestamp.title.clone(),
            },
            segment: AudioSegment {
                start_time: song_timestamp.start_time,
                end_time: song_timestamp.end_time,
                is_song: true,
            },
            // Loaded from JSON, not a fresh overlay estimate.
            start_from_overlay: false,
        })
        .collect()
}

/// Sanitized directory-name stem derived from a concert's album (or artist, if
/// no album). Shared by the CLI's default output-directory resolution and this
/// module's `temp_frames/` scratch directory so both sides agree on the name.
pub fn folder_name(info: &ConcertInfo) -> String {
    // Strip colons only (matches concert-tracker's sanitize_album so the same
    // directory is referenced from both sides).
    let name = if info.album.is_empty() {
        &info.artist
    } else {
        &info.album
    };
    io::sanitize_filename(&name.replace(':', ""))
}

fn write_timestamps_json(output_dir: &str, concert: &ConcertInfo) -> Result<()> {
    let timestamps_path = format!("{}/timestamps.json", output_dir);
    fs::write(&timestamps_path, serde_json::to_string_pretty(concert)?)
        .with_context(|| format!("Failed to write {}", timestamps_path))?;
    Ok(())
}

fn cleanup_temp_dir(
    temp_dir: &str,
    keep_frames: bool,
    progress: &mut dyn FnMut(ConcertSplitProgress),
) {
    if keep_frames {
        progress(ConcertSplitProgress::Diagnostic(format!(
            "Keeping temporary frames folder (--keep-frames): {}",
            temp_dir
        )));
    } else if Path::new(temp_dir).exists() {
        progress(ConcertSplitProgress::Diagnostic(format!(
            "Cleaning up temporary folder: {}",
            temp_dir
        )));
        match fs::remove_dir_all(temp_dir) {
            Ok(_) => progress(ConcertSplitProgress::Diagnostic(
                "Successfully removed temporary album folder".to_string(),
            )),
            Err(e) => progress(ConcertSplitProgress::Warning(format!(
                "Failed to clean up temporary album folder: {}",
                e
            ))),
        }
    }
}

/// Run a complete Concert Split synchronously. `progress` receives typed events
/// as the workflow advances — the deliberate seam for a later ticket to run this
/// inside `spawn_blocking` and forward events over an mpsc channel captured in
/// the closure, with no `Send`/channel type imposed on this library.
///
/// Infrastructure faults (ffprobe/ffmpeg/IO, and request validation failures)
/// return `Err`. Domain shortfalls — analysis-only runs and songs recovery
/// could not find — return `Ok(ConcertSplitOutcome::NoOutput)`.
pub fn run(
    request: ConcertSplitRequest,
    progress: &mut dyn FnMut(ConcertSplitProgress),
) -> Result<ConcertSplitOutcome> {
    progress(ConcertSplitProgress::PhaseStarted(SplitPhase::Validate));
    validate_request(&request)?;

    let ConcertSplitRequest {
        mut concert,
        input_file,
        output_dir,
        timestamps,
        options,
    } = request;

    let input_file_str = input_file
        .to_str()
        .ok_or_else(|| anyhow!("input file path is not valid UTF-8"))?
        .to_string();
    let staging_parent = output_dir
        .parent()
        .ok_or_else(|| anyhow!("output directory has no parent: {}", output_dir.display()))?;
    fs::create_dir_all(staging_parent).with_context(|| {
        format!(
            "Failed to create staging parent: {}",
            staging_parent.display()
        )
    })?;
    let staging = tempfile::Builder::new()
        .prefix(".concert-split-staging-")
        .tempdir_in(staging_parent)
        .context("Failed to create Concert Split staging directory")?;
    let staging_dir = staging.path().to_path_buf();
    let output_dir_str = staging_dir
        .to_str()
        .ok_or_else(|| anyhow!("staging directory path is not valid UTF-8"))?
        .to_string();

    let num_songs = concert.set_list.len();

    progress(ConcertSplitProgress::PhaseStarted(SplitPhase::Inspect));
    let video_info = VideoInfo::from_ffprobe_file(&input_file_str)
        .with_context(|| format!("Failed to get video information from {}", input_file_str))?;

    // If timestamps were supplied (explicitly, or embedded in the concert JSON),
    // load segments from them instead of detecting. Mirrors the CLI's two
    // sources: `--timestamps-file` (here: `timestamps`) takes precedence over
    // timestamps embedded in the concert metadata itself.
    let mut segments: Vec<SongSegment> = Vec::new();
    let mut overlay_clusters: Vec<f64> = Vec::new();
    if let Some(ts) = &timestamps {
        segments = segments_from_timestamps(ts);
    } else if let Some(ts) = &concert.timestamps {
        segments = segments_from_timestamps(ts);
    }

    io::ensure_dir("temp_frames")?;
    let temp_dir = format!("temp_frames/{}", folder_name(&concert));
    io::ensure_dir(&temp_dir)?;

    if segments.is_empty() {
        progress(ConcertSplitProgress::PhaseStarted(SplitPhase::Detect));
        let settings = Settings {
            analyze_images: options.analyze_images,
            reuse_frames: options.reuse_frames,
            ocr_choice: options.ocr_engine.unwrap_or_else(default_ocr_choice),
        };
        let detection = detect::detect_song_boundaries_from_text(
            &input_file_str,
            &concert.artist,
            &concert.set_list,
            &video_info,
            &settings,
            &temp_dir,
            progress,
        )?;
        segments = detection.segments;
        overlay_clusters = detection.unmatched_overlay_clusters;
    }

    // Cache for the audio waveform — extracted at most once, regardless of
    // whether silence-based recovery and/or refinement need it.
    let mut audio_data: Option<Vec<f32>> = None;

    // If text detection came up short, try silence-based recovery before giving up.
    if segments.iter().filter(|s| s.segment.is_song).count() < num_songs {
        progress(ConcertSplitProgress::PhaseStarted(
            SplitPhase::RecoverSilence,
        ));
        let waveform = audio::extract_audio_waveform(&input_file_str)
            .with_context(|| format!("Failed to extract audio waveform from {}", input_file_str))?;
        let results = recover::recover_missing_songs(
            &mut segments,
            &concert.set_list,
            &overlay_clusters,
            &waveform,
            progress,
        );
        audio_data = Some(waveform);

        let still_missing: Vec<String> = concert
            .set_list
            .iter()
            .zip(results.iter())
            .filter_map(|(song, result)| {
                if *result == RecoveryResult::StillMissing {
                    Some(song.title.clone())
                } else {
                    None
                }
            })
            .collect();

        if !still_missing.is_empty() {
            // No cleanup here: mirrors the original CLI's hard error at this
            // point, which returned before reaching cleanup.
            return Ok(ConcertSplitOutcome::NoOutput {
                reason: NoOutputReason::NothingDetected {
                    missing: still_missing,
                },
            });
        }
    }

    // Only an explicit `timestamps` (mirroring `--timestamps-file`) defaults to
    // skipping refinement; embedded concert.timestamps still refines, matching
    // the CLI's exact condition (`cli.timestamps_file.is_none() || cli.refine_timestamps`).
    let refine_now = timestamps.is_none() || options.refine_timestamps;
    if refine_now {
        progress(ConcertSplitProgress::PhaseStarted(SplitPhase::RefineAudio));
        let audio_samples = match audio_data.take() {
            Some(w) => w,
            None => audio::extract_audio_waveform(&input_file_str).with_context(|| {
                format!("Failed to extract audio waveform from {}", input_file_str)
            })?,
        };
        segments = refine::refine_segments_with_audio_analysis(
            &segments,
            &audio_samples,
            video_info.duration,
            progress,
        )
        .with_context(|| "Failed to refine segments with audio analysis")?;
        segments = refine::refine_last_song_end_time(
            &input_file_str,
            segments,
            video_info.duration,
            options.reuse_frames,
            &temp_dir,
            progress,
        )
        .with_context(|| "Failed to refine last song end time")?;
    }

    // Outcome timestamps are always computed (so a library caller gets them
    // without reading a file), but `timestamps.json` is written only under the
    // same condition the CLI historically used — writing it unconditionally
    // would add a file reset-to-auto/user-timestamp runs never wrote before.
    let outcome_timestamps = produce::create_song_timestamps(&segments, &concert.set_list);
    concert.timestamps = Some(outcome_timestamps.clone());

    if refine_now {
        progress(ConcertSplitProgress::PhaseStarted(
            SplitPhase::WriteMetadata,
        ));
        write_timestamps_json(&output_dir_str, &concert)?;
    }

    let mut tracks: Vec<ProducedTrack> = Vec::new();
    if !options.no_save_songs {
        progress(ConcertSplitProgress::PhaseStarted(SplitPhase::Cut));
        // Resolve the media duration needed for interlude derivation. Prefer the
        // explicit `media_duration` option (avoids a second ffprobe), fall back
        // to the duration already obtained above.
        let resolved_media_duration = if options.emit_interludes {
            options.media_duration.unwrap_or(video_info.duration)
        } else {
            0.0 // unused when emit_interludes is false
        };

        // Smart cutting probes the source's stream properties once for the run.
        let source_params = match (options.output_format, options.video_cut_mode) {
            (OutputFormat::Video | OutputFormat::Both, VideoCutMode::Smart) => {
                Some(crate::cut::probe_source_video_params(&input_file_str)?)
            }
            _ => None,
        };
        let ctx = CutContext {
            input_file: &input_file_str,
            output_dir: &output_dir_str,
            output_format: options.output_format,
            source_params,
            video_cut_mode: options.video_cut_mode,
            concert: &concert,
        };
        tracks = produce::process_segments(
            &segments,
            &concert,
            ctx,
            options.emit_interludes,
            resolved_media_duration,
            progress,
        )?;
    }

    let mut replacement_files = Vec::new();
    if refine_now {
        replacement_files.push(PathBuf::from("timestamps.json"));
    }
    for track in &tracks {
        let stem = match track.kind {
            TrackKind::Song => io::sanitize_filename(&track.title),
            TrackKind::Interlude => track.title.clone(),
        };
        match options.output_format {
            OutputFormat::Video => replacement_files.push(PathBuf::from(format!("{stem}.mp4"))),
            OutputFormat::Audio => replacement_files.push(PathBuf::from(format!("{stem}.m4a"))),
            OutputFormat::Both => {
                replacement_files.push(PathBuf::from(format!("{stem}.mp4")));
                replacement_files.push(PathBuf::from(format!("{stem}.m4a")));
            }
        }
    }
    if !replacement_files.is_empty() {
        progress(ConcertSplitProgress::PhaseStarted(
            SplitPhase::ValidateOutput,
        ));
        let produced_songs: Vec<&str> = tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Song)
            .map(|track| track.title.as_str())
            .collect();
        let expected_songs: Vec<&str> = concert
            .set_list
            .iter()
            .map(|song| song.title.as_str())
            .collect();
        if !options.no_save_songs {
            anyhow::ensure!(
                produced_songs == expected_songs,
                "produced song set does not match the concert set list"
            );
        }
        anyhow::ensure!(
            outcome_timestamps.len() == concert.set_list.len()
                && outcome_timestamps
                    .iter()
                    .zip(&concert.set_list)
                    .all(|(timestamp, song)| timestamp.title == song.title),
            "Concert Split timestamps do not match the concert set list"
        );
        let expected_interludes: Vec<String> = if options.emit_interludes {
            derive_interludes(
                &outcome_timestamps,
                options.media_duration.unwrap_or(video_info.duration),
            )
            .iter()
            .map(|interlude| interlude_filename_stem(interlude.index))
            .collect()
        } else {
            Vec::new()
        };
        let produced_interludes: Vec<&str> = tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Interlude)
            .map(|track| track.title.as_str())
            .collect();
        if !options.no_save_songs {
            anyhow::ensure!(
                produced_interludes
                    == expected_interludes
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                "produced interlude set does not match the expected timeline gaps"
            );
        }
        progress(ConcertSplitProgress::PhaseStarted(SplitPhase::Publish));
        publication::publish(&PublicationRequest {
            canonical_dir: output_dir.clone(),
            staging_dir: staging_dir.clone(),
            replacement_files,
        })?;
    }

    progress(ConcertSplitProgress::PhaseStarted(SplitPhase::Cleanup));
    cleanup_temp_dir(&temp_dir, options.keep_frames, progress);

    let outcome = if options.no_save_songs {
        ConcertSplitOutcome::NoOutput {
            reason: NoOutputReason::AnalysisOnly,
        }
    } else {
        ConcertSplitOutcome::Complete(ConcertSplitOutput {
            timestamps: outcome_timestamps,
            tracks,
            output_dir,
        })
    };
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use concert_types::Musician;
    use std::path::Path;

    /// `album` must be unique per test: `run` derives its `temp_frames/<album>`
    /// scratch directory from it, and tests run concurrently in the same
    /// process/working directory, so a shared album name races (one test's
    /// cleanup can delete another's in-flight scratch directory).
    fn test_concert(album: &str, set_list_titles: &[&str]) -> ConcertInfo {
        ConcertInfo {
            artist: "Test Artist".to_string(),
            source: String::new(),
            show: String::new(),
            date: None,
            album: album.to_string(),
            description: None,
            set_list: set_list_titles
                .iter()
                .map(|t| Song {
                    title: t.to_string(),
                })
                .collect(),
            musicians: Vec::<Musician>::new(),
            preview_image_url: None,
            teaser: None,
            timestamps: None,
        }
    }

    fn default_options() -> ConcertSplitOptions {
        ConcertSplitOptions {
            no_save_songs: false,
            refine_timestamps: false,
            output_format: OutputFormat::Audio,
            video_cut_mode: VideoCutMode::Copy,
            analyze_images: false,
            reuse_frames: false,
            keep_frames: false,
            ocr_engine: None,
            emit_interludes: false,
            media_duration: None,
        }
    }

    /// A short real media fixture (silent-overlay `testsrc` video + a continuous
    /// sine tone, no silence and no title-overlay text) generated once per test
    /// process via `ffmpeg -f lavfi`, matching the spec's "small real-media
    /// suite" — the Inspect phase's ffprobe runs unconditionally even when
    /// timestamps are supplied, so these tests cannot avoid real ffmpeg/ffprobe.
    fn fixture(dir: &Path) -> PathBuf {
        let path = dir.join("fixture.mp4");
        let duration = "8";
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc=duration={duration}:size=320x240:rate=25"),
                "-f",
                "lavfi",
                "-i",
                &format!("sine=frequency=440:duration={duration}"),
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
                path.to_str().unwrap(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("failed to spawn ffmpeg to build the test fixture");
        assert!(status.success(), "ffmpeg failed to build the test fixture");
        path
    }

    fn no_progress(_event: ConcertSplitProgress) {}

    // --- Validation (run()'s Validate phase; no ffmpeg/ffprobe needed) ---

    #[test]
    fn empty_set_list_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let request = ConcertSplitRequest {
            concert: test_concert("EmptySetList", &[]),
            input_file: dir.path().join("missing.mp4"),
            output_dir: dir.path().join("out"),
            timestamps: None,
            options: default_options(),
        };
        let err = run(request, &mut no_progress).unwrap_err();
        assert!(err.to_string().contains("set list is empty"));
    }

    #[test]
    fn empty_explicit_timestamps_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let request = ConcertSplitRequest {
            concert: test_concert("EmptyTimestamps", &["Intro"]),
            input_file: dir.path().join("missing.mp4"),
            output_dir: dir.path().join("out"),
            timestamps: Some(Vec::new()),
            options: default_options(),
        };
        let err = run(request, &mut no_progress).unwrap_err();
        assert!(err
            .to_string()
            .contains("Timestamps file has no timestamps"));
    }

    #[test]
    fn missing_input_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let request = ConcertSplitRequest {
            concert: test_concert("MissingInputFile", &["Intro"]),
            input_file: dir.path().join("does-not-exist.mp4"),
            output_dir: dir.path().join("out"),
            timestamps: Some(vec![SongTimestamp {
                title: "Intro".to_string(),
                start_time: 0.0,
                end_time: 4.0,
                duration: 4.0,
            }]),
            options: default_options(),
        };
        let err = run(request, &mut no_progress).unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    #[cfg(not(feature = "leptess-ocr"))]
    fn uncompiled_ocr_backend_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut options = default_options();
        options.ocr_engine = Some(OcrChoice::Tesseract);
        let request = ConcertSplitRequest {
            concert: test_concert("UncompiledOcr", &["Intro"]),
            input_file: dir.path().join("missing.mp4"),
            output_dir: dir.path().join("out"),
            timestamps: None,
            options,
        };
        let err = run(request, &mut no_progress).unwrap_err();
        assert!(err.to_string().contains("leptess-ocr"));
    }

    // --- Successful outcomes (provided timestamps skip detection; a real
    // fixture is still required because Inspect's ffprobe is unconditional) ---

    #[test]
    fn provided_timestamps_cut_to_complete_with_planned_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let media = fixture(dir.path());
        let output_dir = dir.path().join("out");

        let request = ConcertSplitRequest {
            concert: test_concert("CompleteCut", &["Intro", "Outro"]),
            input_file: media,
            output_dir: output_dir.clone(),
            timestamps: Some(vec![
                SongTimestamp {
                    title: "Intro".to_string(),
                    start_time: 0.0,
                    end_time: 4.0,
                    duration: 4.0,
                },
                SongTimestamp {
                    title: "Outro".to_string(),
                    start_time: 4.0,
                    end_time: 8.0,
                    duration: 4.0,
                },
            ]),
            options: default_options(), // refine_timestamps: false
        };

        let mut events = Vec::new();
        let mut sink = |event: ConcertSplitProgress| events.push(event);
        let outcome = run(request, &mut sink).expect("run should succeed");

        let output = match outcome {
            ConcertSplitOutcome::Complete(output) => output,
            other => panic!("expected Complete, got {other:?}"),
        };
        assert_eq!(output.tracks.len(), 2);
        assert_eq!(output.timestamps.len(), 2);
        assert_eq!(output.timestamps[0].title, "Intro");
        assert_eq!(output.timestamps[1].title, "Outro");
        assert!(output_dir.join("Intro.m4a").exists());
        assert!(output_dir.join("Outro.m4a").exists());

        // refine_timestamps is false and timestamps were explicit, so refine_now
        // is false: timestamps.json must NOT be written (parity with the CLI's
        // historical reset-to-auto/user-timestamp behavior).
        assert!(!output_dir.join("timestamps.json").exists());

        assert!(events
            .iter()
            .any(|e| matches!(e, ConcertSplitProgress::CutPlanned { total: 2 })));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, ConcertSplitProgress::TrackCompleted { .. }))
                .count(),
            2
        );
        // Deep-phase diagnostics (here, from produce::process_segments) must reach
        // the caller through `progress`, not just stdout — this is the point of the
        // typed-progress seam, not only the phase-boundary/track events above.
        assert!(
            events.iter().any(|e| matches!(
                e,
                ConcertSplitProgress::Diagnostic(msg) if msg.contains("Processing") && msg.contains("segments")
            )),
            "expected a deep-phase Diagnostic event from process_segments, got: {events:?}"
        );
    }

    #[test]
    fn no_save_songs_with_explicit_timestamps_yields_analysis_only_and_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let media = fixture(dir.path());
        let output_dir = dir.path().join("out");

        let mut options = default_options();
        options.no_save_songs = true;
        let request = ConcertSplitRequest {
            concert: test_concert("AnalysisOnly", &["Intro", "Outro"]),
            input_file: media,
            output_dir: output_dir.clone(),
            timestamps: Some(vec![
                SongTimestamp {
                    title: "Intro".to_string(),
                    start_time: 0.0,
                    end_time: 4.0,
                    duration: 4.0,
                },
                SongTimestamp {
                    title: "Outro".to_string(),
                    start_time: 4.0,
                    end_time: 8.0,
                    duration: 4.0,
                },
            ]),
            options,
        };

        let outcome = run(request, &mut no_progress).expect("run should succeed");
        assert!(matches!(
            outcome,
            ConcertSplitOutcome::NoOutput {
                reason: NoOutputReason::AnalysisOnly
            }
        ));
        // Neither refine (explicit, non-refine timestamps) nor cut (no_save_songs)
        // ran, so the output directory is never even created — matching the
        // original CLI, which only created it inside those two conditions.
        assert!(!output_dir.exists());
    }

    #[test]
    fn embedded_timestamps_always_refine_and_write_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let media = fixture(dir.path());
        let output_dir = dir.path().join("out");

        let mut concert = test_concert("EmbeddedRefine", &["Intro", "Outro"]);
        // Embedded in the concert JSON itself (not `--timestamps-file`): per the
        // CLI's historical condition, this still refines and writes
        // timestamps.json even though `options.refine_timestamps` is false.
        concert.timestamps = Some(vec![
            SongTimestamp {
                title: "Intro".to_string(),
                start_time: 0.0,
                end_time: 4.0,
                duration: 4.0,
            },
            SongTimestamp {
                title: "Outro".to_string(),
                start_time: 4.0,
                end_time: 8.0,
                duration: 4.0,
            },
        ]);

        let mut options = default_options();
        options.no_save_songs = true; // keep the test fast; unrelated to the write
        let request = ConcertSplitRequest {
            concert,
            input_file: media,
            output_dir: output_dir.clone(),
            timestamps: None, // explicit field absent; embedded is the only source
            options,
        };

        let outcome = run(request, &mut no_progress).expect("run should succeed");
        assert!(matches!(
            outcome,
            ConcertSplitOutcome::NoOutput {
                reason: NoOutputReason::AnalysisOnly
            }
        ));
        let written = std::fs::read_to_string(output_dir.join("timestamps.json"))
            .expect("timestamps.json should have been written");
        let parsed: ConcertInfo = serde_json::from_str(&written).unwrap();
        let timestamps = parsed.timestamps.expect("timestamps should be populated");
        assert_eq!(timestamps.len(), 2);
        assert_eq!(timestamps[0].title, "Intro");
        assert_eq!(timestamps[1].title, "Outro");
    }

    // --- Unsuccessful outcome: locks the Partial-vs-NoOutput classification
    // boundary (today's workflow never produces Partial; see the enum's doc). ---

    #[test]
    fn incomplete_supplied_timestamps_are_rejected_before_cutting() {
        let dir = tempfile::tempdir().unwrap();
        let media = fixture(dir.path());
        let output_dir = dir.path().join("out");

        // "Missing" has no matching timestamp entry, and sits at the head (no
        // anchor before it) — recover_missing_songs explicitly does not recover
        // head/tail runs, only interior gaps, so this is guaranteed to stay
        // StillMissing without depending on any silence heuristic.
        let request = ConcertSplitRequest {
            concert: test_concert("HeadMissing", &["Missing", "Found"]),
            input_file: media,
            output_dir,
            timestamps: Some(vec![SongTimestamp {
                title: "Found".to_string(),
                start_time: 4.0,
                end_time: 8.0,
                duration: 4.0,
            }]),
            options: default_options(),
        };

        let error = run(request, &mut no_progress).unwrap_err();
        assert!(error
            .to_string()
            .contains("timestamps do not match the concert set list"));
    }
}
