//! In-process Concert Split library adapter (#141): translates a [`SplitJob`]
//! into a `live_set_splitter::concert_split::ConcertSplitRequest`, runs it
//! synchronously on a blocking thread, and maps the typed outcome back onto the
//! job engine's [`JobStepOutcome`]. This is the library counterpart to
//! `build_cli_split_command` (`jobs::mod`), which does the same translation for
//! the CLI (subprocess) adapter. See docs/concert-split.md for the library
//! interface this adapts, and
//! docs/change/2026-07-21-library-splitter-default.md for why concert-web now
//! defaults to it.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use live_set_splitter::concert_split::{
    self, ConcertSplitOptions, ConcertSplitOutcome, ConcertSplitProgress, ConcertSplitRequest,
    NoOutputReason, OutputFormat, SplitPhase, TrackKind,
};
use live_set_splitter::cut::VideoCutMode;

use super::{JobStepOutcome, SplitJob, SplitMode};

/// The subset of [`SplitJob`]'s fields the library adapter needs, owned so it
/// can cross into `spawn_blocking`'s `'static` closure without cloning
/// `SplitJob` itself (which isn't `Clone` — it owns `NamedTempFile` RAII
/// guards that must not be duplicated). The real `SplitJob` stays alive on the
/// caller's stack for the duration of this module's `run` future (see its doc
/// comment), so `json_path`'s underlying temp file is still there when
/// `write_concert_json_if_analyze` reads it on the blocking thread.
struct Job {
    concert_id: i64,
    concert: concert_types::ConcertInfo,
    json_path: PathBuf,
    input_file: PathBuf,
    output_dir: PathBuf,
    mode: SplitMode,
}

impl From<&SplitJob> for Job {
    fn from(job: &SplitJob) -> Self {
        Self {
            concert_id: job.concert_id,
            concert: job.concert.clone(),
            json_path: job.json_path.clone(),
            input_file: job.input_file.clone(),
            output_dir: job.output_dir.clone(),
            mode: job.mode.clone(),
        }
    }
}

/// Options concert-web always wants from the library adapter. The CLI adapter
/// gets `OutputFormat::Both`/`VideoCutMode::Smart` for free from clap's
/// `#[arg(default_value_t = ...)]` on `live-set-song-splitter/src/main.rs`'s
/// `Cli` struct — `JobConfig::production`'s subprocess command builder never
/// passes `--output-format`/`--video-cut-mode` explicitly. The library adapter
/// has no clap layer between it and `ConcertSplitOptions`, so these (and every
/// other tuning flag) must be set explicitly here to match, or splits would
/// silently ship audio-only/copy-cut output.
fn options_for(job: &Job) -> ConcertSplitOptions {
    let (emit_interludes, media_duration) = match &job.mode {
        SplitMode::UserTimestamps { media_duration, .. } => (true, Some(*media_duration)),
        SplitMode::Analyze | SplitMode::ResetToAuto(_) => (false, None),
    };
    ConcertSplitOptions {
        no_save_songs: false,
        // Deliberately false for every mode: user/reset timestamps are already
        // correct and must not be silently rewritten (mirrors the CLI
        // subprocess command builder never passing --refine-timestamps); Analyze
        // mode's `timestamps: None` below makes `run` refine unconditionally
        // regardless of this flag (see docs/concert-split.md).
        refine_timestamps: false,
        output_format: OutputFormat::Both,
        video_cut_mode: VideoCutMode::Smart,
        analyze_images: false,
        reuse_frames: false,
        keep_frames: false,
        ocr_engine: None,
        emit_interludes,
        media_duration,
    }
}

/// Translate a [`Job`] into a typed [`ConcertSplitRequest`], mirroring
/// `build_cli_split_command`'s (`jobs::mod`) argument translation field for
/// field: Analyze supplies no timestamps (detection runs); UserTimestamps and
/// ResetToAuto both supply already-correct timestamps and skip detection.
fn request_for(job: &Job) -> ConcertSplitRequest {
    let timestamps = match &job.mode {
        SplitMode::Analyze => None,
        SplitMode::UserTimestamps { ts, .. } | SplitMode::ResetToAuto(ts) => {
            Some(ts.songs().to_vec())
        }
    };
    ConcertSplitRequest {
        concert: job.concert.clone(),
        input_file: job.input_file.clone(),
        output_dir: job.output_dir.clone(),
        timestamps,
        options: options_for(job),
    }
}

/// Render one [`ConcertSplitProgress`] event to a single human-readable line
/// plus its stream, mirroring the CLI adapter's `render_progress`
/// (`live-set-song-splitter/src/main.rs`) closely enough that the per-job log
/// file/tracing output reads the same regardless of which adapter split it.
fn render_progress_line(event: &ConcertSplitProgress) -> (&'static str, String) {
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
                SplitPhase::Cleanup => "Cleaning up",
            };
            ("stdout", label.to_string())
        }
        ConcertSplitProgress::CutPlanned { total } => (
            "stdout",
            format!("Processing {} planned track(s)...", total),
        ),
        ConcertSplitProgress::TrackCompleted { index, title, kind } => {
            let noun = match kind {
                TrackKind::Song => "song",
                TrackKind::Interlude => "interlude",
            };
            ("stdout", format!("Completed {} {}: {}", noun, index, title))
        }
        ConcertSplitProgress::Warning(message) => ("stderr", format!("Warning: {}", message)),
        ConcertSplitProgress::Diagnostic(message) => ("stdout", message.clone()),
    }
}

/// `concert.json` is a byte-for-byte copy of `job.json_path` (only written if
/// not already present) — a transport artifact. The CLI adapter's subprocess
/// gets this for free: the spawned `live-set-splitter` binary's own `main()`
/// copies it (`copy_concert_json`, `live-set-song-splitter/src/main.rs`). The
/// library adapter runs no subprocess, so this adapter must replicate that
/// copy itself. Only Analyze mode writes it — mirrors the CLI's
/// `refine_now = timestamps_file.is_none() || refine_timestamps` gate, which
/// is `true` only for Analyze (`job.json_path` holds the same typed
/// `ConcertInfo` data the CLI would have copied from — see `SplitJob::concert`'s
/// doc comment).
fn write_concert_json_if_analyze(job: &Job) -> Result<(), String> {
    if !matches!(job.mode, SplitMode::Analyze) {
        return Ok(());
    }
    let canonical_path = job.output_dir.join("concert.json");
    if canonical_path.exists() {
        return Ok(());
    }
    std::fs::copy(&job.json_path, &canonical_path)
        .map(|_| ())
        .map_err(|e| {
            format!(
                "Failed to copy {} -> {}: {}",
                job.json_path.display(),
                canonical_path.display(),
                e
            )
        })
}

/// Map a [`ConcertSplitOutcome`] (or an infrastructure `Err`) onto
/// [`JobStepOutcome`], mirroring the CLI adapter's `exit_code_for`
/// (`live-set-song-splitter/src/main.rs`): `Complete`/`NoOutput::AnalysisOnly`
/// succeed (exit 0 there); `NoOutput::NothingDetected`/`Partial`/an
/// infrastructure error fail (exit 1 there). A `concert.json` copy failure
/// also fails the step — mirroring the CLI binary's own `main()`, which
/// propagates that copy's `Result` with `?` and so exits non-zero even though
/// splitting itself succeeded.
fn outcome_to_step(job: &Job, result: anyhow::Result<ConcertSplitOutcome>) -> JobStepOutcome {
    match result {
        Ok(ConcertSplitOutcome::Complete(_))
        | Ok(ConcertSplitOutcome::NoOutput {
            reason: NoOutputReason::AnalysisOnly,
        }) => match write_concert_json_if_analyze(job) {
            Ok(()) => JobStepOutcome::Succeeded,
            Err(message) => JobStepOutcome::Failed { message },
        },
        Ok(ConcertSplitOutcome::NoOutput {
            reason: reason @ NoOutputReason::NothingDetected { .. },
        }) => JobStepOutcome::Failed {
            message: reason.to_string(),
        },
        Ok(ConcertSplitOutcome::Partial(_)) => JobStepOutcome::Failed {
            message: "unexpected Partial outcome (reserved variant not produced by this build)"
                .to_string(),
        },
        Err(err) => JobStepOutcome::Failed {
            message: format!("{err:#}"),
        },
    }
}

/// Run `concert_split::run` on a blocking thread — it's synchronous and can
/// take minutes (ffmpeg/OCR), so calling it directly on an async task would
/// starve the tokio runtime it's spawned from. Progress is forwarded to
/// tracing and the optional per-job log file as it arrives; unlike the CLI
/// adapter's subprocess (which `kill_on_drop`s on cancellation), a cancelled
/// job here cannot stop this blocking thread — see docs/concert-split.md's
/// adapter-selection section for the accepted cancellation-semantics
/// divergence this implies.
fn run_blocking(job: &Job, log_file: Option<&Path>) -> JobStepOutcome {
    let mut log = log_file.and_then(|path| match std::fs::File::create(path) {
        Ok(f) => Some(f),
        Err(e) => {
            tracing::warn!("failed to create job log file {}: {}", path.display(), e);
            None
        }
    });

    let concert_id = job.concert_id;
    let mut sink = |event: ConcertSplitProgress| {
        let (stream, line) = render_progress_line(&event);
        tracing::info!(
            target: "concert_tracker::jobs::split",
            kind = "split",
            concert_id = concert_id,
            stream = stream,
            "{}",
            line
        );
        if let Some(f) = log.as_mut() {
            let _ = writeln!(f, "[{}] {}", stream, line);
        }
    };

    let result = concert_split::run(request_for(job), &mut sink);
    outcome_to_step(job, result)
}

/// Async entry point wired into [`super::ProductionJobRunner::run_split`] for
/// [`super::SplitBackend::Library`]. `job` and everything it owns (including
/// its temp-file RAII guards) stay alive on the caller's stack for this
/// future's whole lifetime, so `run_blocking` (moved into `spawn_blocking` as
/// an owned [`Job`] snapshot of the paths it needs) can safely read
/// `job.json_path`'s file without racing its deletion.
pub(super) async fn run(job: &SplitJob, log_file: Option<&Path>) -> JobStepOutcome {
    let concert_id = job.concert_id;
    let owned_job = Job::from(job);
    let log_file = log_file.map(|p| p.to_path_buf());
    let outcome =
        tokio::task::spawn_blocking(move || run_blocking(&owned_job, log_file.as_deref())).await;
    match outcome {
        Ok(step) => step,
        Err(join_error) => JobStepOutcome::Failed {
            message: format!("split job {concert_id} panicked or was aborted: {join_error}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::split_timestamps::{TimestampPayloadSong, ValidatedTimestamps};

    fn validated_timestamps() -> ValidatedTimestamps {
        let set_list = vec!["Song".to_string()];
        let songs = vec![TimestampPayloadSong {
            title: "Song".to_string(),
            start_time: 0.0,
            end_time: 100.0,
        }];
        ValidatedTimestamps::validate(&set_list, None, &songs).unwrap()
    }

    fn test_job(mode: SplitMode) -> Job {
        Job {
            concert_id: 1,
            concert: concert_types::ConcertInfo {
                artist: "Artist".to_string(),
                source: String::new(),
                show: String::new(),
                date: None,
                album: "Album".to_string(),
                description: None,
                set_list: vec![],
                musicians: vec![],
                preview_image_url: None,
                teaser: None,
                timestamps: None,
            },
            json_path: PathBuf::from("/does/not/matter/for/pure/translation.json"),
            input_file: PathBuf::from("/media/input.mp4"),
            output_dir: PathBuf::from("/media/output"),
            mode,
        }
    }

    #[test]
    fn options_for_analyze_matches_cli_defaults_and_skips_interludes() {
        let job = test_job(SplitMode::Analyze);
        let options = options_for(&job);
        assert!(matches!(options.output_format, OutputFormat::Both));
        assert!(matches!(options.video_cut_mode, VideoCutMode::Smart));
        assert!(!options.no_save_songs);
        assert!(!options.emit_interludes);
        assert_eq!(options.media_duration, None);
    }

    #[test]
    fn options_for_user_timestamps_emits_interludes_with_media_duration() {
        let job = test_job(SplitMode::UserTimestamps {
            ts: validated_timestamps(),
            media_duration: 321.0,
        });
        let options = options_for(&job);
        assert!(options.emit_interludes);
        assert_eq!(options.media_duration, Some(321.0));
    }

    #[test]
    fn options_for_reset_to_auto_skips_interludes() {
        let job = test_job(SplitMode::ResetToAuto(validated_timestamps()));
        let options = options_for(&job);
        assert!(!options.emit_interludes);
        assert_eq!(options.media_duration, None);
    }

    #[test]
    fn request_for_analyze_has_no_timestamps() {
        let job = test_job(SplitMode::Analyze);
        let request = request_for(&job);
        assert!(request.timestamps.is_none());
    }

    #[test]
    fn request_for_user_timestamps_carries_the_provided_songs() {
        let job = test_job(SplitMode::UserTimestamps {
            ts: validated_timestamps(),
            media_duration: 100.0,
        });
        let request = request_for(&job);
        let ts = request
            .timestamps
            .expect("UserTimestamps must set timestamps");
        assert_eq!(ts.len(), 1);
        assert_eq!(ts[0].title, "Song");
    }

    #[test]
    fn request_for_reset_to_auto_carries_the_provided_songs() {
        let job = test_job(SplitMode::ResetToAuto(validated_timestamps()));
        let request = request_for(&job);
        assert!(request.timestamps.is_some());
    }

    #[test]
    fn outcome_to_step_maps_complete_to_succeeded_and_writes_concert_json() {
        let tmp = tempfile::tempdir().unwrap();
        let output_dir = tmp.path().join("out");
        std::fs::create_dir_all(&output_dir).unwrap();
        let mut job = test_job(SplitMode::Analyze);
        job.output_dir = output_dir.clone();
        job.json_path = tmp.path().join("input.json");
        std::fs::write(&job.json_path, b"{\"fake\":true}").unwrap();

        let outcome = Ok(ConcertSplitOutcome::Complete(
            live_set_splitter::concert_split::ConcertSplitOutput {
                timestamps: vec![],
                tracks: vec![],
                output_dir: output_dir.clone(),
            },
        ));
        let step = outcome_to_step(&job, outcome);
        assert!(matches!(step, JobStepOutcome::Succeeded));
        assert!(output_dir.join("concert.json").exists());
    }

    #[test]
    fn outcome_to_step_does_not_write_concert_json_for_user_timestamps() {
        let tmp = tempfile::tempdir().unwrap();
        let output_dir = tmp.path().join("out");
        std::fs::create_dir_all(&output_dir).unwrap();
        let mut job = test_job(SplitMode::UserTimestamps {
            ts: validated_timestamps(),
            media_duration: 100.0,
        });
        job.output_dir = output_dir.clone();
        job.json_path = tmp.path().join("input.json");
        std::fs::write(&job.json_path, b"{\"fake\":true}").unwrap();

        let outcome = Ok(ConcertSplitOutcome::Complete(
            live_set_splitter::concert_split::ConcertSplitOutput {
                timestamps: vec![],
                tracks: vec![],
                output_dir: output_dir.clone(),
            },
        ));
        let step = outcome_to_step(&job, outcome);
        assert!(matches!(step, JobStepOutcome::Succeeded));
        assert!(!output_dir.join("concert.json").exists());
    }

    #[test]
    fn outcome_to_step_maps_nothing_detected_to_failed() {
        let job = test_job(SplitMode::Analyze);
        let outcome = Ok(ConcertSplitOutcome::NoOutput {
            reason: NoOutputReason::NothingDetected {
                missing: vec!["Song".to_string()],
            },
        });
        let step = outcome_to_step(&job, outcome);
        match step {
            JobStepOutcome::Failed { message } => assert!(message.contains("Song")),
            JobStepOutcome::Succeeded => panic!("expected Failed"),
        }
    }

    #[test]
    fn outcome_to_step_maps_infra_error_to_failed() {
        let job = test_job(SplitMode::Analyze);
        let outcome = Err(anyhow::anyhow!("ffprobe exploded"));
        let step = outcome_to_step(&job, outcome);
        match step {
            JobStepOutcome::Failed { message } => assert!(message.contains("ffprobe exploded")),
            JobStepOutcome::Succeeded => panic!("expected Failed"),
        }
    }

    #[test]
    fn outcome_to_step_maps_partial_to_failed() {
        let job = test_job(SplitMode::Analyze);
        let outcome = Ok(ConcertSplitOutcome::Partial(
            live_set_splitter::concert_split::ConcertSplitOutput {
                timestamps: vec![],
                tracks: vec![],
                output_dir: job.output_dir.clone(),
            },
        ));
        let step = outcome_to_step(&job, outcome);
        assert!(matches!(step, JobStepOutcome::Failed { .. }));
    }
}
