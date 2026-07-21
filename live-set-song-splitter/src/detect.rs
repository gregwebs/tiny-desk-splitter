//! Text-overlay detection: find song boundaries by OCR-matching the artist/title
//! overlay Tiny Desk concerts show a few seconds into each song.

use crate::concert_split::{AudioSegment, ConcertSplitProgress, SongSegment};
use crate::ocr::{matches_song_title, matches_song_title_weighted, song_title_candidate_lines};
use crate::ocr_backend::{create_ocr_backend, OcrChoice, OcrPhase};
use crate::video::VideoInfo;
use crate::{ffmpeg, io};
use concert_types::Song;

use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::fs;

pub(crate) const CROP_TO_TEXT: &str = "scale=400:200,crop=iw/1.5:ih/4:0:160";

/// Tuning shared by the detection and refinement passes.
pub(crate) struct Settings {
    pub analyze_images: bool,
    pub reuse_frames: bool,
    pub ocr_choice: OcrChoice,
}

/// Result of the text-overlay detection pass.
pub(crate) struct TextDetection {
    /// One segment per song whose title overlay was detected and matched.
    pub segments: Vec<SongSegment>,
    /// Timestamps (seconds) of frames where the artist overlay was detected but no
    /// song title matched — one earliest timestamp per consecutive cluster. These
    /// mark title cards whose (short/stylized) title was unreadable; they are used as
    /// the preferred boundary anchors for still-missing songs in
    /// [`crate::recover::recover_missing_songs`].
    pub unmatched_overlay_clusters: Vec<f64>,
}

/// A title card stays on screen for several seconds, so an "artist overlay seen but
/// title unreadable" event spans a run of consecutive 1-fps frames. Frames within
/// this many seconds of each other are collapsed into a single cluster.
const OVERLAY_CLUSTER_GAP_SECONDS: f64 = 10.0;

/// Collapse "artist overlay seen but title unreadable" frame numbers into one
/// earliest timestamp per cluster. Detection frames are 1 fps, so a frame number is
/// a timestamp in seconds; consecutive frames within `max_gap_seconds` belong to the
/// same title card. Input need not be sorted.
pub(crate) fn cluster_overlay_frames(frames: &[usize], max_gap_seconds: f64) -> Vec<f64> {
    let mut sorted: Vec<usize> = frames.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    let mut clusters = Vec::new();
    let mut iter = sorted.into_iter();
    if let Some(first) = iter.next() {
        let mut cluster_start = first;
        let mut prev = first;
        for f in iter {
            if (f - prev) as f64 > max_gap_seconds {
                clusters.push(cluster_start as f64);
                cluster_start = f;
            }
            prev = f;
        }
        clusters.push(cluster_start as f64);
    }
    clusters
}

const MIN_GAP_FOR_FIRST_SONG_FALLBACK: f64 = 60.0;

/// If exactly one song is missing and the earliest detected song starts
/// well into the video, the missing song almost certainly fills the gap
/// at the beginning. Add it at time 0.
fn first_song_missing_fallback(
    songs: &[Song],
    song_title_matched: &mut HashMap<String, f64>,
    progress: &mut dyn FnMut(ConcertSplitProgress),
) {
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
        progress(ConcertSplitProgress::Diagnostic(format!(
            "Adding missing song '{}' at time 0.0 (first-song fallback: earliest detected song is at {}s)",
            missing, earliest_detected_time
        )));
        song_title_matched.insert(missing, 0.0);
    }
}

pub(crate) fn frame_number_from_image_filename(frame_path: &std::path::Path) -> usize {
    let frame_name = frame_path.file_name().unwrap().to_string_lossy();
    frame_name
        .strip_suffix(".png")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0)
}

/// True for a primary extracted frame (`N.png`), false for anything else,
/// including the black-and-white variants (`Nbw.png`) some passes also write into
/// the same directory. Mirrors the filter the detection pass uses (see
/// `extract_frames`): the `bw` suffix makes the stem unparseable, so those files
/// must be excluded from a frame listing or they inflate `frames.len()` and break
/// the index arithmetic in [`refined_match_to_source_frame`].
pub(crate) fn is_source_frame(path: &std::path::Path) -> bool {
    path.extension().is_some_and(|ext| ext == "png")
        && path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.parse::<usize>().is_ok())
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
pub(crate) fn refined_match_to_source_frame(
    end_frame_num: usize,
    frame_count: usize,
    earliest_match: usize,
) -> usize {
    end_frame_num - (frame_count - earliest_match)
}

pub(crate) fn extract_frames(
    input_file: &str,
    temp_dir: &str,
    reuse_frames: bool,
    progress: &mut dyn FnMut(ConcertSplitProgress),
) -> Result<Vec<std::path::PathBuf>> {
    if reuse_frames {
        progress(ConcertSplitProgress::Diagnostic(format!(
            "Reusing existing frames from {} for song title detection...",
            temp_dir
        )));
        io::ensure_dir(temp_dir)?;
    } else {
        // Only overwrite directory if not reusing frames or if no frames exist
        io::overwrite_dir(temp_dir)?;

        progress(ConcertSplitProgress::Diagnostic(
            "Extracting frames every 1 seconds for song title detection...".to_string(),
        ));

        let every_few_seconds = "fps=1,select='not(mod(t,1))'";

        // Extract 1 frame every few seconds
        // focus on the text area
        // Invert colors so the overlay text will be black, which tesseract prefers
        let filters = format!("{},{},{}", every_few_seconds, CROP_TO_TEXT, "negate");

        // Extract frames every 1 seconds with potential text overlays
        let mut ffmpeg = ffmpeg::create_ffmpeg_command();
        // Add command line options to invert the colors
        ffmpeg.args([
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

        progress(ConcertSplitProgress::Diagnostic(
            "Frames extracted successfully for image detection.".to_string(),
        ));

        if !status.success() {
            return Err(anyhow!("Failed to extract frames"));
        }
    }

    // Get list of extracted frames, excluding BW variants and refined subdirectories
    let frames = fs::read_dir(temp_dir)?
        .filter_map(Result::ok)
        .filter(|entry| {
            let path = entry.path();
            path.extension().is_some_and(|ext| ext == "png")
                && path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.parse::<usize>().is_ok())
        })
        .map(|entry| entry.path())
        .collect::<Vec<_>>();

    progress(ConcertSplitProgress::Diagnostic(format!(
        "Extracted {} frames, analyzing for text...",
        frames.len()
    )));
    Ok(frames)
}

#[allow(clippy::too_many_arguments)] // All arguments are required for the detection pass
pub(crate) fn detect_song_boundaries_from_text(
    input_file: &str,
    artist: &str,
    songs: &[Song],
    video_info: &VideoInfo,
    settings: &Settings,
    temp_dir: &str,
    progress: &mut dyn FnMut(ConcertSplitProgress),
) -> Result<TextDetection> {
    let mut frames = extract_frames(input_file, temp_dir, settings.reuse_frames, progress)?;

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

    // Frames where the artist overlay was detected but no title matched (e.g. a
    // short/stylized title the OCR couldn't read). Clustered and returned so missing
    // songs can be anchored to a real title card rather than a silence guess.
    let mut unmatched_overlay_frames: Vec<usize> = Vec::new();

    let mut last_song_start_time: Option<f64> = None;
    for mut frame_path in frames {
        // Extract frame number to calculate timestamp
        let frame_num = frame_number_from_image_filename(&frame_path);

        if !song_title_matched.is_empty() {
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
        let mut all_ocr_results: Vec<crate::ocr::OcrParse> = Vec::new();

        // Track, for this frame, whether the artist overlay was seen at all and
        // whether it produced a title match. An overlay seen with no match means a
        // title card we couldn't read — recorded as an unmatched-overlay frame.
        let mut overlay_seen_this_frame = false;
        let mut overlay_matched_this_frame = false;

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
            overlay_seen_this_frame |= has_artist_overlay;

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
                    temp_dir,
                    &modified_ocr,
                    song_titles_to_match,
                    &artist_cmp,
                    frame_num,
                    video_info,
                    settings,
                    progress,
                )?;

                if let Some((song, time, overlay)) = title_time {
                    if overlay {
                        song_title_matched.insert(song, time);
                        last_song_start_time = Some(time);
                        overlay_matched_this_frame = true;
                        break 'convert; // Found a match, no need to try other OCR results
                    } else {
                        // Store title-only match for potential fallback
                        title_only_matches.push((song, time, frame_num));
                    }
                }
            }
        }

        // Overlay present but no title matched: an unreadable title card. Record it
        // as a boundary candidate for a missing song (see `recover_missing_songs`).
        if overlay_seen_this_frame && !overlay_matched_this_frame {
            unmatched_overlay_frames.push(frame_num);
        }
    }

    // Check if we need to use fallback matches (title-only) for missing songs
    let total_songs = songs.len() as i32;
    let matched_songs = song_title_matched.len() as i32;

    if matched_songs < total_songs && !title_only_matches.is_empty() {
        progress(ConcertSplitProgress::Diagnostic(
            "Some songs were not matched yet. Checking title only matches now".to_string(),
        ));

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
                if *song_title == missing_song
                    && (best_match.is_none() || time < &best_match.as_ref().unwrap().1)
                {
                    best_match = Some((song_title.clone(), *time, *frame_num));
                }
            }

            if let Some((song, time, frame_num)) = best_match {
                progress(ConcertSplitProgress::Diagnostic(format!(
                    "Using fallback title-only match for '{}' at frame {} since all other songs have been matched",
                    song, frame_num
                )));
                song_title_matched.insert(song, time);
            }
        }
    }

    first_song_missing_fallback(songs, &mut song_title_matched, progress);

    // Sort song start times by timestamp
    let mut song_start_times: Vec<(&String, &f64)> = song_title_matched.iter().collect();
    song_start_times.sort_by(|a, b| a.1.partial_cmp(b.1).unwrap());

    let unmatched_overlay_clusters =
        cluster_overlay_frames(&unmatched_overlay_frames, OVERLAY_CLUSTER_GAP_SECONDS);
    if !unmatched_overlay_clusters.is_empty() {
        progress(ConcertSplitProgress::Diagnostic(format!(
            "Detected {} unmatched artist-overlay cluster(s) (title unreadable) at: {:?}",
            unmatched_overlay_clusters.len(),
            unmatched_overlay_clusters
        )));
    }

    // Create segments from detected song boundaries
    let mut segments = Vec::new();

    if song_start_times.is_empty() {
        progress(ConcertSplitProgress::Diagnostic(
            "No song titles detected in frames. Will fall back to audio analysis.".to_string(),
        ));
        return Ok(TextDetection {
            segments: Vec::new(),
            unmatched_overlay_clusters,
        });
    }

    progress(ConcertSplitProgress::Diagnostic(format!(
        "Detected {} song boundaries from text overlays",
        song_start_times.len()
    )));

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

    Ok(TextDetection {
        segments,
        unmatched_overlay_clusters,
    })
}

#[allow(clippy::too_many_arguments)] // All arguments are required for per-frame OCR matching
fn match_song_titles(
    input_file: &str,
    temp_dir: &str,
    ocr_parse: &crate::ocr::OcrParse,
    song_titles_to_match: &Vec<String>,
    artist_cmp: &str,
    frame_num: usize,
    video_info: &VideoInfo,
    settings: &Settings,
    progress: &mut dyn FnMut(ConcertSplitProgress),
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
        progress(ConcertSplitProgress::Diagnostic(format!(
            "Frame {}: Detected overlay: '{}{}'",
            frame_num,
            filtered_text.split("\n").next().unwrap(),
            extra,
        )));
    }

    // Store all matches, not just the best one
    let mut all_matches: Vec<(String, (crate::ocr::MatchReason, String, u32))> = Vec::new();

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
            progress(ConcertSplitProgress::Diagnostic(format!(
                "Did not find a match for frame {}. {}",
                frame_num,
                lines.to_vec().join("\n")
            )));
        }
        return Ok(None);
    }

    // The best match is the first one after sorting
    let (song_title, _) = &all_matches[0];

    // Report all matches, with the best match indicated
    for (i, (match_title, (match_reason, match_line, match_dist))) in all_matches.iter().enumerate()
    {
        if i == 0 {
            if *overlay {
                progress(ConcertSplitProgress::Diagnostic(format!(
                    "Match found! '{}' matches song '{}' frame={} dist={} reason={} (best match)",
                    match_line, match_title, frame_num, match_dist, match_reason,
                )));
            } else {
                let overlay_text = if !lines.is_empty() { &lines[0] } else { "" };
                progress(ConcertSplitProgress::Diagnostic(format!(
                    "Skipping best match because no artist. '{}' matches song '{}' frame={} dist={} reason={} (best match)\n{}",
                    match_line, match_title, frame_num, match_dist, match_reason, overlay_text
                )));
            }
        } else {
            progress(ConcertSplitProgress::Diagnostic(format!(
                "Other match: '{}' matches song '{}' frame={} dist={} reason={}",
                match_line, match_title, frame_num, match_dist, match_reason,
            )));
        }
    }

    // If analyze_images flag is enabled, save the matched image
    if settings.analyze_images {
        let frame_path = std::path::PathBuf::from(format!("{}/{}.png", temp_dir, frame_num));
        save_matched_image(&frame_path, song_title, frame_num, "initial")?;
    }

    // Don't bother refining
    // TODO: if we don't match a song then look at refined images to see if there is an overlay
    if !*overlay {
        return Ok(Some((song_title.to_string(), frame_num as f64, *overlay)));
    }

    match timestamp_for_song(
        input_file, temp_dir, artist_cmp, song_title, frame_num, video_info, settings, progress,
    ) {
        Ok(timestamp) => Ok(Some((song_title.to_string(), timestamp, *overlay))),
        Err(e) => Err(e),
    }
}

#[allow(clippy::too_many_arguments)] // All arguments are required for per-frame OCR matching
fn timestamp_for_song(
    input_file: &str,
    temp_dir: &str,
    artist_cmp: &str,
    song_title: &str,
    frame_num: usize,
    video_info: &VideoInfo,
    settings: &Settings,
    progress: &mut dyn FnMut(ConcertSplitProgress),
) -> Result<f64> {
    // Extract additional frames around this timestamp for more accurate boundary detection
    let refined_timestamp = refine_song_start_time(
        input_file, temp_dir, artist_cmp, song_title, frame_num, video_info, settings, progress,
    )?;

    // Use the refined timestamp if available, otherwise use the original
    let final_timestamp = if refined_timestamp > 0.0 && refined_timestamp < (frame_num as f64) {
        refined_timestamp
    } else {
        frame_num as f64
    };
    Ok(final_timestamp)
}

#[allow(clippy::too_many_arguments)] // All arguments are required for per-frame OCR matching
fn refine_song_start_time(
    input_file: &str,
    temp_dir: &str,
    artist: &str,
    song_title: &str,
    initial_frame_num: usize,
    video_info: &VideoInfo,
    settings: &Settings,
    progress: &mut dyn FnMut(ConcertSplitProgress),
) -> Result<f64> {
    let initial_timestamp = initial_frame_num as f64;
    progress(ConcertSplitProgress::Diagnostic(format!(
        "Refining start time for '{}' (initially at frame {} {}s)...",
        song_title, initial_frame_num, initial_timestamp
    )));

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
    progress(ConcertSplitProgress::Diagnostic(format!(
        "looking back from frame {} {} after {}",
        end_frame_num, end_timestamp, initial_timestamp
    )));

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
            .time_range(start_time, end_timestamp)
            .args(["-i", input_file])
            .png()
            .video_filter(
                &format!("{}/%d.png", refined_dir), // Sequential numbering starting from 1
                vec![&format!("fps={}", fps), CROP_TO_TEXT], // Use original video framerate
            );
        let status = ffmpeg.cmd().status()?;

        if !status.success() {
            return Err(anyhow!("Failed to extract refined frames"));
        }

        progress(ConcertSplitProgress::Diagnostic(format!(
            "Extracted refined frames for '{}'",
            song_title
        )));
    } else {
        progress(ConcertSplitProgress::Diagnostic(format!(
            "Reusing existing refined frames from {} for '{}'",
            refined_dir, song_title
        )));
    }

    // Read the refined frames and analyze them
    let mut frames = fs::read_dir(&refined_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_source_frame(path))
        .collect::<Vec<_>>();

    progress(ConcertSplitProgress::Diagnostic(format!(
        "Analyzing {} refined frames for song title '{}' from {}s to {}s at {} fps",
        frames.len(),
        song_title,
        start_time,
        end_timestamp,
        fps
    )));

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
            if matched && (earliest_match.is_none() || frame_num < earliest_match.unwrap()) {
                earliest_match = Some(frame_num);
                earliest_match_found = true;

                // If analyze_images flag is enabled, save the matched image
                if settings.analyze_images {
                    save_matched_image(&frame_path, song_title, frame_num, "refined")?;
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
            progress(ConcertSplitProgress::Diagnostic(format!(
                "earliest match frame {:?}/{}",
                earliest_match, frame_count
            )));
            let earliest_frame_num =
                refined_match_to_source_frame(end_frame_num, frame_count, earliest_match);
            // We never detect the fade soon enough
            // So go back to the previous keyframe
            // This then allows for video splitting without re-encoding
            let frame = video_info.frames[earliest_frame_num];
            let ((_, before_frame), _, _) = video_info.nearest_frames_by_time(frame.timestamp);
            let new_time = video_info.frames[before_frame].timestamp;
            progress(ConcertSplitProgress::Diagnostic(format!(
                "Successfully refined start time for '{}' from {}s to {}s (-{:.2}s) frame {}",
                song_title,
                end_timestamp,
                new_time,
                end_timestamp - new_time,
                earliest_match,
            )));
            Ok(new_time)
        }
        _ => {
            progress(ConcertSplitProgress::Diagnostic(format!(
                "Could not find earlier boundary for '{}', keeping original timestamp: {}s. zero={}",
                song_title, initial_timestamp, earliest_match.is_some(),
            )));
            Ok(0.0)
        }
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
            .filter(|p| p.extension().is_some_and(|e| e == "png"))
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
        let source_count = all_png
            .iter()
            .filter(|p| is_source_frame(p.as_path()))
            .count();
        assert_eq!(
            source_count, 3,
            "only the three N.png frames are source frames"
        );
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

    fn no_progress(_event: ConcertSplitProgress) {}

    #[test]
    fn adds_missing_first_song_when_gap_is_large() {
        let songs = make_songs(&["ohio", "another living soul", "strange fruit", "hujan"]);
        let mut matched = HashMap::new();
        matched.insert("another living soul".to_string(), 291.0);
        matched.insert("strange fruit".to_string(), 676.0);
        matched.insert("hujan".to_string(), 1018.0);

        first_song_missing_fallback(&songs, &mut matched, &mut no_progress);

        assert_eq!(matched.len(), 4);
        assert_eq!(matched.get("ohio"), Some(&0.0));
    }

    #[test]
    fn does_not_add_when_gap_is_small() {
        let songs = make_songs(&["ohio", "another living soul"]);
        let mut matched = HashMap::new();
        matched.insert("another living soul".to_string(), 30.0);

        first_song_missing_fallback(&songs, &mut matched, &mut no_progress);

        assert_eq!(matched.len(), 1);
        assert!(!matched.contains_key("ohio"));
    }

    #[test]
    fn does_not_add_when_more_than_one_missing() {
        let songs = make_songs(&["ohio", "another living soul", "strange fruit"]);
        let mut matched = HashMap::new();
        matched.insert("strange fruit".to_string(), 676.0);

        first_song_missing_fallback(&songs, &mut matched, &mut no_progress);

        assert_eq!(matched.len(), 1);
    }

    #[test]
    fn does_not_add_when_all_matched() {
        let songs = make_songs(&["ohio", "another living soul"]);
        let mut matched = HashMap::new();
        matched.insert("ohio".to_string(), 0.0);
        matched.insert("another living soul".to_string(), 291.0);

        first_song_missing_fallback(&songs, &mut matched, &mut no_progress);

        assert_eq!(matched.len(), 2);
    }

    #[test]
    fn adds_missing_middle_song_when_gap_is_large() {
        let songs = make_songs(&["ohio", "another living soul", "strange fruit"]);
        let mut matched = HashMap::new();
        matched.insert("ohio".to_string(), 100.0);
        matched.insert("strange fruit".to_string(), 676.0);

        first_song_missing_fallback(&songs, &mut matched, &mut no_progress);

        assert_eq!(matched.len(), 3);
        assert_eq!(matched.get("another living soul"), Some(&0.0));
    }

    #[test]
    fn does_not_add_at_boundary_of_60s() {
        let songs = make_songs(&["ohio", "another living soul"]);
        let mut matched = HashMap::new();
        matched.insert("another living soul".to_string(), 60.0);

        first_song_missing_fallback(&songs, &mut matched, &mut no_progress);

        assert_eq!(matched.len(), 1);
        assert!(!matched.contains_key("ohio"));
    }

    #[test]
    fn does_not_add_when_no_songs() {
        let songs = make_songs(&["ohio"]);
        let mut matched: HashMap<String, f64> = HashMap::new();

        first_song_missing_fallback(&songs, &mut matched, &mut no_progress);

        assert_eq!(matched.len(), 0);
    }

    #[test]
    fn uses_lowercase_title() {
        let songs = make_songs(&["Ohio", "Another Living Soul"]);
        let mut matched: HashMap<String, f64> = HashMap::new();
        matched.insert("another living soul".to_string(), 291.0);

        first_song_missing_fallback(&songs, &mut matched, &mut no_progress);

        assert_eq!(matched.len(), 2);
        assert_eq!(matched.get("ohio"), Some(&0.0));
    }
}

#[cfg(test)]
mod tests_cluster_overlay_frames {
    use super::*;

    #[test]
    fn collapses_consecutive_run_to_earliest() {
        // The yeule VV overlay: frames 262..=265 -> one cluster at 262.
        assert_eq!(
            cluster_overlay_frames(&[262, 263, 264, 265], 10.0),
            vec![262.0]
        );
    }

    #[test]
    fn separates_runs_beyond_gap() {
        assert_eq!(
            cluster_overlay_frames(&[262, 263, 400, 401], 10.0),
            vec![262.0, 400.0]
        );
    }

    #[test]
    fn sorts_and_dedups_input() {
        assert_eq!(
            cluster_overlay_frames(&[265, 262, 264, 263, 263], 10.0),
            vec![262.0]
        );
    }

    #[test]
    fn empty_and_single() {
        assert!(cluster_overlay_frames(&[], 10.0).is_empty());
        assert_eq!(cluster_overlay_frames(&[42], 10.0), vec![42.0]);
    }

    #[test]
    fn gap_exactly_at_threshold_stays_in_cluster() {
        // 262 and 272 are exactly 10s apart; `> max_gap` splits, so this stays one.
        assert_eq!(cluster_overlay_frames(&[262, 272], 10.0), vec![262.0]);
        // 11s apart -> two clusters.
        assert_eq!(
            cluster_overlay_frames(&[262, 273], 10.0),
            vec![262.0, 273.0]
        );
    }
}
