pub mod ocr;
pub mod ocr_backend;
#[cfg(feature = "leptess-ocr")]
pub mod ocr_leptess;
#[cfg(feature = "paddle-ocr")]
pub mod ocr_paddle;
use crate::ocr::{matches_song_title, matches_song_title_weighted, song_title_candidate_lines};
use crate::ocr_backend::{create_ocr_backend, default_ocr_choice, OcrChoice, OcrPhase};
mod audio;
mod ffmpeg;
mod image;
mod io;
mod video;
use crate::video::VideoInfo;
use concert_types::{ConcertInfo, Song, SongTimestamp};

use anyhow::{anyhow, Context, Result};
use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::Path;

/// Output format for extracted segments
#[derive(Parser, Debug, Clone, Copy, ValueEnum)]
#[clap(rename_all = "lowercase")]
enum OutputFormat {
    /// Output video files (mp4)
    Video,
    /// Output audio files (m4a)
    Audio,
    /// Output both video and audio files
    Both,
}

impl Default for OutputFormat {
    fn default() -> Self {
        OutputFormat::Both
    }
}

/// How to cut the video stream for each track. Both modes keep audio and video in
/// sync; they trade cut precision against speed/quality.
#[derive(Parser, Debug, Clone, Copy, ValueEnum, PartialEq)]
#[clap(rename_all = "lowercase")]
enum VideoCutMode {
    /// Stream-copy (fast, lossless). Snaps each cut back to the nearest preceding
    /// keyframe, so a track may start up to one GOP (a few seconds) early.
    Copy,
    /// Re-encode the video so each cut is frame-accurate at the detected start
    /// (slower, slight quality change).
    Reencode,
}

impl Default for VideoCutMode {
    fn default() -> Self {
        VideoCutMode::Copy
    }
}

/// x264 encoding parameters used by [`VideoCutMode::Reencode`].
const REENCODE_PRESET: &str = "veryfast";
const REENCODE_CRF: &str = "18";

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

    /// How to cut the video stream: `copy` (fast, lossless; snaps each cut back to
    /// the nearest preceding keyframe) or `reencode` (frame-accurate at the detected
    /// start, but slower and re-encodes the video). Both keep audio/video in sync.
    #[arg(long, value_enum, default_value_t = VideoCutMode::Copy)]
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
}

#[derive(Clone, Debug)]
struct AudioSegment {
    pub start_time: f64,
    pub end_time: f64,
    pub is_song: bool,
}

#[derive(Clone, Debug)]
struct SongSegment {
    pub song: Song,
    pub segment: AudioSegment,
    /// True when `segment.start_time` came from detecting the title overlay. The
    /// overlay appears ~`OVERLAY_DELAY_SECONDS` after the song actually starts, so
    /// these (and only these) starts get the overlay-delay pullback when audio
    /// silence can't relocate them. Recovered/silence-placed and JSON-loaded starts
    /// are not overlay estimates and must not be pulled back.
    pub start_from_overlay: bool,
}

/// Write the two metadata JSON files into `output_dir`:
///   - `concert.json` — a verbatim copy of the input metadata, only if not
///     already present.
///   - `timestamps.json` — the timestamps-augmented concert struct.
fn write_concert_json_outputs(output_dir: &str, input_path: &str, concert: &ConcertInfo) -> Result<()> {
    let canonical_path = format!("{}/concert.json", output_dir);
    if !Path::new(&canonical_path).exists() {
        fs::copy(input_path, &canonical_path)
            .with_context(|| format!("Failed to copy {} -> {}", input_path, canonical_path))?;
    }

    let timestamps_path = format!("{}/timestamps.json", output_dir);
    fs::write(&timestamps_path, serde_json::to_string_pretty(concert)?)
        .with_context(|| format!("Failed to write {}", timestamps_path))?;
    Ok(())
}

fn folder_name(info: &ConcertInfo) -> String {
    // Strip colons only (matches concert-tracker's sanitize_album so the same
    // directory is referenced from both sides).
    let name = if info.album.is_empty() {
        &info.artist
    } else {
        &info.album
    };
    io::sanitize_filename(&name.replace(':', ""))
}

#[derive(Serialize, Deserialize, Debug)]
struct Timestamps {
    songs: Vec<SongTimestamp>,
}

fn main() -> Result<()> {
    // Parse command line arguments using clap
    let cli = Cli::parse();

    // Fail fast if an explicitly-chosen OCR backend wasn't compiled into this build.
    if let Some(choice) = cli.ocr_engine {
        crate::ocr_backend::ensure_ocr_choice_available(choice)?;
    }

    let cli_input_file = &cli.input_file;
    let concert_path = &cli.concert_file;

    // Parse the JSON setlist file
    let concert_file = File::open(concert_path)
        .with_context(|| format!("Failed to open setlist file: {}", concert_path))?;
    let concert_reader = BufReader::new(concert_file);
    let mut concert: ConcertInfo = serde_json::from_reader(concert_reader)
        .with_context(|| format!("Failed to parse setlist JSON from {}", concert_path))?;
    let info = concert.clone();

    let num_songs = concert.set_list.len();

    let input_file = match cli_input_file {
        Some(file) => file.clone(),
        None => {
            if info.album.is_empty() {
                return Err(anyhow!("No album found in concert metadata file. Please specify a --input-path to the mp4 file for the concert."));
            }
            let album = info.album.replace(':', "");
            let input_dir = match std::path::Path::new(&concert_path).parent() {
                Some(dir) => dir.to_str().unwrap(),
                None => ".",
            };
            if input_dir == "" {
                format!("{}.mp4", album)
            } else {
                format!("{}/{}.mp4", input_dir, album).to_string()
            }
        }
    };

    println!("Analyzing file: {}", input_file);
    println!("Artist: {}", info.artist);
    println!("Expected number of songs: {}", num_songs);
    println!("Songs:");
    for (i, song) in concert.set_list.iter().enumerate() {
        println!("  {}. {}", i + 1, song.title);
    }

    // Get all video information at once
    let video_info = VideoInfo::from_ffprobe_file(&input_file)
        .with_context(|| format!("Failed to get video information from {}", input_file))?;
    println!("Total duration: {:.2} seconds", video_info.duration);

    // Determine output directory path (will be used later too).
    //
    // When `--output-dir` is supplied, use it verbatim — the caller (e.g.
    // concert-tracker) has already computed the per-concert directory. When
    // omitted, default to a sibling directory named after the concert.
    let output_dir = if let Some(custom_dir) = &cli.output_dir {
        println!("Using custom output directory: {}", custom_dir);
        custom_dir.clone()
    } else {
        folder_name(&info)
    };

    // If timestamps file is provided, read from it instead of detecting segments
    let mut segments = Vec::new();
    if let Some(timestamps_path) = &cli.timestamps_file {
        println!("Reading song timestamps from file: {}", timestamps_path);
        // Fall back to the old format if not found
        let timestamps_file = File::open(timestamps_path)
            .with_context(|| format!("Failed to open timestamps file: {}", timestamps_path))?;
        let timestamps_reader = BufReader::new(timestamps_file);
        let timestamps_data: Timestamps = serde_json::from_reader(timestamps_reader)
            .with_context(|| format!("Failed to parse timestamps JSON from {}", timestamps_path))?;

        if timestamps_data.songs.len() == 0 {
            return Err(anyhow!("Timestamps file has no timestamps"));
        }
        // Create segments from the timestamps
        for song_timestamp in &timestamps_data.songs {
            let song = Song {
                title: song_timestamp.title.clone(),
            };
            let segment = AudioSegment {
                start_time: song_timestamp.start_time,
                end_time: song_timestamp.end_time,
                is_song: true,
            };
            segments.push(SongSegment {
                song,
                segment,
                // Loaded from JSON, not a fresh overlay estimate.
                start_from_overlay: false,
            });
        }

        println!(
            "Loaded {} song segments from timestamps file",
            segments.len()
        );
    } else if let Some(timestamps) = &concert.timestamps {
        // Create segments from the timestamps
        for song_timestamp in timestamps {
            let song = Song {
                title: song_timestamp.title.clone(),
            };
            let segment = AudioSegment {
                start_time: song_timestamp.start_time,
                end_time: song_timestamp.end_time,
                is_song: true,
            };
            segments.push(SongSegment {
                song,
                segment,
                // Loaded from JSON, not a fresh overlay estimate.
                start_from_overlay: false,
            });
        }
        println!("Loaded {} song segments from JSON file", segments.len());
    }

    io::ensure_dir("temp_frames")?;
    let temp_dir = format!("temp_frames/{}", folder_name(&info));
    io::ensure_dir(&temp_dir)?;

    if segments.len() == 0 {
        let settings = Settings {
            analyze_images: cli.analyze_images,
            reuse_frames: cli.reuse_frames,
            ocr_choice: cli.ocr_engine.unwrap_or_else(default_ocr_choice),
        };

        // First try to detect song boundaries using text overlays
        println!("Attempting to detect song boundaries using text overlays...");

        // Get song segments from text detection
        let song_segments = detect_song_boundaries_from_text(
            &input_file,
            &info.artist,
            &concert.set_list,
            &video_info,
            &settings,
            &temp_dir,
        )?;
        segments = song_segments;
        for segment in &segments {
            println!("Segment: {:?}", segment);
        }
    }

    // Cache for the audio waveform — extracted at most once, regardless of
    // whether silence-based recovery and/or refinement need it.
    let mut audio_data: Option<Vec<f32>> = None;

    // If text detection came up short, try silence-based recovery before erroring.
    if segments.iter().filter(|s| s.segment.is_song).count() < num_songs {
        println!("Text overlay detection missing some songs; extracting audio for silence-based recovery...");
        let waveform = audio::extract_audio_waveform(&input_file)
            .with_context(|| format!("Failed to extract audio waveform from {}", input_file))?;
        let results =
            recover_missing_songs_from_silence(&mut segments, &concert.set_list, &waveform);
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
            return Err(anyhow!(
                "Text overlay detection didn't find all songs and silence-based recovery couldn't fill in: {}",
                still_missing.join(", ")
            ));
        }
        for segment in &segments {
            println!("Segment (post-recovery): {:?}", segment);
        }
    }

    if cli.timestamps_file.is_none() || cli.refine_timestamps {
        // Reuse the waveform already extracted for recovery; otherwise pull it now.
        let audio_data = match audio_data.take() {
            Some(w) => w,
            None => {
                println!("Extracting audio waveform for refinement...");
                audio::extract_audio_waveform(&input_file)
                    .with_context(|| format!("Failed to extract audio waveform from {}", input_file))?
            }
        };

        // Refine segments using audio analysis
        println!("Refining song boundaries using audio analysis...");
        segments = refine_segments_with_audio_analysis(&segments, &audio_data, video_info.duration)
            .with_context(|| "Failed to refine segments with audio analysis")?;
        println!("Found {} segments", segments.len());

        // Refine the end time of the last song using black frame detection
        segments = refine_last_song_end_time(
            &input_file,
            segments,
            video_info.duration,
            cli.reuse_frames,
            &temp_dir,
        )
        .with_context(|| "Failed to refine last song end time")?;

        // Create song timestamps and output JSON file
        concert.timestamps = Some(create_song_timestamps(&segments, &concert.set_list));
        // Create output directory for JSON file even if we don't save songs
        fs::create_dir_all(&output_dir)
            .with_context(|| format!("Failed to create output directory: {}", output_dir))?;
        write_concert_json_outputs(&output_dir, concert_path, &concert)?;
    }

    for (i, segment) in segments.iter().enumerate() {
        println!(
            "Segment {}: {:.2}s to {:.2}s ({:.2}s) - {}",
            i + 1,
            segment.segment.start_time,
            segment.segment.end_time,
            segment.segment.end_time - segment.segment.start_time,
            if segment.segment.is_song {
                "SONG"
            } else {
                "gap"
            }
        );
    }

    // Process each detected segment (skip if --no-save-songs is provided)
    if !cli.no_save_songs {
        fs::create_dir_all(&output_dir)?;
        process_segments(
            &input_file,
            &segments,
            concert,
            &output_dir,
            cli.output_format,
            cli.video_cut_mode,
        )?;
    }

    // Print completion message based on output format
    match cli.output_format {
        OutputFormat::Video => println!("Video splitting complete!"),
        OutputFormat::Audio => println!("Audio extraction complete!"),
        OutputFormat::Both => println!("Video and audio extraction complete!"),
    }

    if cli.keep_frames {
        println!("Keeping temporary frames folder (--keep-frames): {}", temp_dir);
    } else if std::path::Path::new(&temp_dir).exists() {
        println!("Cleaning up temporary folder: {}", temp_dir);
        match fs::remove_dir_all(&temp_dir) {
            Ok(_) => println!("Successfully removed temporary album folder"),
            Err(e) => println!("Warning: Failed to clean up temporary album folder: {}", e),
        }
    }

    Ok(())
}

fn refine_last_song_end_time(
    input_file: &str,
    segments: Vec<SongSegment>,
    total_duration: f64,
    reuse_frames: bool,
    temp_dir: &str,
) -> Result<Vec<SongSegment>> {
    // Find the last song segment
    let mut refined_segments = segments;
    if let Some(last_idx) = refined_segments.iter().rposition(|seg| seg.segment.is_song) {
        println!("Finding precise end time for the last song...");

        // Get the current end time of the last song
        let current_end = refined_segments[last_idx].segment.end_time;

        // Try to find a black frame to use as the end time
        if let Some(black_frame_time) =
            find_black_frame_end_time(input_file, total_duration, reuse_frames, temp_dir)?
        {
            println!(
                "Adjusted last song end time from {:.2}s to {:.2}s (found black frame)",
                current_end, black_frame_time
            );
            refined_segments[last_idx].segment.end_time = black_frame_time;
        }
    }

    Ok(refined_segments)
}

fn find_black_frame_end_time(
    input_file: &str,
    total_duration: f64,
    reuse_frames: bool,
    temp_dir: &str,
) -> Result<Option<f64>> {
    println!("Looking for black frames to determine end of last song...");

    // Define the search window (last 40 seconds)
    let search_duration = 40.0;
    let search_start = (total_duration - search_duration).max(0.0);
    let temp_dir = format!("{}/end_frames", temp_dir);

    if reuse_frames {
        println!(
            "Reusing existing end frames from {} for black frame detection",
            temp_dir
        );
    } else {
        io::ensure_dir(&temp_dir)?;
        // Only overwrite directory if not reusing frames or if no frames exist
        io::overwrite_dir(&temp_dir)?;

        // Extract frames at full framerate for the last 40 seconds
        let mut ffmpeg = ffmpeg::create_ffmpeg_command();
        ffmpeg
            .from_to(search_start, total_duration)
            .args(&["-i", input_file])
            .png()
            .args(&[
                "-frame_pts",
                "1",
                "-fps_mode",
                "passthrough", // Use original timestamps
            ])
            .video_filter(&format!("{}/%d.png", temp_dir), vec!["scale=200:100"]);
        let status = ffmpeg
            .cmd()
            // TODO: can we get rid of this particular error without just silencing stderr?
            // [image2 @ 0x132e08570] Application provided invalid, non monotonically increasing dts to muxer in stream 0: 928 >= 928
            .stderr(std::process::Stdio::null())
            .status()?;

        if !status.success() {
            return Err(anyhow!("Failed to extract end frames"));
        }

        println!(
            "Extracted {} end frames for black frame detection",
            search_duration
        );
    }

    // Get list of extracted frames
    let mut frames = fs::read_dir(&temp_dir)?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().map_or(false, |ext| ext == "png"))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();

    println!("Extracted {} frames for end detection", frames.len());

    // Sort frames by frame number
    frames.sort_by(|a, b| {
        frame_number_from_image_filename(a).cmp(&frame_number_from_image_filename(b))
    });

    // Analyze frames to find black frame
    let mut black_frame_time = None;
    let threshold = 25; // Pixel brightness threshold (0-255)

    for frame_path in frames {
        // Parse frame number to get timestamp
        let frame_num = frame_number_from_image_filename(&frame_path);
        let frame_time = search_start + (frame_num as f64 / 30.0); // Approximate timestamp

        // Open image and check if it's black
        match ::image::open(&frame_path) {
            Ok(img) => {
                // Convert to grayscale and analyze pixels
                let pixel_data = img.as_rgb8().unwrap().as_raw();
                let dark_ratio = crate::image::grayscale_darkness(pixel_data, threshold);

                // Check if most pixels are black
                if dark_ratio > 0.80 {
                    println!(
                        "Found black frame at {:.2}s (frame {})",
                        frame_time, frame_num
                    );
                    black_frame_time = Some(frame_time);
                    break;
                }
            }
            Err(e) => {
                println!("Error analyzing frame: {}", e);
                continue;
            }
        }
    }

    // Clean up temporary files
    // fs::remove_dir_all(temp_dir)
    Ok(black_frame_time)
}

/// Status of each expected song after recovery, in set-list order.
#[derive(Clone, Copy, Debug, PartialEq)]
enum RecoveryResult {
    /// Song was already in `segments` before recovery ran.
    AlreadyFound,
    /// Song was missing but a boundary was inserted (audio silence or equal-split).
    Recovered,
    /// Song is still missing (no anchor pair, or the gap couldn't be filled).
    StillMissing,
}

/// Compute the adaptive silence threshold used both for recovery and for the
/// later refinement pass — keeping them identical means the boundaries we
/// insert here are exactly the silences the refinement step would consider.
fn adaptive_silence_threshold(energy_profile: &[f64]) -> f64 {
    let mean_energy: f64 = energy_profile.iter().sum::<f64>() / energy_profile.len() as f64;
    let adaptive = mean_energy * 0.25;
    adaptive
        .min(audio::ENERGY_THRESHOLD)
        .max(audio::ENERGY_THRESHOLD * 0.1)
}

/// For each missing song that sits in an interior gap (between two found
/// boundaries), insert a `SongSegment` whose start time is taken from the
/// longest audio silences inside the gap. Falls back to equal-spacing the gap
/// when no qualifying silence is available.
///
/// Songs missing at the head (before the first found boundary) or tail (after
/// the last) are not recovered here — the head case is handled separately by
/// `first_song_missing_fallback`, and the tail case is out of scope.
///
/// Returns one `RecoveryResult` per song in `set_list` order so the caller can
/// build the still-missing list.
fn recover_missing_songs_from_silence(
    segments: &mut Vec<SongSegment>,
    set_list: &[Song],
    audio_data: &[f32],
) -> Vec<RecoveryResult> {
    let mut results: Vec<RecoveryResult> = set_list
        .iter()
        .map(|song| {
            if segments
                .iter()
                .any(|s| s.song.title.to_lowercase() == song.title.to_lowercase())
            {
                RecoveryResult::AlreadyFound
            } else {
                RecoveryResult::StillMissing
            }
        })
        .collect();

    // Compute silence spans once.
    let energy_profile = audio::calculate_energy_profile(audio_data);
    let threshold = adaptive_silence_threshold(&energy_profile);
    let silence_spans = audio::find_silence_spans(&energy_profile, threshold);

    let mut i = 0;
    while i < set_list.len() {
        if results[i] != RecoveryResult::StillMissing {
            i += 1;
            continue;
        }

        // Find prev anchor (last AlreadyFound or Recovered before i).
        let prev_idx = (0..i).rev().find(|&j| results[j] != RecoveryResult::StillMissing);
        // Find run of missing songs starting at i.
        let mut run_end = i;
        while run_end + 1 < set_list.len() && results[run_end + 1] == RecoveryResult::StillMissing {
            run_end += 1;
        }
        // Find next anchor after run_end.
        let next_idx = ((run_end + 1)..set_list.len())
            .find(|&j| results[j] != RecoveryResult::StillMissing);

        let (prev_idx, next_idx) = match (prev_idx, next_idx) {
            (Some(p), Some(n)) => (p, n),
            // Head or tail run — leave these missing.
            _ => {
                i = run_end + 1;
                continue;
            }
        };

        let prev_segment = find_segment_for_song(segments, &set_list[prev_idx]);
        let next_segment = find_segment_for_song(segments, &set_list[next_idx]);
        let gap_start = prev_segment.segment.start_time;
        let gap_end = next_segment.segment.start_time;
        let missing_count = run_end - i + 1;

        // For each missing slot in chronological order, compute its expected
        // position (assuming equal-length songs) and pick the silence midpoint
        // closest to that position. This is more robust than "longest silence"
        // when the gap contains both an end-of-song silence and a
        // start-of-song silence — proximity to the expected slot picks the
        // right one. Spacing constraints keep two chosen midpoints from
        // landing too close to each other or to the gap endpoints.
        #[derive(Clone, Copy)]
        enum Source {
            Silence,
            EqualSplit,
        }
        let mut chosen: Vec<(f64, Source)> = vec![(0.0, Source::EqualSplit); missing_count];
        let mut filled = vec![false; missing_count];

        let gap_size = gap_end - gap_start;
        let mut candidates: Vec<f64> = silence_spans
            .iter()
            .filter(|s| s.midpoint_seconds > gap_start && s.midpoint_seconds < gap_end)
            .filter(|s| {
                let m = s.midpoint_seconds;
                (m - gap_start).abs() >= audio::MIN_SONG_GAP_SECONDS
                    && (gap_end - m).abs() >= audio::MIN_SONG_GAP_SECONDS
            })
            .map(|s| s.midpoint_seconds)
            .collect();

        for slot in 0..missing_count {
            if candidates.is_empty() {
                break;
            }
            let expected = gap_start + ((slot + 1) as f64) * gap_size / ((missing_count + 1) as f64);
            // Pick the candidate closest to `expected`.
            let (best_i, &best_mid) = candidates
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    (*a - expected)
                        .abs()
                        .partial_cmp(&(*b - expected).abs())
                        .unwrap()
                })
                .unwrap();
            chosen[slot] = (best_mid, Source::Silence);
            filled[slot] = true;
            candidates.remove(best_i);
            // Drop other candidates within the spacing window so a later slot
            // can't pick a near-duplicate.
            candidates.retain(|&m| (m - best_mid).abs() >= audio::MIN_SONG_GAP_SECONDS);
        }

        let unfilled_count = filled.iter().filter(|f| !**f).count();
        if unfilled_count > 0 {
            let missing_titles: Vec<&str> =
                (i..=run_end).map(|j| set_list[j].title.as_str()).collect();
            println!(
                "warning: silence-based recovery only filled {}/{} boundaries in gap {:.2}s–{:.2}s; equally spacing remaining songs: {:?}",
                missing_count - unfilled_count,
                missing_count,
                gap_start,
                gap_end,
                missing_titles
            );
            // Build current anchors (gap endpoints + already-filled slots), sort,
            // then repeatedly bisect the widest subgap to fill unfilled slots.
            let mut anchors: Vec<f64> = Vec::with_capacity(2 + missing_count);
            anchors.push(gap_start);
            anchors.push(gap_end);
            for (slot, was_filled) in filled.iter().enumerate() {
                if *was_filled {
                    anchors.push(chosen[slot].0);
                }
            }
            anchors.sort_by(|a, b| a.partial_cmp(b).unwrap());

            for (slot, was_filled) in filled.iter().enumerate() {
                if *was_filled {
                    continue;
                }
                let (widest_i, _) = anchors
                    .windows(2)
                    .enumerate()
                    .max_by(|(_, a), (_, b)| (a[1] - a[0]).partial_cmp(&(b[1] - b[0])).unwrap())
                    .unwrap();
                let mid = (anchors[widest_i] + anchors[widest_i + 1]) / 2.0;
                chosen[slot] = (mid, Source::EqualSplit);
                anchors.insert(widest_i + 1, mid);
            }
        }

        chosen.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

        for (offset, &(start_time, source)) in chosen.iter().enumerate() {
            let song_idx = i + offset;
            let source_label = match source {
                Source::Silence => "audio silence",
                Source::EqualSplit => "equal-split",
            };
            println!(
                "Recovered missing song '{}' at {:.2}s ({}, between '{}' and '{}')",
                set_list[song_idx].title,
                start_time,
                source_label,
                set_list[prev_idx].title,
                set_list[next_idx].title,
            );
            segments.push(SongSegment {
                song: set_list[song_idx].clone(),
                segment: AudioSegment {
                    start_time,
                    end_time: gap_end,
                    is_song: true,
                },
                // Recovered at a silence/equal-split point, not an overlay estimate.
                start_from_overlay: false,
            });
            results[song_idx] = RecoveryResult::Recovered;
        }

        i = run_end + 1;
    }

    // Re-sort segments by start time so downstream code sees them in order.
    segments.sort_by(|a, b| {
        a.segment
            .start_time
            .partial_cmp(&b.segment.start_time)
            .unwrap()
    });

    // Tighten end_times so each song's end matches the next song's start.
    for i in 0..segments.len() {
        if i + 1 < segments.len() {
            segments[i].segment.end_time = segments[i + 1].segment.start_time;
        }
    }

    results
}

fn find_segment_for_song<'a>(segments: &'a [SongSegment], song: &Song) -> &'a SongSegment {
    segments
        .iter()
        .find(|s| s.song.title.to_lowercase() == song.title.to_lowercase())
        .expect("caller must guarantee the song is present")
}

/// The title overlay typically appears this many seconds AFTER the song actually
/// starts, so an overlay-derived start sits ~this late. When audio silence can't
/// relocate such a start, we pull it back by this amount as a best-effort guess.
const OVERLAY_DELAY_SECONDS: f64 = 3.0;

/// How far back from a detected start to look for a real silence gap to snap to.
/// (Distinct from `OVERLAY_DELAY_SECONDS`, which happens to share the value today.)
const SILENCE_LOOKBACK_SECONDS: f64 = 3.0;

/// Outcome of refining a single song's start time.
#[derive(Debug, PartialEq)]
enum StartRefinement {
    /// Snap back to a real audio silence at this time.
    Snapped(f64),
    /// No silence; pull back to this best-effort start to undo the overlay delay.
    PulledBack(f64),
    /// Leave the start at the originally detected time.
    Unchanged,
}

/// Decide a song's refined start time.
///
/// `nearby_silence` are silence midpoints already filtered to the look-back window
/// `[song_start - SILENCE_LOOKBACK_SECONDS, song_start)`. `prev_song_start` is the
/// previous song's start (None when the previous segment is a gap or absent); the
/// pullback is clamped so it can't shrink the previous song below
/// `audio::MIN_SONG_GAP_SECONDS`. `allow_overlay_pullback` is true only for
/// overlay-derived starts — recovered/silence-placed starts must not be pulled back.
fn refine_start(
    song_start: f64,
    prev_song_start: Option<f64>,
    nearby_silence: &[f64],
    allow_overlay_pullback: bool,
) -> StartRefinement {
    // Prefer snapping to the latest real silence in the window. A detected silence
    // is hard evidence of a real boundary, so — unlike the speculative pullback
    // below — it is intentionally NOT floor-clamped against the previous song's
    // length: we trust the audio over the min-length heuristic. (In practice the
    // window is only SILENCE_LOOKBACK_SECONDS wide, so a snap can't move the start
    // far anyway.)
    if let Some(&silence) = nearby_silence.iter().max_by(|a, b| a.total_cmp(b)) {
        return StartRefinement::Snapped(silence);
    }

    // No silence to snap to: for an overlay-derived start, pull back by the overlay
    // delay, but not so far that the previous song drops below the minimum length.
    if allow_overlay_pullback {
        let floor = prev_song_start
            .map(|p| p + audio::MIN_SONG_GAP_SECONDS)
            .unwrap_or(0.0);
        let new_start = (song_start - OVERLAY_DELAY_SECONDS).max(floor);
        if new_start < song_start {
            return StartRefinement::PulledBack(new_start);
        }
    }

    StartRefinement::Unchanged
}

fn refine_segments_with_audio_analysis(
    segments: &[SongSegment],
    audio_data: &[f32],
    total_duration: f64,
) -> Result<Vec<SongSegment>> {
    println!("Refining song boundaries using audio analysis...");

    // Calculate energy profile from audio data
    let energy_profile = audio::calculate_energy_profile(audio_data);

    // Adaptive threshold calculation (similar to analyze_audio)
    let mean_energy: f64 = energy_profile.iter().sum::<f64>() / energy_profile.len() as f64;
    let adaptive_threshold = mean_energy * 0.25; // 25% of mean energy
    let threshold = adaptive_threshold
        .min(audio::ENERGY_THRESHOLD)
        .max(audio::ENERGY_THRESHOLD * 0.1);

    println!("Using energy threshold for refinement: {:.6}", threshold);

    let silence_spans = audio::find_silence_spans(&energy_profile, threshold);
    let silence_timestamps: Vec<f64> = silence_spans
        .iter()
        .map(|s| s.midpoint_seconds)
        .collect();

    println!(
        "Found {} potential silence points for refinement",
        silence_timestamps.len()
    );

    // Create refined segments
    let mut refined_segments: Vec<SongSegment> = Vec::new();

    for (i, segment) in segments.iter().enumerate() {
        if i == 0 || !segment.segment.is_song {
            // Keep the first segment and non-song segments as they are
            refined_segments.push(segment.clone());
            continue;
        }

        let song_start = segment.segment.start_time;
        let search_start = (song_start - SILENCE_LOOKBACK_SECONDS).max(0.0);

        // Silence points within the look-back window, just before the start.
        let nearby_silence: Vec<f64> = silence_timestamps
            .iter()
            .filter(|&&ts| ts >= search_start && ts < song_start)
            .cloned()
            .collect();

        // Previous (already-finalized) segment's start, only if it is a song.
        let prev_song_start = refined_segments
            .last()
            .filter(|s| s.segment.is_song)
            .map(|s| s.segment.start_time);

        let mut refined = segment.clone();
        let new_start = match refine_start(
            song_start,
            prev_song_start,
            &nearby_silence,
            segment.start_from_overlay,
        ) {
            StartRefinement::Snapped(t) => {
                println!(
                    "Refined song {} start: snapped to silence, {:.2}s -> {:.2}s (-{:.2}s)",
                    i,
                    song_start,
                    t,
                    song_start - t
                );
                Some(t)
            }
            StartRefinement::PulledBack(t) => {
                println!(
                    "Refined song {} start: no silence, estimated start (overlay -{:.2}s), {:.2}s -> {:.2}s",
                    i,
                    song_start - t,
                    song_start,
                    t
                );
                Some(t)
            }
            StartRefinement::Unchanged => None,
        };

        if let Some(new_start) = new_start {
            refined.segment.start_time = new_start;
            // Keep the previous song's end chained to this start.
            if let Some(prev) = refined_segments.last_mut() {
                if prev.segment.is_song {
                    prev.segment.end_time = new_start;
                }
            }
        }
        refined_segments.push(refined);
    }

    // Ensure the last segment ends at the total duration
    if let Some(last) = refined_segments.last_mut() {
        last.segment.end_time = total_duration;
    }

    Ok(refined_segments)
}

fn create_song_timestamps(segments: &[SongSegment], song_list: &[Song]) -> Vec<SongTimestamp> {
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

fn process_segments(
    input_file: &str,
    segments: &[SongSegment],
    concert: ConcertInfo,
    output_dir: &str,
    output_format: OutputFormat,
    video_cut_mode: VideoCutMode,
) -> Result<()> {
    let songs = &concert.set_list;
    println!("Processing {} segments...", segments.len());
    if segments.len() > songs.len() {
        return Err(anyhow!(
            "Too many segments detected. {} segments but only {} songs provided.",
            segments.len(),
            songs.len()
        ));
    }

    let mut song_counter = 0;
    let mut gap_counter = 0;

    for segment in segments.iter() {
        if !segment.segment.is_song {
            // Optionally process gaps
            gap_counter += 1;
            // let output_file = format!("gap_{:02}.mp4", gap_counter);

            println!(
                "ignoring gap {}: {:.2}s to {:.2}s",
                gap_counter, segment.segment.start_time, segment.segment.end_time
            );

            // extract_segment(input_file, &output_file, segment.segment.start_time, segment.segment.end_time, None, None, None)?;
            continue;
        }

        // Process song
        song_counter += 1;

        // Check if we have a song title for this segment
        let song_title = if song_counter <= songs.len() {
            &songs[song_counter - 1].title
        } else {
            // Fallback if we have more segments than songs
            println!("Warning: More song segments detected than provided in setlist. Using default naming.");
            &format!("song_{}", song_counter)
        };

        // Create a safe filename from the song title
        let safe_title = io::sanitize_filename(song_title);

        println!(
            "Extracting {:#?} for song {}: \"{}\" - {:.2}s to {:.2}s (duration: {:.2}s)",
            &output_format,
            song_counter,
            song_title,
            segment.segment.start_time,
            segment.segment.end_time,
            segment.segment.end_time - segment.segment.start_time
        );

        match output_format {
            OutputFormat::Video | OutputFormat::Both => {
                let output_file = format!("{}/{}.mp4", output_dir, safe_title);

                extract_segment(
                    input_file,
                    &output_file,
                    segment.segment.start_time,
                    segment.segment.end_time,
                    video_cut_mode,
                    Some(song_title),
                    &concert,
                    Some(song_counter), // Add song number as track metadata
                )?;
            }
            _ => {}
        }

        match output_format {
            OutputFormat::Audio | OutputFormat::Both => {
                let output_file = format!("{}/{}.m4a", output_dir, safe_title);

                ffmpeg::extract_audio_segment(
                    input_file,
                    &output_file,
                    segment.segment.start_time,
                    segment.segment.end_time,
                    Some(song_title),
                    &concert,
                    Some(song_counter), // Add song number as track metadata
                )?;
            }
            _ => {}
        }
    }

    println!(
        "Successfully extracted {} songs and {} gaps",
        song_counter, gap_counter
    );
    Ok(())
}

fn frame_number_from_image_filename(frame_path: &std::path::PathBuf) -> usize {
    let frame_name = frame_path.file_name().unwrap().to_string_lossy();
    return frame_name
        .strip_suffix(".png")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
}

/// True for a primary extracted frame (`N.png`), false for anything else,
/// including the black-and-white variants (`Nbw.png`) some passes also write into
/// the same directory. Mirrors the filter the detection pass uses (see
/// `extract_frames`): the `bw` suffix makes the stem unparseable, so those files
/// must be excluded from a frame listing or they inflate `frames.len()` and break
/// the index arithmetic in [`refined_match_to_source_frame`].
fn is_source_frame(path: &std::path::Path) -> bool {
    path.extension().map_or(false, |ext| ext == "png")
        && path
            .file_stem()
            .and_then(|s| s.to_str())
            .map_or(false, |s| s.parse::<usize>().is_ok())
}

/// Map a 1-based index into the refined extraction back to a source-video frame
/// index. `earliest_match` is the matched extraction frame (1..=`frame_count`),
/// `frame_count` is the number of extracted frames (which aligns with
/// `end_frame_num`, the source frame at the end of the search window). So the
/// matched frame sits `frame_count - earliest_match` frames before the end.
///
/// `frame_count` MUST be the count of source frames only — counting B/W variants
/// here over-subtracts and pushes the boundary earlier (this was the cause of a
/// song boundary landing ~3s before the overlay actually appeared).
fn refined_match_to_source_frame(
    end_frame_num: usize,
    frame_count: usize,
    earliest_match: usize,
) -> usize {
    end_frame_num - (frame_count - earliest_match)
}

const CROP_TO_TEXT: &str = "scale=400:200,crop=iw/1.5:ih/4:0:160";

pub struct Settings {
    analyze_images: bool,
    reuse_frames: bool,
    ocr_choice: OcrChoice,
}

fn extract_frames(
    input_file: &str,
    temp_dir: &str,
    reuse_frames: bool,
) -> Result<Vec<std::path::PathBuf>> {
    if reuse_frames {
        println!(
            "Reusing existing frames from {} for song title detection...",
            temp_dir
        );
        io::ensure_dir(temp_dir)?;
    } else {
        // Only overwrite directory if not reusing frames or if no frames exist
        io::overwrite_dir(&temp_dir)?;

        println!("Extracting frames every 1 seconds for song title detection...");

        let every_few_seconds = "fps=1,select='not(mod(t,1))'";

        // Extract 1 frame every few seconds
        // focus on the text area
        // Invert colors so the overlay text will be black, which tesseract prefers
        let filters = format!("{},{},{}", every_few_seconds, CROP_TO_TEXT, "negate");

        // Extract frames every 1 seconds with potential text overlays
        let mut ffmpeg = ffmpeg::create_ffmpeg_command();
        // Add command line options to invert the colors
        ffmpeg.args(&[
            "-i",
            input_file,
            "-c:v",
            "png",
            "-frame_pts",
            "1",
            "-fps_mode",
            "passthrough", // Use original timestamps (replaces -vsync 0)
            "-vf",
            &filters,
            &format!("{}/%d.png", temp_dir), // Use sequential numbering
        ]);
        let status = ffmpeg.cmd().status()?;

        println!("Frames extracted successfully for image detection.");

        if !status.success() {
            return Err(anyhow!("Failed to extract frames"));
        }
    }

    // Get list of extracted frames, excluding BW variants and refined subdirectories
    let frames = fs::read_dir(&temp_dir)?
        .filter_map(Result::ok)
        .filter(|entry| {
            let path = entry.path();
            path.extension().map_or(false, |ext| ext == "png")
                && path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map_or(false, |s| s.parse::<usize>().is_ok())
        })
        .map(|entry| entry.path())
        .collect::<Vec<_>>();

    println!("Extracted {} frames, analyzing for text...", frames.len());
    return Ok(frames);
}

const MIN_GAP_FOR_FIRST_SONG_FALLBACK: f64 = 60.0;

/// If exactly one song is missing and the earliest detected song starts
/// well into the video, the missing song almost certainly fills the gap
/// at the beginning. Add it at time 0.
fn first_song_missing_fallback(songs: &[Song], song_title_matched: &mut HashMap<String, f64>) {
    let total_songs = songs.len();
    let matched_songs = song_title_matched.len();
    if matched_songs + 1 != total_songs {
        return;
    }
    let earliest_detected_time = song_title_matched
        .values()
        .copied()
        .min_by(|a, b| a.partial_cmp(b).unwrap())
        .unwrap_or(0.0);
    if earliest_detected_time <= MIN_GAP_FOR_FIRST_SONG_FALLBACK {
        return;
    }
    let matched_titles: std::collections::HashSet<String> =
        song_title_matched.keys().cloned().collect();
    let missing_song: Option<String> = songs
        .iter()
        .map(|s| s.title.to_lowercase())
        .find(|title| !matched_titles.contains(title));
    if let Some(missing) = missing_song {
        println!(
            "Adding missing song '{}' at time 0.0 (first-song fallback: earliest detected song is at {}s)",
            missing, earliest_detected_time
        );
        song_title_matched.insert(missing, 0.0);
    }
}

#[cfg(test)]
mod tests_back_search_offset {
    use super::*;
    use std::path::{Path, PathBuf};

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/blue_back_search")
    }

    fn list_png(dir: &Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().map_or(false, |e| e == "png"))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn is_source_frame_excludes_bw_variants() {
        // The `bw` suffix makes the stem unparseable; such files must be excluded
        // from a refined-frames listing or they inflate the frame count.
        assert!(is_source_frame(Path::new("dir/73.png")));
        assert!(!is_source_frame(Path::new("dir/73bw.png")));
        assert!(!is_source_frame(Path::new("dir/notes.txt")));
        // And the underlying parse quirk that motivates the filter:
        assert_eq!(
            frame_number_from_image_filename(&PathBuf::from("dir/73bw.png")),
            0
        );
    }

    #[test]
    fn refined_listing_filter_drops_bw_variants() {
        // testdata/blue_back_search mirrors a refined-frames directory: the real
        // "Bloc Party / Blue" overlay frames 73,74,75 each alongside a `bw` variant.
        let all_png = list_png(&fixture_dir());
        assert_eq!(all_png.len(), 6, "fixture has a plain + bw file per frame");
        let source_count = all_png.iter().filter(|p| is_source_frame(p.as_path())).count();
        assert_eq!(source_count, 3, "only the three N.png frames are source frames");
    }

    #[test]
    fn refined_match_maps_to_source_frame() {
        // Reproduces the "Blue" boundary: the overlay was matched at frame 73 of 75
        // source frames, the window ending at source video frame 18381 — so the
        // start maps to 18379. Feeding the old, B/W-inflated count (150) instead
        // would over-subtract ~75 frames (~3s), landing on 18304 — the bug that put
        // the boundary at 762.5s when the overlay only appears at ~765s.
        assert_eq!(refined_match_to_source_frame(18381, 75, 73), 18379);
        assert_eq!(
            refined_match_to_source_frame(18381, 150, 73),
            18304,
            "doubling the count via bw variants is what shifted the boundary early"
        );
    }
}

#[cfg(test)]
mod tests_refine_start {
    use super::*;

    #[test]
    fn snaps_to_latest_silence_in_window() {
        let r = refine_start(100.0, Some(40.0), &[97.5, 98.9, 98.2], true);
        assert_eq!(r, StartRefinement::Snapped(98.9));
    }

    #[test]
    fn silence_snap_applies_even_to_non_overlay_starts() {
        // A recovered start still snaps to a real silence if one is present.
        assert_eq!(
            refine_start(100.0, Some(40.0), &[98.0], false),
            StartRefinement::Snapped(98.0)
        );
    }

    #[test]
    fn pulls_back_overlay_start_when_no_silence() {
        assert_eq!(
            refine_start(100.0, Some(40.0), &[], true),
            StartRefinement::PulledBack(97.0)
        );
    }

    #[test]
    fn does_not_pull_back_non_overlay_start() {
        // Recovered / silence-placed / JSON-loaded starts must not be pulled back.
        assert_eq!(
            refine_start(100.0, Some(40.0), &[], false),
            StartRefinement::Unchanged
        );
    }

    #[test]
    fn snap_is_not_floor_clamped() {
        // A real silence wins even when it sits below the min-song-length floor:
        // snapping to detected silence is deliberately NOT clamped (unlike pullback).
        // Here the pullback floor (prev + gap = 119) is past song_start, so a
        // pullback would be Unchanged — but the snap still applies.
        assert_eq!(
            refine_start(100.0, Some(99.0), &[98.5], true),
            StartRefinement::Snapped(98.5)
        );
    }

    #[test]
    fn pulls_back_with_no_previous_song() {
        // No previous song -> floor is 0.0, so the full overlay delay is applied.
        assert_eq!(
            refine_start(50.0, None, &[], true),
            StartRefinement::PulledBack(47.0)
        );
    }

    #[test]
    fn pullback_clamped_to_unchanged_when_prev_song_too_short() {
        // Previous song starts close enough that a full pullback would leave it
        // shorter than MIN_SONG_GAP_SECONDS, and even the floor is past song_start.
        let prev = 100.0 - audio::MIN_SONG_GAP_SECONDS + 1.0; // floor = prev + gap = 101.0
        assert_eq!(
            refine_start(100.0, Some(prev), &[], true),
            StartRefinement::Unchanged
        );
    }

    #[test]
    fn pullback_partial_to_floor_when_min_length_allows_some() {
        // floor = prev + gap sits between song_start-3 and song_start, so we pull
        // back only as far as the floor keeps the previous song long enough.
        let prev = 100.0 - audio::MIN_SONG_GAP_SECONDS - 1.0; // floor = 99.0
        assert_eq!(
            refine_start(100.0, Some(prev), &[], true),
            StartRefinement::PulledBack(99.0)
        );
    }
}

#[cfg(test)]
mod tests_first_song_fallback {
    use super::*;

    fn make_songs(titles: &[&str]) -> Vec<Song> {
        titles
            .iter()
            .map(|t| Song {
                title: t.to_string(),
            })
            .collect()
    }

    #[test]
    fn adds_missing_first_song_when_gap_is_large() {
        let songs = make_songs(&["ohio", "another living soul", "strange fruit", "hujan"]);
        let mut matched = HashMap::new();
        matched.insert("another living soul".to_string(), 291.0);
        matched.insert("strange fruit".to_string(), 676.0);
        matched.insert("hujan".to_string(), 1018.0);

        first_song_missing_fallback(&songs, &mut matched);

        assert_eq!(matched.len(), 4);
        assert_eq!(matched.get("ohio"), Some(&0.0));
    }

    #[test]
    fn does_not_add_when_gap_is_small() {
        let songs = make_songs(&["ohio", "another living soul"]);
        let mut matched = HashMap::new();
        matched.insert("another living soul".to_string(), 30.0);

        first_song_missing_fallback(&songs, &mut matched);

        assert_eq!(matched.len(), 1);
        assert!(!matched.contains_key("ohio"));
    }

    #[test]
    fn does_not_add_when_more_than_one_missing() {
        let songs = make_songs(&["ohio", "another living soul", "strange fruit"]);
        let mut matched = HashMap::new();
        matched.insert("strange fruit".to_string(), 676.0);

        first_song_missing_fallback(&songs, &mut matched);

        assert_eq!(matched.len(), 1);
    }

    #[test]
    fn does_not_add_when_all_matched() {
        let songs = make_songs(&["ohio", "another living soul"]);
        let mut matched = HashMap::new();
        matched.insert("ohio".to_string(), 0.0);
        matched.insert("another living soul".to_string(), 291.0);

        first_song_missing_fallback(&songs, &mut matched);

        assert_eq!(matched.len(), 2);
    }

    #[test]
    fn adds_missing_middle_song_when_gap_is_large() {
        let songs = make_songs(&["ohio", "another living soul", "strange fruit"]);
        let mut matched = HashMap::new();
        matched.insert("ohio".to_string(), 100.0);
        matched.insert("strange fruit".to_string(), 676.0);

        first_song_missing_fallback(&songs, &mut matched);

        assert_eq!(matched.len(), 3);
        assert_eq!(matched.get("another living soul"), Some(&0.0));
    }

    #[test]
    fn does_not_add_at_boundary_of_60s() {
        let songs = make_songs(&["ohio", "another living soul"]);
        let mut matched = HashMap::new();
        matched.insert("another living soul".to_string(), 60.0);

        first_song_missing_fallback(&songs, &mut matched);

        assert_eq!(matched.len(), 1);
        assert!(!matched.contains_key("ohio"));
    }

    #[test]
    fn does_not_add_when_no_songs() {
        let songs = make_songs(&["ohio"]);
        let mut matched: HashMap<String, f64> = HashMap::new();

        first_song_missing_fallback(&songs, &mut matched);

        assert_eq!(matched.len(), 0);
    }

    #[test]
    fn uses_lowercase_title() {
        let songs = make_songs(&["Ohio", "Another Living Soul"]);
        let mut matched: HashMap<String, f64> = HashMap::new();
        matched.insert("another living soul".to_string(), 291.0);

        first_song_missing_fallback(&songs, &mut matched);

        assert_eq!(matched.len(), 2);
        assert_eq!(matched.get("ohio"), Some(&0.0));
    }
}

#[cfg(test)]
mod tests_recover_missing_songs {
    use super::*;
    use crate::audio::frames_per_second;

    fn songs(titles: &[&str]) -> Vec<Song> {
        titles
            .iter()
            .map(|t| Song {
                title: t.to_string(),
            })
            .collect()
    }

    fn segment(title: &str, start: f64) -> SongSegment {
        SongSegment {
            song: Song {
                title: title.to_string(),
            },
            segment: AudioSegment {
                start_time: start,
                end_time: start,
                is_song: true,
            },
            start_from_overlay: false,
        }
    }

    /// Build a synthetic audio waveform that has loud sections interleaved with
    /// silent blocks. `blocks` is a list of (seconds, is_silent) tuples. Loud
    /// sections produce ±0.5 amplitude, silence produces ~0. The resulting
    /// waveform, when run through `calculate_energy_profile` and
    /// `find_silence_spans`, surfaces a span at the right time.
    fn synth_audio(blocks: &[(f64, bool)]) -> Vec<f32> {
        let sr = audio::SAMPLE_RATE as f64;
        let mut samples = Vec::new();
        let mut t: f64 = 0.0;
        for &(seconds, is_silent) in blocks {
            let count = (seconds * sr) as usize;
            for i in 0..count {
                if is_silent {
                    samples.push(0.0);
                } else {
                    // Audible sine-like wave at 100Hz so RMS is well above the
                    // threshold of 0.005.
                    let phase = (t + i as f64 / sr) * 2.0 * std::f64::consts::PI * 100.0;
                    samples.push(0.5 * phase.sin() as f32);
                }
            }
            t += seconds;
        }
        samples
    }

    fn overlay_segment(title: &str, start: f64, end: f64) -> SongSegment {
        SongSegment {
            song: Song {
                title: title.to_string(),
            },
            segment: AudioSegment {
                start_time: start,
                end_time: end,
                is_song: true,
            },
            start_from_overlay: true,
        }
    }

    #[test]
    fn overlay_pullback_chains_previous_end() {
        // All-loud audio -> no silence spans, so the second (overlay) song's start
        // is pulled back by the overlay delay and the first song's end is chained to
        // it. The first song (i == 0) is never pulled back.
        let audio = synth_audio(&[(60.0, false)]);
        let segments = vec![
            overlay_segment("a", 0.0, 30.0),
            overlay_segment("b", 30.0, 60.0),
        ];

        let refined = refine_segments_with_audio_analysis(&segments, &audio, 60.0).unwrap();

        assert_eq!(refined[0].segment.start_time, 0.0, "first song untouched");
        assert_eq!(
            refined[1].segment.start_time,
            30.0 - OVERLAY_DELAY_SECONDS,
            "second song pulled back by the overlay delay"
        );
        assert_eq!(
            refined[0].segment.end_time, refined[1].segment.start_time,
            "previous end chained to the new start (no gap/overlap)"
        );
        assert_eq!(
            refined[1].segment.end_time, 60.0,
            "last song extends to total duration"
        );
    }

    #[test]
    fn k1_recovers_at_obvious_silence_midpoint() {
        // 60s gap with a single 5s silence centered at ~30s.
        let audio = synth_audio(&[
            (10.0, false), // song A body
            (20.0, false), // ...
            (5.0, true),   // silence between A and B (midpoint ~32.5s)
            (25.0, false), // song B body
        ]);
        let set_list = songs(&["A", "B"]);
        let mut segments = vec![segment("A", 0.0), segment("B", 60.0)];
        let results = recover_missing_songs_from_silence(&mut segments, &set_list, &audio);

        // Both songs reported as already-found (we seeded both), so nothing to do.
        assert_eq!(results, vec![RecoveryResult::AlreadyFound; 2]);

        // Now drop B and put a missing song between them.
        let set_list = songs(&["A", "B", "C"]);
        let mut segments = vec![segment("A", 0.0), segment("C", 60.0)];
        let results = recover_missing_songs_from_silence(&mut segments, &set_list, &audio);
        assert_eq!(
            results,
            vec![
                RecoveryResult::AlreadyFound,
                RecoveryResult::Recovered,
                RecoveryResult::AlreadyFound,
            ]
        );
        let b = segments.iter().find(|s| s.song.title == "B").unwrap();
        // Silence centered around 32.5s. Allow generous slack for energy smoothing.
        assert!(
            (b.segment.start_time - 32.5).abs() < 3.0,
            "B placed at {:.2}s",
            b.segment.start_time
        );
    }

    #[test]
    fn k1_picks_silence_closest_to_expected_midpoint() {
        // Two silences in a 100s gap. Gap midpoint is 50s. A silence sits at
        // ~26.5s (closer to the "Carillon ended" side) and another at ~46s
        // (closer to the expected boundary). The shorter-but-better-positioned
        // silence at ~46s must win — picking the longest one regardless of
        // position would mis-attribute the end-of-prev-song pause as the start
        // of the missing song. This regression covers the Sean Shibe case
        // where the longest silence in the gap was right before the next
        // detected song.
        let audio = synth_audio(&[
            (25.0, false),
            (6.0, true),  // longest silence; midpoint ~28s, far from midpoint=50
            (15.0, false),
            (4.0, true),  // shorter; midpoint ~48s, close to midpoint=50
            (50.0, false),
        ]);
        // gap_start=0, gap_end=100, expected midpoint=50.
        let set_list = songs(&["A", "B", "C"]);
        let mut segments = vec![segment("A", 0.0), segment("C", 100.0)];
        let results = recover_missing_songs_from_silence(&mut segments, &set_list, &audio);
        assert_eq!(results[1], RecoveryResult::Recovered);
        let b = segments.iter().find(|s| s.song.title == "B").unwrap();
        assert!(
            (b.segment.start_time - 48.0).abs() < 4.0,
            "expected closest-to-midpoint pick near 48s, got {:.2}s",
            b.segment.start_time
        );
    }

    #[test]
    fn k2_chronological_ordering_of_chosen_silences() {
        // Three qualifying silences: 7s long at ~33s, 6s at ~62s, 5s at ~92s.
        let audio = synth_audio(&[
            (30.0, false),
            (7.0, true),  // longest, mid ~33.5s
            (20.0, false),
            (6.0, true),  // mid ~62.5s
            (20.0, false),
            (5.0, true),  // mid ~92s
            (15.0, false),
        ]);
        let set_list = songs(&["A", "B", "C", "D"]);
        let mut segments = vec![segment("A", 0.0), segment("D", 105.0)];
        let results = recover_missing_songs_from_silence(&mut segments, &set_list, &audio);
        assert_eq!(results[1], RecoveryResult::Recovered);
        assert_eq!(results[2], RecoveryResult::Recovered);

        let b = segments.iter().find(|s| s.song.title == "B").unwrap();
        let c = segments.iter().find(|s| s.song.title == "C").unwrap();
        // Two longest silences are 7s (~33.5s) and 6s (~62.5s); B should land at the earlier one.
        assert!(
            b.segment.start_time < c.segment.start_time,
            "expected B<C chronologically; got B={:.2} C={:.2}",
            b.segment.start_time,
            c.segment.start_time
        );
        assert!((b.segment.start_time - 33.5).abs() < 3.0, "{:?}", b.segment);
        assert!((c.segment.start_time - 62.5).abs() < 3.0, "{:?}", c.segment);
    }

    #[test]
    fn spacing_constraint_forces_equal_split_for_close_silences() {
        // Two silences only ~4s apart inside a 200s gap.
        let audio = synth_audio(&[
            (90.0, false),
            (3.0, true),  // mid ~91.5s
            (1.0, false),
            (3.0, true),  // mid ~96s (only ~4.5s after first)
            (103.0, false),
        ]);
        let set_list = songs(&["A", "B", "C", "D"]);
        let mut segments = vec![segment("A", 0.0), segment("D", 200.0)];
        let results = recover_missing_songs_from_silence(&mut segments, &set_list, &audio);
        // Both B and C should be recovered, but C via equal-split since the
        // second silence is within MIN_SONG_GAP_SECONDS=20s of the first.
        assert_eq!(results[1], RecoveryResult::Recovered);
        assert_eq!(results[2], RecoveryResult::Recovered);
        let b = segments.iter().find(|s| s.song.title == "B").unwrap();
        let c = segments.iter().find(|s| s.song.title == "C").unwrap();
        // The two recovered boundaries must be at least MIN_SONG_GAP_SECONDS apart.
        assert!(
            (c.segment.start_time - b.segment.start_time).abs() >= audio::MIN_SONG_GAP_SECONDS,
            "B={:.2} C={:.2}",
            b.segment.start_time,
            c.segment.start_time
        );
    }

    #[test]
    fn equal_split_fires_when_no_silence_qualifies() {
        // 60s of loud music, no silence at all.
        let audio = synth_audio(&[(60.0, false)]);
        let set_list = songs(&["A", "B", "C"]);
        let mut segments = vec![segment("A", 0.0), segment("C", 60.0)];
        let results = recover_missing_songs_from_silence(&mut segments, &set_list, &audio);
        assert_eq!(results[1], RecoveryResult::Recovered);
        let b = segments.iter().find(|s| s.song.title == "B").unwrap();
        // Equal split between 0 and 60 puts B at 30.
        assert!((b.segment.start_time - 30.0).abs() < 0.001);
    }

    #[test]
    fn missing_at_head_is_not_recovered() {
        let audio = synth_audio(&[(60.0, false)]);
        let set_list = songs(&["A", "B"]);
        // B is found at 30s but A is missing — no anchor before A.
        let mut segments = vec![segment("B", 30.0)];
        let results = recover_missing_songs_from_silence(&mut segments, &set_list, &audio);
        assert_eq!(results[0], RecoveryResult::StillMissing);
        assert_eq!(results[1], RecoveryResult::AlreadyFound);
        assert_eq!(segments.len(), 1, "no segment should have been inserted for A");
    }

    #[test]
    fn missing_at_tail_is_not_recovered() {
        let audio = synth_audio(&[(60.0, false)]);
        let set_list = songs(&["A", "B"]);
        let mut segments = vec![segment("A", 0.0)];
        let results = recover_missing_songs_from_silence(&mut segments, &set_list, &audio);
        assert_eq!(results[0], RecoveryResult::AlreadyFound);
        assert_eq!(results[1], RecoveryResult::StillMissing);
        assert_eq!(segments.len(), 1);
    }

    #[test]
    fn all_found_is_noop() {
        let audio = synth_audio(&[(60.0, false)]);
        let set_list = songs(&["A", "B"]);
        let mut segments = vec![segment("A", 0.0), segment("B", 30.0)];
        let before = segments.clone();
        let results = recover_missing_songs_from_silence(&mut segments, &set_list, &audio);
        assert_eq!(results, vec![RecoveryResult::AlreadyFound; 2]);
        assert_eq!(segments.len(), before.len());
        for (a, b) in segments.iter().zip(before.iter()) {
            assert!((a.segment.start_time - b.segment.start_time).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn end_times_chain_through_inserted_segments() {
        let audio = synth_audio(&[
            (10.0, false),
            (5.0, true),
            (15.0, false),
        ]);
        let set_list = songs(&["A", "B", "C"]);
        let mut segments = vec![segment("A", 0.0), segment("C", 30.0)];
        let _ = recover_missing_songs_from_silence(&mut segments, &set_list, &audio);

        // After recovery, segments should be sorted by start_time and chained:
        // A.end == B.start, B.end == C.start.
        segments.sort_by(|a, b| a.segment.start_time.partial_cmp(&b.segment.start_time).unwrap());
        let a = &segments[0];
        let b = &segments[1];
        let c = &segments[2];
        assert!((a.segment.end_time - b.segment.start_time).abs() < f64::EPSILON);
        assert!((b.segment.end_time - c.segment.start_time).abs() < f64::EPSILON);
    }

    /// Sanity check that the synthetic audio actually surfaces silence at the
    /// expected position via the real audio pipeline — guards against the
    /// energy-smoothing window swallowing short silences in fixture data.
    #[test]
    fn synth_audio_produces_detectable_silence() {
        let audio = synth_audio(&[
            (10.0, false),
            (5.0, true),
            (10.0, false),
        ]);
        let profile = audio::calculate_energy_profile(&audio);
        let threshold = adaptive_silence_threshold(&profile);
        let spans = audio::find_silence_spans(&profile, threshold);
        assert!(!spans.is_empty(), "expected at least one silence span");
        let center = spans[0].midpoint_seconds;
        assert!(
            (center - 12.5).abs() < 2.0,
            "expected silence midpoint near 12.5s, got {:.2}s (fps={:.2})",
            center,
            frames_per_second()
        );
    }
}

fn detect_song_boundaries_from_text(
    input_file: &str,
    artist: &str,
    songs: &[Song],
    video_info: &VideoInfo,
    settings: &Settings,
    temp_dir: &str,
) -> Result<Vec<SongSegment>> {
    let mut frames = extract_frames(input_file, temp_dir, settings.reuse_frames)?;

    let total_duration = video_info.duration;
    let artist_cmp = artist.to_lowercase();

    let mut sorted_songs: Vec<Song> = songs
        .to_vec()
        .iter()
        .map(|song| Song {
            title: song.title.to_lowercase(),
        })
        .collect();
    // sorted_songs.clone_from_slice(songs);
    sorted_songs.sort_by(|a, b| a.title.len().partial_cmp(&b.title.len()).unwrap().reverse());

    // Map to store detected song start times
    let mut song_title_matched: HashMap<String, f64> = HashMap::new();

    // Store potential title-only matches for fallback
    let mut title_only_matches: Vec<(String, f64, usize)> = Vec::new();

    // Process each frame to detect text
    frames.sort_by(|a, b| {
        frame_number_from_image_filename(a).cmp(&frame_number_from_image_filename(b))
    });

    let mut backend = create_ocr_backend(settings.ocr_choice, OcrPhase::Detection)?;
    // Whether to try a binarized fallback pass when the color pass finds no overlay
    // (tesseract: yes; paddle: no).
    let do_bw = backend.options().black_and_white;

    let mut last_song_start_time: Option<f64> = None;
    for mut frame_path in frames {
        // Extract frame number to calculate timestamp
        let frame_num = frame_number_from_image_filename(&frame_path);

        if song_title_matched.len() > 0 {
            if song_title_matched.len() == songs.len() {
                break;
            }
            if let Some(last_start_time) = last_song_start_time {
                // A song must be at least 30 seconds
                if (frame_num as f64) - last_start_time < 30.0 {
                    continue;
                }
            }
        }

        let song_titles_to_match = &sorted_songs
            .iter()
            .filter(|song|
            // skip already matched songs
            !song_title_matched.contains_key(&song.title))
            .map(|song| &song.title)
            .cloned()
            .collect::<Vec<_>>();

        // Candidates accumulate ACROSS the color and (optional) B/W passes so that, when
        // the color pass finds no overlay, the union is matched with the B/W-derived
        // overlay flag (a color-pass line can match with the overlay bonus). The backend
        // owns the OCR fan-out; the pipeline only decides whether to run the B/W pass.
        let mut all_ocr_results: Vec<ocr::OcrParse> = Vec::new();

        let passes: &[bool] = if do_bw { &[false, true] } else { &[false] };
        'convert: for &convert in passes {
            if convert {
                let orig_path = frame_path.clone();
                frame_path.set_file_name(format!("{}bw.png", frame_num));
                crate::image::write_black_and_white(&orig_path, &frame_path)?;
            }
            let frame_path_str = frame_path.to_str().unwrap();

            // OCR this pass (backend fans out internally); propagate the first error.
            let candidates = backend
                .ocr_image_path(frame_path_str, &artist_cmp)
                .into_iter()
                .collect::<Result<Vec<_>>>()?;
            all_ocr_results.extend(candidates.into_iter().map(|c| c.parse));

            // Check if any OCR result contains the artist name (indicates overlay)
            let has_artist_overlay = all_ocr_results.iter().any(|(_, overlay)| *overlay);

            // If we haven't found the overlay, first do the B/W conversion and look for it.
            if !has_artist_overlay && !convert && do_bw {
                continue;
            }
            let ocr_results = std::mem::take(&mut all_ocr_results);

            for ocr_result in &ocr_results {
                // Create a modified OCR result that indicates overlay presence
                let modified_ocr = (ocr_result.0.clone(), has_artist_overlay);

                let title_time = match_song_titles(
                    input_file,
                    &temp_dir,
                    &modified_ocr,
                    song_titles_to_match,
                    &artist_cmp,
                    frame_num,
                    video_info,
                    settings,
                )?;

                if let Some((song, time, overlay)) = title_time {
                    // println!("overlay {}. ocr result {:?}", has_artist_overlay, ocr_result);
                    if overlay {
                        song_title_matched.insert(song, time);
                        last_song_start_time = Some(time);
                        break 'convert; // Found a match, no need to try other OCR results
                    } else {
                        // Store title-only match for potential fallback
                        title_only_matches.push((song, time, frame_num));
                    }
                }
            }
        }
    }

    // Check if we need to use fallback matches (title-only) for missing songs
    let total_songs = songs.len() as i32;
    let matched_songs = song_title_matched.len() as i32;

    if matched_songs < total_songs && !title_only_matches.is_empty() {
        println!("Some songs were not matched yet. Checking title only matches now");

        // Find which song is missing
        let matched_titles: std::collections::HashSet<String> =
            song_title_matched.keys().cloned().collect();
        let missing_songs: Vec<String> = songs
            .iter()
            .map(|s| s.title.to_lowercase())
            .filter(|title| !matched_titles.contains(title))
            .collect();

        for missing_song in missing_songs {
            // Find the best title-only match for the missing song
            let mut best_match: Option<(String, f64, usize)> = None;
            for (song_title, time, frame_num) in &title_only_matches {
                if *song_title == missing_song {
                    if best_match.is_none() || time < &best_match.as_ref().unwrap().1 {
                        best_match = Some((song_title.clone(), *time, *frame_num));
                    }
                }
            }

            if let Some((song, time, frame_num)) = best_match {
                println!("Using fallback title-only match for '{}' at frame {} since all other songs have been matched", song, frame_num);
                song_title_matched.insert(song, time);
            }
        }
    }

    first_song_missing_fallback(songs, &mut song_title_matched);

    // Sort song start times by timestamp
    let mut song_start_times: Vec<(&String, &f64)> = song_title_matched.iter().collect();
    song_start_times.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

    // Create segments from detected song boundaries
    let mut segments = Vec::new();

    if song_start_times.is_empty() {
        println!("No song titles detected in frames. Will fall back to audio analysis.");
        return Ok(Vec::new());
    }

    println!(
        "Detected {} song boundaries from text overlays",
        song_start_times.len()
    );

    // Create segments from the detected song start times
    for i in 0..song_start_times.len() {
        let start_time = if i == 0 {
            // Always set the first song to start at 0 seconds
            0.0
        } else {
            *song_start_times[i].1
        };

        let end_time = if i < song_start_times.len() - 1 {
            *song_start_times[i + 1].1
        } else {
            total_duration
        };

        // Find the corresponding song for this segment
        let song_title = song_start_times[i].0;

        // Find the matching song object from the input list
        let song_obj = songs
            .iter()
            .find(|s| s.title.to_lowercase() == song_title.to_lowercase())
            .cloned()
            .unwrap_or(Song {
                title: song_title.clone(),
            });

        let segment = AudioSegment {
            start_time,
            end_time,
            is_song: true,
        };

        segments.push(SongSegment {
            song: song_obj,
            segment,
            // Start came from the title overlay (~OVERLAY_DELAY_SECONDS late).
            start_from_overlay: true,
        });
    }

    // Note: No need to add a gap at the beginning since first song starts at 0.0

    // Clean up temporary files
    // fs::remove_dir_all(temp_dir)?;

    Ok(segments)
}

fn match_song_titles(
    input_file: &str,
    temp_dir: &str,
    ocr_parse: &ocr::OcrParse,
    song_titles_to_match: &Vec<String>,
    artist_cmp: &str,
    frame_num: usize,
    video_info: &VideoInfo,
    settings: &Settings,
) -> Result<Option<(String, f64, bool)>> {
    let (lines, overlay) = ocr_parse;

    // Format text for display
    let filtered_text = if *overlay {
        lines[1..].to_vec().join("\n")
    } else {
        lines.to_vec().join("\n")
    };

    if *overlay {
        let extra = if lines.len() > 2 { "..." } else { "" };
        println!(
            "Frame {}: Detected overlay: '{}{}'",
            frame_num,
            filtered_text.split("\n").next().unwrap(),
            extra,
        );
    } else {
        /*
        println!("Frame {}: Detected text: '{}'", frame_num, filtered_text);
        */
    }

    // Store all matches, not just the best one
    let mut all_matches: Vec<(String, (ocr::MatchReason, String, u32))> = Vec::new();

    // For an overlay, line 0 is the artist; exclude it so the artist name can't win as a
    // song-title match (e.g. artist "Floetry" is Levenshtein-2 from the song "Floetic",
    // which would steal that song's slot). See
    // docs/change/2026-06-05-artist-line-song-match-fix.md.
    let candidate_lines = song_title_candidate_lines(ocr_parse);

    for song_title in song_titles_to_match {
        if let Some(matched) = matches_song_title(candidate_lines, song_title, *overlay) {
            all_matches.push((song_title.to_string(), matched));
        }
    }

    // Sort matches by Levenshtein distance (lower is better)
    all_matches.sort_by_key(|&(_, (_, _, dist))| dist);

    if all_matches.is_empty() {
        if *overlay {
            println!(
                "Did not find a match for frame {}. {}",
                frame_num,
                lines.to_vec().join("\n")
            )
        }
        return Ok(None);
    }

    // The best match is the first one after sorting
    let (song_title, _) = &all_matches[0];

    // Print all matches, with the best match indicated
    for (i, (match_title, (match_reason, match_line, match_dist))) in all_matches.iter().enumerate()
    {
        if i == 0 {
            if *overlay {
                println!(
                    "Match found! '{}' matches song '{}' frame={} dist={} reason={} (best match)",
                    match_line, match_title, frame_num, match_dist, match_reason,
                );
            } else {
                let overlay_text = if lines.len() > 0 { &lines[0] } else { "" };
                println!(
                    "Skipping best match because no artist. '{}' matches song '{}' frame={} dist={} reason={} (best match)\n{}",
                    match_line, match_title, frame_num, match_dist, match_reason, overlay_text
                );
            }
        } else {
            println!(
                "Other match: '{}' matches song '{}' frame={} dist={} reason={}",
                match_line, match_title, frame_num, match_dist, match_reason,
            );
        }
    }

    // If analyze_images flag is enabled, save the matched image
    if settings.analyze_images {
        let frame_path = std::path::PathBuf::from(format!("{}/{}.png", temp_dir, frame_num));
        save_matched_image(&frame_path, &song_title, frame_num, "initial")?;
    }

    // Don't bother refining
    // TODO: if we don't match a song then look at refined images to see if there is an overlay
    if !*overlay {
        return Ok(Some((song_title.to_string(), frame_num as f64, *overlay)));
    }

    match timestamp_for_song(
        input_file,
        temp_dir,
        &artist_cmp,
        &song_title,
        frame_num,
        video_info,
        settings,
    ) {
        Ok(timestamp) => {
            return Ok(Some((song_title.to_string(), timestamp, *overlay)));
        }
        Err(e) => Err(e),
    }
}

fn timestamp_for_song(
    input_file: &str,
    temp_dir: &str,
    artist_cmp: &str,
    song_title: &str,
    frame_num: usize,
    video_info: &VideoInfo,
    settings: &Settings,
) -> Result<f64> {
    // Extract additional frames around this timestamp for more accurate boundary detection
    let refined_timestamp = refine_song_start_time(
        input_file,
        temp_dir,
        &artist_cmp,
        song_title,
        frame_num,
        video_info,
        settings,
    )?;

    // Use the refined timestamp if available, otherwise use the original
    let final_timestamp = if refined_timestamp > 0.0 && refined_timestamp < (frame_num as f64) {
        refined_timestamp
    } else {
        frame_num as f64
    };
    return Ok(final_timestamp);
}

fn refine_song_start_time(
    input_file: &str,
    temp_dir: &str,
    artist: &str,
    song_title: &str,
    initial_frame_num: usize,
    video_info: &VideoInfo,
    settings: &Settings,
) -> Result<f64> {
    let initial_timestamp = initial_frame_num as f64;
    println!(
        "Refining start time for '{}' (initially at frame {} {}s)...",
        song_title, initial_frame_num, initial_timestamp
    );

    // Define the time window to look before the detected timestamp
    let look_back_seconds = 3;
    let start_time = if initial_timestamp > (look_back_seconds as f64) {
        initial_timestamp - (look_back_seconds as f64)
    } else {
        if initial_timestamp != 0.0 {
            return Err(anyhow!(
                "Initial timestamp is less than look back seconds and not zero!"
            ));
        }
        0.0
    };

    // find an exact frame
    let (_, after_opt, _) = video_info.nearest_frames_by_time(initial_frame_num as f64);
    let (end_frame_num, end_timestamp) = if let Some(after_key_frame) = after_opt {
        (
            after_key_frame,
            video_info.frames[after_key_frame].timestamp,
        )
    } else {
        return Err(anyhow!("Could not find frame after initial timestamp"));
    };
    println!(
        "looking back from frame {} {} after {}",
        end_frame_num, end_timestamp, initial_timestamp
    );

    // Create a subdirectory for the refined frames
    let refined_dir = format!("{}/refined_{}", temp_dir, io::sanitize_filename(song_title));

    // Check if we should reuse existing refined frames
    let frames_exist = std::path::Path::new(&refined_dir).exists()
        && std::fs::read_dir(&refined_dir)
            .map(|entries| entries.count() > 0)
            .unwrap_or(false);

    // Get the original video framerate
    let fps = video_info.framerate;

    if !settings.reuse_frames || !frames_exist {
        // Only overwrite directory if not reusing frames or if no frames exist
        io::overwrite_dir(&refined_dir)?;

        // Extract frames at original video framerate for accuracy.
        // Only the primary `N.png` frames are needed: the matching loop below keys
        // off the frame number parsed from the filename, and a `Nbw.png` variant
        // parses to 0 (see `is_source_frame`), so it could never be selected as the
        // earliest match — extracting it was wasted work that also inflated the
        // frame count used by `refined_match_to_source_frame`.
        let mut ffmpeg = ffmpeg::create_ffmpeg_command();
        ffmpeg
            .from_to(start_time, end_timestamp)
            .args(&["-i", input_file])
            .png()
            .video_filter(
                &format!("{}/%d.png", refined_dir), // Sequential numbering starting from 1
                vec![&format!("fps={}", fps), CROP_TO_TEXT], // Use original video framerate
            );
        let status = ffmpeg.cmd().status()?;

        if !status.success() {
            return Err(anyhow!("Failed to extract refined frames"));
        }

        println!("Extracted refined frames for '{}'", song_title);
    } else {
        println!(
            "Reusing existing refined frames from {} for '{}'",
            refined_dir, song_title
        );
    }

    // Read the refined frames and analyze them
    let mut frames = fs::read_dir(&refined_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_source_frame(path))
        .collect::<Vec<_>>();

    println!(
        "Analyzing {} refined frames for song title '{}' from {}s to {}s at {} fps",
        frames.len(),
        song_title,
        start_time,
        end_timestamp,
        fps
    );

    let mut earliest_match: Option<usize> = None;

    frames.sort_by(|a, b| {
        frame_number_from_image_filename(a)
            .cmp(&frame_number_from_image_filename(b))
            .reverse()
    });
    // `frames` now contains only the primary `N.png` source frames (the listing
    // above filters out B/W variants via `is_source_frame`), so this count is the
    // source-frame count required by `refined_match_to_source_frame`.
    let frame_count = frames.len();

    // The backend fans out internally; each candidate carries the match-leniency to use
    // for it (tesseract: per-PSM stingy/greedy; paddle: its single parse under both).
    let mut backend = create_ocr_backend(settings.ocr_choice, OcrPhase::Refine)?;

    // Process each refined frame
    for frame_path in frames {
        let frame_file = frame_path.to_str().unwrap();
        // Extract frame number
        let frame_num = frame_number_from_image_filename(&frame_path);

        let mut earliest_match_found = false;
        let candidates = backend
            .ocr_image_path(frame_file, artist)
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        for candidate in &candidates {
            let (lines, overlay) = &candidate.parse;
            // If we see the artist overlay that's good enough.
            // On the initial fade in we might be able to see the artist name but not the song title.
            let matched = *overlay
                || matches_song_title_weighted(lines, song_title, *overlay, &candidate.weights)
                    .is_some();
            if matched {
                if earliest_match.is_none() || frame_num < earliest_match.unwrap() {
                    earliest_match = Some(frame_num);
                    earliest_match_found = true;

                    // If analyze_images flag is enabled, save the matched image
                    if settings.analyze_images {
                        save_matched_image(&frame_path, song_title, frame_num, "refined")?;
                    }
                }
            }
        }

        // If we go to an earlier time finding the match becomes harder, so break once we can no longer match
        // wait for earliest_match to be present because of the keyframe adjustment
        if earliest_match.is_some() && !earliest_match_found {
            break;
        }
    }

    // Return the earliest match if found, otherwise 0.0
    match earliest_match {
        Some(earliest_match) if earliest_match > 0 => {
            println!("earliest match frame {:?}/{}", earliest_match, frame_count);
            let earliest_frame_num =
                refined_match_to_source_frame(end_frame_num, frame_count, earliest_match);
            // We never detect the fade soon enough
            // So go back to the previous keyframe
            // This then allows for video splitting without re-encoding
            let frame = video_info.frames[earliest_frame_num];
            let ((_, before_frame), _, _) = video_info.nearest_frames_by_time(frame.timestamp);
            let new_time = video_info.frames[before_frame].timestamp;
            /*
            if earliest_frame_num > 1 {
                earliest_frame_num -= 1;
            }
            let frame = video_info.frames[earliest_frame_num];
            let new_time = frame.timestamp;
            */
            println!(
                "Successfully refined start time for '{}' from {}s to {}s (-{:.2}s) frame {}",
                song_title,
                end_timestamp,
                new_time,
                end_timestamp - new_time,
                earliest_match,
            );
            Ok(new_time)
        }
        _ => {
            println!(
                "Could not find earlier boundary for '{}', keeping original timestamp: {}s. zero={}",
                song_title, initial_timestamp, earliest_match.is_some(),
            );
            return Ok(0.0);
        }
    }
}

/// Build the ffmpeg seek/codec arguments for cutting `input_file` to `[start, end]`
/// under the chosen [`VideoCutMode`]. These slot in after the ffmpeg base flags and
/// before the per-track metadata and output path.
///
/// Both modes use input-side seeking (`-ss` before `-i`) so audio and video start
/// together. The previous command placed `-ss` *after* `-i` with `-c copy`, which
/// left the video starting at the first keyframe *after* the cut while the audio
/// started exactly at the cut — desyncing every track not cut on a keyframe by up to
/// one GOP (e.g. ~1.7s on a 4s-keyframe source). See
/// https://superuser.com/questions/1850814/how-to-cut-a-video-with-ffmpeg-with-no-or-minimal-re-encoding
fn build_cut_args(mode: VideoCutMode, input_file: &str, start: f64, end: f64) -> Vec<String> {
    match mode {
        // Stream copy. Input seeking snaps the start back to the preceding keyframe;
        // `-copyts` keeps the original timeline so `-to` still ends at the true `end`,
        // and `avoid_negative_ts make_zero` shifts the first packet to ~0 so both
        // streams begin together.
        VideoCutMode::Copy => vec![
            "-ss".into(),
            format!("{:.3}", start),
            "-i".into(),
            input_file.into(),
            "-to".into(),
            format!("{:.3}", end),
            "-c".into(),
            "copy".into(),
            "-copyts".into(),
            "-avoid_negative_ts".into(),
            "make_zero".into(),
        ],
        // Re-encode the video for a frame-accurate cut. Input seeking lands on the
        // preceding keyframe (fast); ffmpeg then decodes and discards up to `start`,
        // so the encoded output begins exactly at the detected cut. `-t duration` is
        // used (not `-to`) because the accurate seek resets output timestamps to 0.
        // Audio is still stream-copied (no keyframe constraint, stays in sync).
        VideoCutMode::Reencode => vec![
            "-ss".into(),
            format!("{:.3}", start),
            "-i".into(),
            input_file.into(),
            "-t".into(),
            format!("{:.3}", end - start),
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            REENCODE_PRESET.into(),
            "-crf".into(),
            REENCODE_CRF.into(),
            "-c:a".into(),
            "copy".into(),
        ],
    }
}

fn extract_segment(
    input_file: &str,
    output_file: &str,
    start_time: f64,
    end_time: f64,
    cut_mode: VideoCutMode,
    song_title: Option<&str>,
    concertdata: &ConcertInfo,
    track_number: Option<usize>,
) -> Result<()> {
    let mut ffmpeg = ffmpeg::create_ffmpeg_command();
    ffmpeg.args(build_cut_args(cut_mode, input_file, start_time, end_time));
    let mut cmd = ffmpeg.cmd();

    // Add metadata
    ffmpeg::add_metadata_to_cmd(&mut cmd, song_title, concertdata, track_number);

    cmd.args(&[
        "-y", // Overwrite output file
        output_file,
    ]);

    let status = cmd.status()?;

    if !status.success() {
        return Err(anyhow!("Failed to extract segment to {}", output_file));
    }

    Ok(())
}

#[cfg(test)]
mod tests_cut_args {
    use super::*;

    // Helper: find the value following `flag` in the arg list, if present.
    fn value_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .map(|s| s.as_str())
    }

    fn index_of(args: &[String], item: &str) -> Option<usize> {
        args.iter().position(|a| a == item)
    }

    // Both modes must seek on the *input* side (`-ss` before `-i`); placing `-ss`
    // after `-i` with `-c copy` was the original desync bug.
    #[test]
    fn seek_is_input_side_in_both_modes() {
        for mode in [VideoCutMode::Copy, VideoCutMode::Reencode] {
            let args = build_cut_args(mode, "in.mp4", 10.0, 20.0);
            let ss = index_of(&args, "-ss").expect("-ss present");
            let i = index_of(&args, "-i").expect("-i present");
            assert!(ss < i, "{:?}: -ss must precede -i (got ss={ss}, i={i})", mode);
            assert_eq!(value_after(&args, "-ss"), Some("10.000"));
        }
    }

    // Copy mode stream-copies and preserves the true end via -copyts + -to.
    #[test]
    fn copy_mode_uses_stream_copy_and_true_end() {
        let args = build_cut_args(VideoCutMode::Copy, "in.mp4", 434.337, 770.921);
        assert_eq!(value_after(&args, "-c"), Some("copy"));
        assert!(args.iter().any(|a| a == "-copyts"));
        assert_eq!(value_after(&args, "-avoid_negative_ts"), Some("make_zero"));
        // `-to` is the absolute end on the original timeline, not a duration.
        assert_eq!(value_after(&args, "-to"), Some("770.921"));
        // Copy mode must not re-encode.
        assert!(!args.iter().any(|a| a == "libx264"));
    }

    // Reencode mode re-encodes video, copies audio, and trims by duration because
    // the accurate input seek resets output timestamps to 0.
    #[test]
    fn reencode_mode_uses_duration_and_x264() {
        let args = build_cut_args(VideoCutMode::Reencode, "in.mp4", 434.337, 770.921);
        assert_eq!(value_after(&args, "-c:v"), Some("libx264"));
        assert_eq!(value_after(&args, "-c:a"), Some("copy"));
        assert_eq!(value_after(&args, "-preset"), Some(REENCODE_PRESET));
        assert_eq!(value_after(&args, "-crf"), Some(REENCODE_CRF));
        // `-t` is a duration (end - start), and `-to` is not used.
        assert_eq!(value_after(&args, "-t"), Some("336.584"));
        assert!(!args.iter().any(|a| a == "-to"));
    }

    #[test]
    fn copy_is_the_default_mode() {
        assert_eq!(VideoCutMode::default(), VideoCutMode::Copy);
    }
}

/// Save a matched image to the analysis directory
fn save_matched_image(
    frame_path: &std::path::PathBuf,
    song_title: &str,
    frame_num: usize,
    prefix: &str,
) -> Result<()> {
    // Create analysis directory if it doesn't exist
    let analysis_dir = "analysis/images";
    fs::create_dir_all(analysis_dir)
        .with_context(|| format!("Failed to create analysis directory: {}", analysis_dir))?;

    // Create a sanitized filename from the song title
    let safe_title = io::sanitize_filename(song_title);
    let target_path = format!(
        "{}/{}_{}_{}.png",
        analysis_dir, prefix, safe_title, frame_num
    );

    // Copy the image file to the analysis directory
    fs::copy(frame_path, &target_path).with_context(|| {
        format!(
            "Failed to copy matched image from {} to {}",
            frame_path.display(),
            target_path
        )
    })?;

    Ok(())
}
