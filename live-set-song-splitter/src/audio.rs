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
        assert_eq!(spans.len(), 2, "expected two silence spans, got {:?}", spans);

        // First silence: starts at 5s, runs 4s -> midpoint ~7s, duration ~4s.
        assert!((spans[0].midpoint_seconds - 7.0).abs() < 0.2, "{:?}", spans[0]);
        assert!((spans[0].duration_seconds - 4.0).abs() < 0.2, "{:?}", spans[0]);

        // Second silence: starts at 14s, runs 6s -> midpoint ~17s, duration ~6s.
        assert!((spans[1].midpoint_seconds - 17.0).abs() < 0.2, "{:?}", spans[1]);
        assert!((spans[1].duration_seconds - 6.0).abs() < 0.2, "{:?}", spans[1]);
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
        let profile = build_profile(&[
            ((5.0 * fps) as usize, false),
            ((3.0 * fps) as usize, true),
        ]);
        let spans = find_silence_spans(&profile, ENERGY_THRESHOLD);
        assert_eq!(spans.len(), 1);
        assert!((spans[0].duration_seconds - 3.0).abs() < 0.2, "{:?}", spans[0]);
    }
}
