use crate::ffmpeg::create_ffprobe_command;
use anyhow::{Context, Result};

#[derive(Debug, Clone, Copy)]
pub struct FrameInfo {
    pub timestamp: f64,
    #[allow(dead_code)]
    pub is_keyframe: bool,
}

#[derive(Debug, Clone)]
pub struct VideoInfo {
    // Basic information
    pub duration: f64,
    pub framerate: u32, // Integer frames per second

    // Frame information
    pub frames: Vec<FrameInfo>,
    #[allow(dead_code)]
    keyframe_indices: Vec<usize>,
}

pub fn frame_blackness(frame_data: &[u8], threshold: u8) -> f64 {
    // Check if frame is black by analyzing pixel data
    // Assumes frame_data is an RGB or grayscale image
    let pixel_count = frame_data.len();
    if pixel_count == 0 {
        return 0.0;
    }

    // Count dark pixels
    let dark_pixels = frame_data
        .iter()
        .filter(|&&pixel| pixel <= threshold)
        .count();
    return dark_pixels as f64 / pixel_count as f64;
}

impl VideoInfo {
    // ((keyframe, frame), frame, keyframe)
    pub fn nearest_frames_by_time(
        &self,
        time: f64,
    ) -> ((usize, usize), Option<usize>, Option<usize>) {
        let mut first_keyframe_num = 0;
        for keyframe_index in &self.keyframe_indices {
            let keyframe = self.frames[*keyframe_index];
            if keyframe.timestamp >= time {
                let mut first_frame_num = first_keyframe_num;
                // TODO: Use binary search or a different data structure to speed this up
                for (i, frame) in self.frames[first_keyframe_num..=*keyframe_index]
                    .iter()
                    .enumerate()
                {
                    if frame.timestamp >= time {
                        return (
                            (first_keyframe_num, first_frame_num),
                            Some(i + first_keyframe_num),
                            Some(*keyframe_index),
                        );
                    } else {
                        first_frame_num = i + first_keyframe_num;
                    }
                }
                return (
                    (first_keyframe_num, first_frame_num),
                    None,
                    Some(*keyframe_index),
                );
            } else {
                first_keyframe_num = *keyframe_index;
            }
        }
        ((first_keyframe_num, 0), None, None)
    }

    #[allow(dead_code)]
    fn get_keyframe_absolute_framenum(&self, frame_num: usize) -> Result<usize> {
        // If we have keyframe indices, map the frame number to the correct keyframe
        if self.keyframe_indices.is_empty() || self.frames.is_empty() {
            return Err(anyhow::anyhow!(
                "Keyframe indices and frames must be populated"
            ));
        }
        // Frame numbers are 1-indexed in our extraction, but array is 0-indexed
        let index = if frame_num > 0 { frame_num - 1 } else { 0 };

        // Check if this index happens to be in our keyframes list
        if index > self.keyframe_indices.len() {
            return Err(anyhow::anyhow!(
                "Frame index {} is out of bounds for keyframe indices",
                index
            ));
        }
        Ok(self.keyframe_indices[index])
    }

    #[allow(dead_code)]
    fn get_keyframe(&self, frame_num: usize) -> Result<FrameInfo> {
        let index = self.get_keyframe_absolute_framenum(frame_num)?;
        // If it's a direct frame match, return that timestamp
        let frame = self.frames[index];
        let is_keyframe = self.frames[index].is_keyframe;
        /*
        println!(
            "Using direct frame timestamp: {}s for frame {} (keyframe: {})",
            frame.timestamp, frame_num, is_keyframe
        );
         */
        if !is_keyframe {
            return Err(anyhow::anyhow!("Frame {} is not a keyframe", frame_num));
        }
        Ok(frame)
    }

    pub fn from_ffprobe_file(input_file: &str) -> Result<VideoInfo> {
        println!("Analyzing video file metadata...");

        // Get basic video information in one call
        let basic_info_output = create_ffprobe_command()
            .args(&[
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=r_frame_rate:format=duration,start_time",
                "-of",
                "json",
                input_file,
            ])
            .output()?;

        if !basic_info_output.status.success() {
            return Err(anyhow::anyhow!("Failed to get video information"));
        }

        let info_json = String::from_utf8(basic_info_output.stdout)?;
        let info: serde_json::Value = serde_json::from_str(&info_json)?;

        // Extract duration
        let duration = info["format"]["duration"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing duration in video metadata"))?
            .parse::<f64>()
            .with_context(|| "Failed to parse duration as a number")?;

        // Extract start time
        let start_time = info["format"]["start_time"]
            .as_str()
            .unwrap_or("0")
            .parse::<f64>()
            .unwrap_or(0.0);
        if start_time != 0.0 {
            return Err(anyhow::anyhow!(
                "Start time is not 0 (found {}), this may cause issues with audio splitting",
                start_time
            ));
        }

        // Extract framerate
        let fps_str = info["streams"][0]["r_frame_rate"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing framerate in video metadata"))?;
        let mut fps: u32 = 24; // Default fallback value
        if let Some((num, den)) = fps_str.split_once('/') {
            if let (Ok(n), Ok(d)) = (num.parse::<f64>(), den.parse::<f64>()) {
                if d > 0.0 {
                    // Calculate framerate and round to nearest integer
                    fps = (n / d).round() as u32;
                }
            }
        }

        println!(
            "Video duration: {}s, start time: {}s, framerate: {} fps",
            duration, start_time, fps
        );

        // Get all frame information in a single pass
        println!("Extracting all frame information...");
        let frame_data = create_ffprobe_command()
            .args(&[
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "packet=pts_time,flags",
                "-of",
                "csv=print_section=0",
                input_file,
            ])
            .output()?;

        let frame_data_str = String::from_utf8(frame_data.stdout)?;

        // Parse frame data - format is "pts_time,flags"
        let mut frames = Vec::new();
        let mut keyframe_indices = Vec::new();

        for (i, line) in frame_data_str.lines().enumerate() {
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() >= 2 {
                if let Ok(timestamp) = parts[0].parse::<f64>() {
                    let is_keyframe = parts[1].contains('K');

                    // Add to frames collection
                    frames.push(FrameInfo {
                        timestamp,
                        is_keyframe,
                    });

                    // If it's a keyframe, record its index
                    if is_keyframe {
                        keyframe_indices.push(i);
                    }
                }
            }
        }

        println!(
            "Found {} frames, including {} keyframes",
            frames.len(),
            keyframe_indices.len()
        );

        Ok(VideoInfo {
            duration,
            framerate: fps,
            frames,
            keyframe_indices,
        })
    }
}
