use crate::ffmpeg::create_ffmpeg_command;
use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufReader, Read};

pub const SAMPLE_RATE: u32 = 44100;
pub const WINDOW_SIZE: usize = 4096;
pub const HOP_SIZE: usize = 1024;
pub const MIN_SILENCE_DURATION: f64 = 2.0; // Seconds of silence to detect a gap
pub const ENERGY_THRESHOLD: f64 = 0.005; // Threshold for audio energy detection (lowered for better sensitivity)

// const MAX_GAP_DURATION: f64 = 15.0; // Seconds - gaps longer than this are considered "talking" segments

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

pub fn find_silence_points(energy_profile: &[f64], threshold: f64) -> Vec<usize> {
    let mut silence_points = Vec::new();
    let frames_per_second = SAMPLE_RATE as f64 / HOP_SIZE as f64;
    let min_silence_frames = (MIN_SILENCE_DURATION * frames_per_second) as usize;

    let mut silence_start = None;
    let mut silence_length = 0;

    // Find silence spans
    for (i, &energy) in energy_profile.iter().enumerate() {
        if energy < threshold {
            // Low energy detected (silence)
            if silence_start.is_none() {
                silence_start = Some(i);
            }
            silence_length += 1;
        } else {
            // Energy above threshold (sound)
            if let Some(start) = silence_start {
                if silence_length >= min_silence_frames {
                    // We found a silence span that's long enough
                    let midpoint = start + silence_length / 2;
                    silence_points.push(midpoint);
                    println!(
                        "Silence detected at {:.2}s (length: {:.2}s)",
                        midpoint as f64 / frames_per_second,
                        silence_length as f64 / frames_per_second
                    );
                }
                silence_start = None;
                silence_length = 0;
            }
        }
    }

    // Check if we ended with silence
    if let Some(start) = silence_start {
        if silence_length >= min_silence_frames {
            let midpoint = start + silence_length / 2;
            silence_points.push(midpoint);
            println!(
                "Final silence detected at {:.2}s (length: {:.2}s)",
                midpoint as f64 / frames_per_second,
                silence_length as f64 / frames_per_second
            );
        }
    }

    silence_points
}
