//! Cutting song and interlude tracks from the source media once boundaries are
//! known, and writing the resulting timestamps.

use crate::concert_split::{
    ConcertSplitProgress, OutputFormat, ProducedTrack, SongSegment, TrackKind,
};
use crate::cut::{self, VideoCutMode};
use crate::{ffmpeg, io};
use concert_types::{derive_interludes, interlude_filename_stem, ConcertInfo, Song, SongTimestamp};

use anyhow::{anyhow, Result};
use std::fs;

/// Parameters shared by every track cut within a single splitter run. Grouping
/// them avoids threading seven scalar args through every call.
pub(crate) struct CutContext<'a> {
    pub input_file: &'a str,
    pub output_dir: &'a str,
    pub output_format: OutputFormat,
    pub source_params: Option<cut::SourceVideoParams>,
    pub video_cut_mode: VideoCutMode,
    pub concert: &'a ConcertInfo,
}

pub(crate) enum SegmentProduction {
    Complete(Vec<ProducedTrack>),
    Failed {
        completed_tracks: Vec<ProducedTrack>,
        error: anyhow::Error,
    },
}

/// Cut a single track (song or interlude) using the shared [`CutContext`].
///
/// `stem` is the filename without extension (already sanitized).
/// `track_number` is `Some(n)` for songs (embedded as ffmpeg metadata) and
/// `None` for interludes.
fn extract_track(
    ctx: &CutContext<'_>,
    stem: &str,
    start_time: f64,
    end_time: f64,
    title: &str,
    track_number: Option<usize>,
) -> Result<()> {
    match ctx.output_format {
        OutputFormat::Video | OutputFormat::Both => {
            let output_file = format!("{}/{}.mp4", ctx.output_dir, stem);
            match &ctx.source_params {
                Some(params) => cut::extract_segment_smart(
                    ctx.input_file,
                    &output_file,
                    start_time,
                    end_time,
                    params,
                    Some(title),
                    ctx.concert,
                    track_number,
                )?,
                None => cut::extract_segment(
                    ctx.input_file,
                    &output_file,
                    start_time,
                    end_time,
                    ctx.video_cut_mode,
                    Some(title),
                    ctx.concert,
                    track_number,
                )?,
            }
        }
        _ => {}
    }

    match ctx.output_format {
        OutputFormat::Audio | OutputFormat::Both => {
            let output_file = format!("{}/{}.m4a", ctx.output_dir, stem);
            ffmpeg::extract_audio_segment(
                ctx.input_file,
                &output_file,
                start_time,
                end_time,
                Some(title),
                ctx.concert,
                track_number,
            )?;
        }
        _ => {}
    }

    Ok(())
}

/// Remove any previously written interlude files from `output_dir` before
/// (re-)cutting interludes, to avoid stale orphans when the number of interludes
/// changes.  Only files whose names match the anchored pattern
/// `interlude_NN.mp4|.m4a` are removed.
fn remove_stale_interlude_files(
    output_dir: &str,
    progress: &mut dyn FnMut(ConcertSplitProgress),
) -> Result<()> {
    let pattern =
        regex::Regex::new(r"^interlude_\d{2}\.(mp4|m4a)$").expect("static regex is valid");
    let dir = match fs::read_dir(output_dir) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(anyhow!(
                "failed to read output directory {}: {}",
                output_dir,
                e
            ))
        }
    };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if pattern.is_match(&name_str) {
            let path = entry.path();
            if let Err(e) = fs::remove_file(&path) {
                progress(ConcertSplitProgress::Warning(format!(
                    "could not remove stale interlude file {}: {}",
                    path.display(),
                    e
                )));
            } else {
                progress(ConcertSplitProgress::Diagnostic(format!(
                    "removed stale interlude file: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

pub(crate) fn create_song_timestamps(
    segments: &[SongSegment],
    song_list: &[Song],
) -> Vec<SongTimestamp> {
    let mut song_timestamps = Vec::new();
    let mut song_counter = 0;

    for segment in segments.iter() {
        if !segment.segment.is_song {
            // Skip gaps
            continue;
        }

        // Process song
        song_counter += 1;

        // Get song title
        let song_title = if song_counter <= song_list.len() {
            &song_list[song_counter - 1].title
        } else {
            // Fallback if we have more segments than songs
            &format!("song_{}", song_counter)
        };

        // Add song to timestamps collection
        song_timestamps.push(SongTimestamp {
            title: song_title.to_string(),
            start_time: segment.segment.start_time,
            end_time: segment.segment.end_time,
            duration: segment.segment.end_time - segment.segment.start_time,
        });
    }

    song_timestamps
}

pub(crate) fn process_segments(
    segments: &[SongSegment],
    concert: &ConcertInfo,
    ctx: CutContext<'_>,
    emit_interludes: bool,
    media_duration: f64,
    progress: &mut dyn FnMut(ConcertSplitProgress),
) -> SegmentProduction {
    let songs = &concert.set_list;
    progress(ConcertSplitProgress::Diagnostic(format!(
        "Processing {} segments...",
        segments.len()
    )));
    if segments.len() > songs.len() {
        return SegmentProduction::Failed {
            completed_tracks: Vec::new(),
            error: anyhow!(
                "Too many segments detected. {} segments but only {} songs provided.",
                segments.len(),
                songs.len()
            ),
        };
    }

    // Computed upfront (same inputs, same logic as the caller's outcome
    // timestamps) so interludes — and the planned track total — are known before
    // any actual cutting starts.
    let song_timestamps = create_song_timestamps(segments, songs);
    let interludes = if emit_interludes {
        derive_interludes(&song_timestamps, media_duration)
    } else {
        Vec::new()
    };
    let planned_song_count = segments.iter().filter(|s| s.segment.is_song).count();
    progress(ConcertSplitProgress::CutPlanned {
        total: planned_song_count + interludes.len(),
    });

    let mut tracks: Vec<ProducedTrack> = Vec::with_capacity(planned_song_count + interludes.len());
    let mut song_counter = 0;
    let mut gap_counter = 0;

    for segment in segments.iter() {
        if !segment.segment.is_song {
            // Gaps between songs that came from the legacy is_song=false path.
            // In --timestamps-file mode all segments are songs so this branch is
            // unreachable; kept for backwards-compatibility with analysis mode.
            gap_counter += 1;
            progress(ConcertSplitProgress::Diagnostic(format!(
                "ignoring gap {}: {:.2}s to {:.2}s",
                gap_counter, segment.segment.start_time, segment.segment.end_time
            )));
            continue;
        }

        // Process song
        song_counter += 1;

        // Check if we have a song title for this segment
        let song_title = if song_counter <= songs.len() {
            &songs[song_counter - 1].title
        } else {
            // Fallback if we have more segments than songs
            progress(ConcertSplitProgress::Warning(
                "More song segments detected than provided in setlist. Using default naming."
                    .to_string(),
            ));
            &format!("song_{}", song_counter)
        };

        // Create a safe filename from the song title
        let safe_title = io::sanitize_filename(song_title);

        progress(ConcertSplitProgress::Diagnostic(format!(
            "Extracting {:#?} for song {}: \"{}\" - {:.2}s to {:.2}s (duration: {:.2}s)",
            &ctx.output_format,
            song_counter,
            song_title,
            segment.segment.start_time,
            segment.segment.end_time,
            segment.segment.end_time - segment.segment.start_time
        )));

        if let Err(error) = extract_track(
            &ctx,
            &safe_title,
            segment.segment.start_time,
            segment.segment.end_time,
            song_title,
            Some(song_counter),
        ) {
            return SegmentProduction::Failed {
                completed_tracks: tracks,
                error,
            };
        }

        progress(ConcertSplitProgress::TrackCompleted {
            index: song_counter,
            title: song_title.to_string(),
            kind: TrackKind::Song,
        });
        tracks.push(ProducedTrack {
            title: song_title.to_string(),
            kind: TrackKind::Song,
            start_time: segment.segment.start_time,
            end_time: segment.segment.end_time,
        });
    }

    progress(ConcertSplitProgress::Diagnostic(format!(
        "Successfully extracted {} songs and {} gaps",
        song_counter, gap_counter
    )));

    // Emit interlude tracks for every uncovered span in [0, media_duration].
    if emit_interludes {
        if let Err(error) = remove_stale_interlude_files(ctx.output_dir, progress) {
            return SegmentProduction::Failed {
                completed_tracks: tracks,
                error,
            };
        }
        progress(ConcertSplitProgress::Diagnostic(format!(
            "Emitting {} interlude track(s) to cover the full timeline",
            interludes.len()
        )));
        for interlude in &interludes {
            let stem = interlude_filename_stem(interlude.index);
            progress(ConcertSplitProgress::Diagnostic(format!(
                "Extracting interlude {}: {:.2}s to {:.2}s (duration: {:.2}s)",
                interlude.index,
                interlude.start_time,
                interlude.end_time,
                interlude.end_time - interlude.start_time,
            )));
            if let Err(error) = extract_track(
                &ctx,
                &stem,
                interlude.start_time,
                interlude.end_time,
                "interlude",
                None, // no track number for interludes
            ) {
                return SegmentProduction::Failed {
                    completed_tracks: tracks,
                    error,
                };
            }

            progress(ConcertSplitProgress::TrackCompleted {
                index: interlude.index,
                title: stem.clone(),
                kind: TrackKind::Interlude,
            });
            tracks.push(ProducedTrack {
                title: stem,
                kind: TrackKind::Interlude,
                start_time: interlude.start_time,
                end_time: interlude.end_time,
            });
        }
    }

    SegmentProduction::Complete(tracks)
}
