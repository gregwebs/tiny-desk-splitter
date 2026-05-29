use crate::ffmpeg::create_ffmpeg_command;
use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufReader, Read};

pub const SAMPLE_RATE: u32 = 44100;
pub const WINDOW_SIZE: usize = 4096;
pub const HOP_SIZE: usize = 1024;
pub const MIN_SILENCE_DURATION: f64 = 2.0; // Seconds of silence to detect a gap
pub const ENERGY_THRESHOLD: f64 = 0.005; // Threshold for audio energy detection (lowered for better sensitivity)

/// Minimum spacing between two recovered song boundaries (and between a
/// recovered boundary and the surrounding anchors). Without this guard, two
/// long silences clustered at one end of a gap could place "different" songs
/// only a few seconds apart.
pub const MIN_SONG_GAP_SECONDS: f64 = 20.0;

/// How far before a detected song start to look for an earlier silent point to
/// snap the start to (overlays appear a few seconds after a song begins, so the
/// true boundary is usually a bit earlier).
pub const REFINE_LOOK_BACK_SECONDS: f64 = 3.0;

/// Fallback forward look: when nothing is found in the backward window, look
/// this far ahead of the detected start for a genuine drop into silence.
/// Invariant: REFINE_LOOK_FORWARD_SECONDS (2.0) << MIN_SONG_GAP_SECONDS (20.0),
/// which keeps a forward silence from ever reaching the next song. Recheck this
/// if the window is ever widened.
pub const REFINE_LOOK_FORWARD_SECONDS: f64 = 2.0;

/// A forward silence only counts as a real drop if the **max** lead-in energy
/// (between the detected start and the silence onset) reaches at least this
/// multiple of the silence threshold — i.e. real sound was playing before the
/// silence, rather than an already-quiet passage. Sole tuning knob; revisit
/// after verifying on real audio. (Max, not mean: we ask "was there ever clear
/// sound here", which a mean would dilute as it approaches the silence.)
pub const FORWARD_DROP_SOUND_FACTOR: f64 = 2.0;

/// Energy-profile frames per audio second.
pub fn frames_per_second() -> f64 {
    SAMPLE_RATE as f64 / HOP_SIZE as f64
}

/// A contiguous silent region in the energy profile, expressed in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SilenceSpan {
    pub midpoint_seconds: f64,
    pub duration_seconds: f64,
}

pub fn extract_audio_waveform(input_file: &str) -> Result<Vec<f32>> {
    // Create a temporary WAV file
    let temp_wav = "temp_audio.wav";

    // Extract audio to WAV using FFmpeg
    let status = create_ffmpeg_command()
        .cmd()
        .args(&[
            "-i",
            input_file,
            "-vn", // No video
            "-acodec",
            "pcm_s16le", // PCM signed 16-bit little-endian
            "-ar",
            &SAMPLE_RATE.to_string(), // Sample rate
            "-ac",
            "1",  // Mono channel
            "-y", // Overwrite output file
            temp_wav,
        ])
        .status()?;

    if !status.success() {
        return Err(anyhow::anyhow!("Failed to extract audio to WAV"));
    }

    // Read the WAV file
    let file = File::open(temp_wav)
        .with_context(|| format!("Failed to open temporary WAV file: {}", temp_wav))?;
    let mut reader = BufReader::new(file);
    let mut buffer = Vec::new();
    reader.read_to_end(&mut buffer)?;

    // WAV header is 44 bytes, then PCM data follows
    let pcm_data = &buffer[44..];

    // Convert bytes to samples (16-bit signed integers)
    let mut samples = Vec::new();
    for i in 0..(pcm_data.len() / 2) {
        let sample = i16::from_le_bytes([pcm_data[i * 2], pcm_data[i * 2 + 1]]);
        samples.push(sample as f32 / 32768.0); // Normalize to [-1.0, 1.0]
    }

    // Clean up
    std::fs::remove_file(temp_wav)?;

    Ok(samples)
}

pub fn calculate_energy_profile(samples: &[f32]) -> Vec<f64> {
    let mut energy_profile = Vec::new();

    // Calculate RMS energy for each window with hop_size step
    for window_start in (0..samples.len()).step_by(HOP_SIZE) {
        if window_start + WINDOW_SIZE > samples.len() {
            break;
        }

        // Calculate RMS for this window
        let sum_squared: f32 = samples[window_start..(window_start + WINDOW_SIZE)]
            .iter()
            .map(|&s| s * s)
            .sum();

        let rms = (sum_squared / WINDOW_SIZE as f32).sqrt();
        energy_profile.push(rms as f64);
    }

    // Apply a simple moving average to smooth the energy profile
    let window_size = (SAMPLE_RATE as usize / HOP_SIZE) / 2; // ~0.5 second window
    let mut smoothed_profile = Vec::with_capacity(energy_profile.len());

    for i in 0..energy_profile.len() {
        let start = if i < window_size { 0 } else { i - window_size };
        let end = std::cmp::min(i + window_size + 1, energy_profile.len());
        let avg = energy_profile[start..end].iter().sum::<f64>() / (end - start) as f64;
        smoothed_profile.push(avg);
    }

    smoothed_profile
}

/// Find all silence spans in the energy profile whose duration meets
/// `MIN_SILENCE_DURATION`. Returns the midpoint and duration of each span
/// in seconds.
pub fn find_silence_spans(energy_profile: &[f64], threshold: f64) -> Vec<SilenceSpan> {
    let mut spans = Vec::new();
    let fps = frames_per_second();
    let min_silence_frames = (MIN_SILENCE_DURATION * fps) as usize;

    let mut silence_start: Option<usize> = None;
    let mut silence_length = 0;

    let record_span = |start: usize, length: usize, spans: &mut Vec<SilenceSpan>| {
        if length >= min_silence_frames {
            let midpoint_frame = start + length / 2;
            let span = SilenceSpan {
                midpoint_seconds: midpoint_frame as f64 / fps,
                duration_seconds: length as f64 / fps,
            };
            log::debug!(
                "Silence detected at {:.2}s (length: {:.2}s)",
                span.midpoint_seconds,
                span.duration_seconds
            );
            spans.push(span);
        }
    };

    for (i, &energy) in energy_profile.iter().enumerate() {
        if energy < threshold {
            if silence_start.is_none() {
                silence_start = Some(i);
            }
            silence_length += 1;
        } else if let Some(start) = silence_start.take() {
            record_span(start, silence_length, &mut spans);
            silence_length = 0;
        }
    }

    if let Some(start) = silence_start {
        record_span(start, silence_length, &mut spans);
    }

    spans
}

/// Find the earliest silence ahead of `song_start` (within `look_forward`
/// seconds) that represents a genuine sound→silence transition: there must be
/// energy >= `FORWARD_DROP_SOUND_FACTOR * threshold` between `song_start` and
/// the silence onset (real sound was playing, then it dropped to silence —
/// rather than an already-quiet passage). Returns the chosen silence midpoint
/// in seconds, or `None` if no qualifying drop is found.
pub fn find_forward_drop(
    energy_profile: &[f64],
    silence_spans: &[SilenceSpan],
    threshold: f64,
    song_start: f64,
    look_forward: f64,
) -> Option<f64> {
    let fps = frames_per_second();
    let window_end = song_start + look_forward;
    let sound_floor = FORWARD_DROP_SOUND_FACTOR * threshold;

    // Spans are produced in time order, so the first qualifying one is earliest.
    for span in silence_spans {
        let midpoint = span.midpoint_seconds;
        if midpoint <= song_start || midpoint > window_end {
            continue;
        }

        // A silence that started at/before the detected start is not a forward
        // drop (there is no sound→silence transition ahead of us).
        let onset = midpoint - span.duration_seconds / 2.0;
        if onset <= song_start {
            continue;
        }

        // Require clearly-above-threshold sound in the lead-in [song_start, onset).
        let start_frame = (song_start * fps) as usize;
        let end_frame = ((onset * fps) as usize).min(energy_profile.len());
        if start_frame >= end_frame {
            continue;
        }
        let lead_in_max = energy_profile[start_frame..end_frame]
            .iter()
            .cloned()
            .fold(f64::MIN, f64::max);
        if lead_in_max >= sound_floor {
            return Some(midpoint);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frames_per_second_matches_constants() {
        let expected = SAMPLE_RATE as f64 / HOP_SIZE as f64;
        assert!((frames_per_second() - expected).abs() < f64::EPSILON);
    }

    /// Build an energy profile with explicit (loud-frames, silent-frames) blocks.
    fn build_profile(blocks: &[(usize, bool)]) -> Vec<f64> {
        let loud = 0.5_f64;
        let silent = 0.0001_f64;
        let mut out = Vec::new();
        for &(frames, is_silent) in blocks {
            let value = if is_silent { silent } else { loud };
            for _ in 0..frames {
                out.push(value);
            }
        }
        out
    }

    #[test]
    fn test_find_silence_spans_reports_midpoint_and_duration() {
        let fps = frames_per_second();
        // 5s loud, 4s silent, 5s loud, 6s silent, 5s loud
        let profile = build_profile(&[
            ((5.0 * fps) as usize, false),
            ((4.0 * fps) as usize, true),
            ((5.0 * fps) as usize, false),
            ((6.0 * fps) as usize, true),
            ((5.0 * fps) as usize, false),
        ]);

        let spans = find_silence_spans(&profile, ENERGY_THRESHOLD);
        assert_eq!(
            spans.len(),
            2,
            "expected two silence spans, got {:?}",
            spans
        );

        // First silence: starts at 5s, runs 4s -> midpoint ~7s, duration ~4s.
        assert!(
            (spans[0].midpoint_seconds - 7.0).abs() < 0.2,
            "{:?}",
            spans[0]
        );
        assert!(
            (spans[0].duration_seconds - 4.0).abs() < 0.2,
            "{:?}",
            spans[0]
        );

        // Second silence: starts at 14s, runs 6s -> midpoint ~17s, duration ~6s.
        assert!(
            (spans[1].midpoint_seconds - 17.0).abs() < 0.2,
            "{:?}",
            spans[1]
        );
        assert!(
            (spans[1].duration_seconds - 6.0).abs() < 0.2,
            "{:?}",
            spans[1]
        );
    }

    #[test]
    fn test_find_silence_spans_filters_short_silences() {
        let fps = frames_per_second();
        // 5s loud, 1s silent (below MIN_SILENCE_DURATION=2.0), 5s loud
        let profile = build_profile(&[
            ((5.0 * fps) as usize, false),
            ((1.0 * fps) as usize, true),
            ((5.0 * fps) as usize, false),
        ]);
        let spans = find_silence_spans(&profile, ENERGY_THRESHOLD);
        assert!(spans.is_empty(), "expected no spans, got {:?}", spans);
    }

    #[test]
    fn test_find_silence_spans_handles_trailing_silence() {
        let fps = frames_per_second();
        let profile = build_profile(&[((5.0 * fps) as usize, false), ((3.0 * fps) as usize, true)]);
        let spans = find_silence_spans(&profile, ENERGY_THRESHOLD);
        assert_eq!(spans.len(), 1);
        assert!(
            (spans[0].duration_seconds - 3.0).abs() < 0.2,
            "{:?}",
            spans[0]
        );
    }

    /// Build an energy profile from explicit (seconds, energy-value) blocks.
    fn build_profile_vals(blocks: &[(f64, f64)]) -> Vec<f64> {
        let fps = frames_per_second();
        let mut out = Vec::new();
        for &(secs, value) in blocks {
            for _ in 0..((secs * fps) as usize) {
                out.push(value);
            }
        }
        out
    }

    #[test]
    fn test_find_forward_drop_sound_then_silence_returns_midpoint() {
        // 1s loud, 2s silent, 2s loud. Detected start at 0.5s, silence onset at 1s.
        let profile = build_profile_vals(&[(1.0, 0.5), (2.0, 0.0001), (2.0, 0.5)]);
        let spans = find_silence_spans(&profile, ENERGY_THRESHOLD);
        let drop = find_forward_drop(&profile, &spans, ENERGY_THRESHOLD, 0.5, 2.0);
        let mid = drop.expect("expected a forward drop");
        assert!((mid - 2.0).abs() < 0.3, "midpoint was {mid}");
    }

    #[test]
    fn test_find_forward_drop_already_quiet_lead_in_returns_none() {
        // Lead-in (0.008) is above the silence threshold but below the sound
        // floor (FORWARD_DROP_SOUND_FACTOR * threshold = 0.01), so it is not a
        // genuine sound→silence drop.
        let profile = build_profile_vals(&[(0.5, 0.008), (2.5, 0.0001), (1.0, 0.5)]);
        let spans = find_silence_spans(&profile, ENERGY_THRESHOLD);
        assert!(find_forward_drop(&profile, &spans, ENERGY_THRESHOLD, 0.0, 2.0).is_none());
    }

    #[test]
    fn test_find_forward_drop_silence_beyond_window_returns_none() {
        // 3s loud then silence: the silence is past the 2s forward window.
        let profile = build_profile_vals(&[(3.0, 0.5), (3.0, 0.0001), (1.0, 0.5)]);
        let spans = find_silence_spans(&profile, ENERGY_THRESHOLD);
        assert!(find_forward_drop(&profile, &spans, ENERGY_THRESHOLD, 0.0, 2.0).is_none());
    }

    #[test]
    fn test_find_forward_drop_returns_earliest_of_two() {
        // Two qualifying silences; with a wide look-forward both are candidates.
        let profile = build_profile_vals(&[
            (1.0, 0.5),
            (2.0, 0.0001),
            (1.0, 0.5),
            (2.0, 0.0001),
            (2.0, 0.5),
        ]);
        let spans = find_silence_spans(&profile, ENERGY_THRESHOLD);
        let mid = find_forward_drop(&profile, &spans, ENERGY_THRESHOLD, 0.5, 10.0)
            .expect("expected a forward drop");
        // First silence (onset 1s, ~2s long) -> midpoint ~2s, not the second (~5s).
        assert!(
            (mid - 2.0).abs() < 0.3,
            "expected earliest midpoint ~2s, got {mid}"
        );
    }

    #[test]
    fn test_find_forward_drop_silence_straddling_start_returns_none() {
        // Already silent at the detected start (onset <= song_start): no drop.
        let profile = build_profile_vals(&[(3.0, 0.0001), (2.0, 0.5)]);
        let spans = find_silence_spans(&profile, ENERGY_THRESHOLD);
        assert!(find_forward_drop(&profile, &spans, ENERGY_THRESHOLD, 0.5, 2.0).is_none());
    }

    #[test]
    fn test_find_forward_drop_empty_spans_returns_none() {
        let profile = build_profile_vals(&[(3.0, 0.5)]);
        assert!(find_forward_drop(&profile, &[], ENERGY_THRESHOLD, 0.5, 2.0).is_none());
    }

    #[test]
    fn test_find_forward_drop_clamps_lead_in_to_profile_end() {
        // A span whose onset lands past the end of the profile must not panic;
        // the lead-in slice is clamped to the profile length.
        let profile = build_profile_vals(&[(1.0, 0.5)]); // ~43 frames
        let span = SilenceSpan {
            midpoint_seconds: 2.0, // onset == 2.0s, well past the 1s profile
            duration_seconds: 0.0,
        };
        let mid = find_forward_drop(&profile, &[span], ENERGY_THRESHOLD, 0.0, 5.0)
            .expect("clamped lead-in still has loud sound");
        assert!((mid - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_find_forward_drop_sound_floor_is_inclusive() {
        let threshold = 0.01;
        let floor = FORWARD_DROP_SOUND_FACTOR * threshold; // 0.02
        let spans_at = {
            let profile = build_profile_vals(&[(0.5, floor), (2.5, 0.0001)]);
            let spans = find_silence_spans(&profile, threshold);
            find_forward_drop(&profile, &spans, threshold, 0.0, 2.0)
        };
        assert!(
            spans_at.is_some(),
            "lead-in exactly at the floor should qualify"
        );

        let just_below = {
            let profile = build_profile_vals(&[(0.5, floor - 0.001), (2.5, 0.0001)]);
            let spans = find_silence_spans(&profile, threshold);
            find_forward_drop(&profile, &spans, threshold, 0.0, 2.0)
        };
        assert!(
            just_below.is_none(),
            "lead-in just below the floor should not qualify"
        );
    }
}
