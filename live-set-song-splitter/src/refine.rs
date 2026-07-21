//! Audio-analysis refinement of detected/recovered song boundaries.

use crate::concert_split::{ConcertSplitProgress, SongSegment};
use crate::detect::frame_number_from_image_filename;
use crate::recover::adaptive_silence_threshold;
use crate::{audio, ffmpeg, io};

use anyhow::{anyhow, Result};
use std::fs;

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

pub(crate) fn refine_segments_with_audio_analysis(
    segments: &[SongSegment],
    audio_data: &[f32],
    total_duration: f64,
    progress: &mut dyn FnMut(ConcertSplitProgress),
) -> Result<Vec<SongSegment>> {
    progress(ConcertSplitProgress::Diagnostic(
        "Refining song boundaries using audio analysis...".to_string(),
    ));

    // Calculate energy profile from audio data
    let energy_profile = audio::calculate_energy_profile(audio_data);

    // Same adaptive-threshold formula `recover::adaptive_silence_threshold` uses for
    // recovery — kept as one shared function so the two passes can't drift apart.
    let threshold = adaptive_silence_threshold(&energy_profile);

    progress(ConcertSplitProgress::Diagnostic(format!(
        "Using energy threshold for refinement: {:.6}",
        threshold
    )));

    let silence_spans = audio::find_silence_spans(&energy_profile, threshold);
    let silence_timestamps: Vec<f64> = silence_spans.iter().map(|s| s.midpoint_seconds).collect();

    progress(ConcertSplitProgress::Diagnostic(format!(
        "Found {} potential silence points for refinement",
        silence_timestamps.len()
    )));

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
                progress(ConcertSplitProgress::Diagnostic(format!(
                    "Refined song {} start: snapped to silence, {:.2}s -> {:.2}s (-{:.2}s)",
                    i,
                    song_start,
                    t,
                    song_start - t
                )));
                Some(t)
            }
            StartRefinement::PulledBack(t) => {
                progress(ConcertSplitProgress::Diagnostic(format!(
                    "Refined song {} start: no silence, estimated start (overlay -{:.2}s), {:.2}s -> {:.2}s",
                    i,
                    song_start - t,
                    song_start,
                    t
                )));
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

pub(crate) fn refine_last_song_end_time(
    input_file: &str,
    segments: Vec<SongSegment>,
    total_duration: f64,
    reuse_frames: bool,
    temp_dir: &str,
    progress: &mut dyn FnMut(ConcertSplitProgress),
) -> Result<Vec<SongSegment>> {
    // Find the last song segment
    let mut refined_segments = segments;
    if let Some(last_idx) = refined_segments.iter().rposition(|seg| seg.segment.is_song) {
        progress(ConcertSplitProgress::Diagnostic(
            "Finding precise end time for the last song...".to_string(),
        ));

        // Get the current end time of the last song
        let current_end = refined_segments[last_idx].segment.end_time;

        // Try to find a black frame to use as the end time
        if let Some(black_frame_time) =
            find_black_frame_end_time(input_file, total_duration, reuse_frames, temp_dir, progress)?
        {
            progress(ConcertSplitProgress::Diagnostic(format!(
                "Adjusted last song end time from {:.2}s to {:.2}s (found black frame)",
                current_end, black_frame_time
            )));
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
    progress: &mut dyn FnMut(ConcertSplitProgress),
) -> Result<Option<f64>> {
    progress(ConcertSplitProgress::Diagnostic(
        "Looking for black frames to determine end of last song...".to_string(),
    ));

    // Define the search window (last 40 seconds)
    let search_duration = 40.0;
    let search_start = (total_duration - search_duration).max(0.0);
    let temp_dir = format!("{}/end_frames", temp_dir);

    if reuse_frames {
        progress(ConcertSplitProgress::Diagnostic(format!(
            "Reusing existing end frames from {} for black frame detection",
            temp_dir
        )));
    } else {
        io::ensure_dir(&temp_dir)?;
        // Only overwrite directory if not reusing frames or if no frames exist
        io::overwrite_dir(&temp_dir)?;

        // Extract frames at full framerate for the last 40 seconds
        let mut ffmpeg = ffmpeg::create_ffmpeg_command();
        ffmpeg
            .time_range(search_start, total_duration)
            .args(["-i", input_file])
            .png()
            .args([
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

        progress(ConcertSplitProgress::Diagnostic(format!(
            "Extracted {} end frames for black frame detection",
            search_duration
        )));
    }

    // Get list of extracted frames
    let mut frames = fs::read_dir(&temp_dir)?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "png"))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();

    progress(ConcertSplitProgress::Diagnostic(format!(
        "Extracted {} frames for end detection",
        frames.len()
    )));

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
                    progress(ConcertSplitProgress::Diagnostic(format!(
                        "Found black frame at {:.2}s (frame {})",
                        frame_time, frame_num
                    )));
                    black_frame_time = Some(frame_time);
                    break;
                }
            }
            Err(e) => {
                progress(ConcertSplitProgress::Warning(format!(
                    "Error analyzing frame: {}",
                    e
                )));
                continue;
            }
        }
    }

    // Clean up temporary files
    // fs::remove_dir_all(temp_dir)
    Ok(black_frame_time)
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
mod tests_refine_segments {
    use super::*;
    use crate::concert_split::AudioSegment;
    use concert_types::Song;

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

    /// Mirrors `recover.rs`'s synthetic audio helper: loud sections interleaved
    /// with silent blocks, so `refine_segments_with_audio_analysis` sees a
    /// deterministic energy profile.
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
                    let phase = (t + i as f64 / sr) * 2.0 * std::f64::consts::PI * 100.0;
                    samples.push(0.5 * phase.sin() as f32);
                }
            }
            t += seconds;
        }
        samples
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

        let refined =
            refine_segments_with_audio_analysis(&segments, &audio, 60.0, &mut |_| {}).unwrap();

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
}
