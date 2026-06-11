use crate::ffmpeg::create_ffmpeg_command;
use anyhow::{Context, Result};
use std::io::Read;
use std::process::Stdio;

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

/// Convert raw little-endian 16-bit mono PCM bytes to f32 samples normalized
/// to [-1.0, 1.0]. A trailing odd byte (truncated sample) is ignored.
fn pcm_s16le_to_samples(pcm_data: &[u8]) -> Vec<f32> {
    pcm_data
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0)
        .collect()
}

pub fn extract_audio_waveform(input_file: &str) -> Result<Vec<f32>> {
    // Stream raw PCM from ffmpeg's stdout instead of going through a temp
    // file: concurrent splitter processes share a working directory, so any
    // fixed temp path is a race (one process deletes or overwrites the file
    // while another is reading it).
    let mut cmd = create_ffmpeg_command().cmd();
    cmd.args(&[
        "-i",
        input_file,
        "-vn", // No video
        "-acodec",
        "pcm_s16le", // PCM signed 16-bit little-endian
        "-ar",
        &SAMPLE_RATE.to_string(), // Sample rate
        "-ac",
        "1", // Mono channel
        "-f",
        "s16le", // Raw samples, no container header
        "-",     // Write to stdout
    ]);
    cmd.stdout(Stdio::piped()); // create_ffmpeg_command defaults stdout to null

    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to spawn ffmpeg to decode audio from {}", input_file))?;
    let mut pcm_data = Vec::new();
    // Read stdout to EOF before wait(): stderr is inherited, so this single
    // pipe cannot deadlock. Check the exit status before trusting the bytes —
    // a failed ffmpeg may have produced a truncated stream.
    let read_result = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("ffmpeg child process has no stdout pipe"))?
        .read_to_end(&mut pcm_data);
    let status = child
        .wait()
        .with_context(|| format!("Failed waiting for ffmpeg decoding {}", input_file))?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "ffmpeg failed to decode audio from {} (exit {:?})",
            input_file,
            status.code()
        ));
    }
    read_result
        .with_context(|| format!("Failed to read PCM stream from ffmpeg for {}", input_file))?;

    if pcm_data.is_empty() {
        log::info!("ffmpeg produced no audio data for {}", input_file);
    }
    let samples = pcm_s16le_to_samples(&pcm_data);
    log::debug!(
        "Extracted {} PCM bytes ({} samples) from {}",
        pcm_data.len(),
        samples.len(),
        input_file
    );
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
    use std::path::{Path, PathBuf};

    #[test]
    fn test_pcm_s16le_to_samples_known_values() {
        let bytes = [
            0x00, 0x00, // 0
            0xFF, 0x7F, // i16::MAX = 32767
            0x00, 0x80, // i16::MIN = -32768
            0x00, 0x40, // 16384
        ];
        let samples = pcm_s16le_to_samples(&bytes);
        assert_eq!(samples.len(), 4);
        assert_eq!(samples[0], 0.0);
        assert!((samples[1] - 32767.0 / 32768.0).abs() < 1e-6);
        assert_eq!(samples[2], -1.0);
        assert_eq!(samples[3], 0.5);
    }

    #[test]
    fn test_pcm_s16le_to_samples_empty_and_odd_input() {
        assert!(pcm_s16le_to_samples(&[]).is_empty());
        // A lone trailing byte is a truncated sample and is dropped.
        assert!(pcm_s16le_to_samples(&[0x12]).is_empty());
        assert_eq!(pcm_s16le_to_samples(&[0x00, 0x40, 0x7F]), vec![0.5]);
    }

    /// Generate a 1-second sine-wave audio file for extraction tests.
    fn generate_sine_file(frequency: u32) -> PathBuf {
        let path = std::env::temp_dir().join(format!("lsss_test_sine_{}.wav", frequency));
        let status = std::process::Command::new("ffmpeg")
            .args(&[
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("sine=frequency={}:duration=1", frequency),
                "-y",
            ])
            .arg(&path)
            .status()
            .expect("ffmpeg must be installed to run these tests");
        assert!(status.success(), "failed to generate sine test file");
        path
    }

    #[test]
    fn test_extract_audio_waveform_from_sine() {
        let input = generate_sine_file(440);
        let samples = extract_audio_waveform(input.to_str().unwrap()).unwrap();
        // 1 second of mono audio at SAMPLE_RATE, allow resampling slack.
        let expected = SAMPLE_RATE as i64;
        assert!(
            (samples.len() as i64 - expected).abs() < 1000,
            "expected ~{} samples, got {}",
            expected,
            samples.len()
        );
        // ffmpeg's sine source generates at 1/8 full scale, so the expected
        // mean square is (0.125^2)/2 ~= 0.0078.
        let mean_square: f32 = samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32;
        assert!(
            (mean_square - 0.0078).abs() < 0.002,
            "unexpected sine energy: {}",
            mean_square
        );
        std::fs::remove_file(&input).ok();
    }

    /// Regression test for the dual-split race: two concurrent extractions in
    /// the same working directory must each get their own waveform and must
    /// not create a shared temp file.
    #[test]
    fn test_extract_audio_waveform_concurrent() {
        let input_a = generate_sine_file(441);
        let input_b = generate_sine_file(882);
        let spawn = |path: PathBuf| {
            std::thread::spawn(move || extract_audio_waveform(path.to_str().unwrap()))
        };
        let thread_a = spawn(input_a.clone());
        let thread_b = spawn(input_b.clone());
        let waveform_a = thread_a.join().unwrap().expect("441Hz extraction failed");
        let waveform_b = thread_b.join().unwrap().expect("882Hz extraction failed");
        assert!(!waveform_a.is_empty() && !waveform_b.is_empty());
        // Different inputs must yield different waveforms — catches one
        // extraction silently reading the other's audio.
        assert_ne!(waveform_a, waveform_b);
        assert!(
            !Path::new("temp_audio.wav").exists(),
            "extraction must not create a shared temp file in the working directory"
        );
        std::fs::remove_file(&input_a).ok();
        std::fs::remove_file(&input_b).ok();
    }

    #[test]
    fn test_extract_audio_waveform_missing_input_errors() {
        let missing = "/nonexistent/lsss_missing_input.mp4";
        let err = extract_audio_waveform(missing).unwrap_err();
        let message = format!("{:#}", err);
        assert!(
            message.contains(missing),
            "error should name the input: {}",
            message
        );
        assert!(
            message.contains("exit"),
            "error should include the exit code: {}",
            message
        );
    }

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
}
