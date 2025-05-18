use std::ffi::OsStr;
use std::process::Command;

use crate::concert;

use anyhow::{anyhow, Result};

// these ones worked okay
// pub const BLACK_AND_WHITE: &str = "hue=s=0";
pub const BLACK_AND_WHITE: &str = "format=gray,maskfun=low=128:high=128:fill=0:sum=128";

#[derive(Debug)]
pub struct Ffmpeg {
    cmd: Command,
}

impl Ffmpeg {
    pub fn cmd(self) -> Command {
        self.cmd
    }

    pub fn arg(&mut self, arg: &str) -> &mut Ffmpeg {
        self.cmd.arg(arg);
        self
    }

    pub fn args<Iter, Str>(&mut self, args: Iter) -> &mut Ffmpeg
    where
        Iter: IntoIterator<Item = Str>,
        Str: AsRef<OsStr>,
    {
        self.cmd.args(args);
        return self;
    }

    pub fn video_filter(&mut self, file: &str, filters: Vec<&str>) -> &mut Ffmpeg {
        self.cmd.arg("-vf");
        self.cmd.arg(filters.join(","));
        self.cmd.arg(file);
        self
    }

    pub fn from_to(&mut self, start_time: f64, end_time: f64) -> &mut Ffmpeg {
        self.args(vec![
            "-ss",
            &format!("{:.3}", start_time),
            "-to",
            &format!("{:.3}", end_time),
        ])
    }

    pub fn png(&mut self) -> &mut Ffmpeg {
        self.arg("-c:v");
        self.arg("png")
    }
}

pub fn create_ffmpeg_command() -> Ffmpeg {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(&["-hide_banner", "-loglevel", "warning"]);
    cmd.stdout(std::process::Stdio::null());
    Ffmpeg { cmd: cmd }
}

pub fn create_ffprobe_command() -> Command {
    let mut cmd = Command::new("ffprobe");
    cmd.args(&["-hide_banner", "-loglevel", "warning"]);
    cmd
}

fn _extract_segment_mp4box(
    input_file: &str,
    output_file: &str,
    start_time: f64,
    end_time: f64,
) -> Result<()> {
    // let duration = end_time - start_time;

    // Use MP4Box for segment extraction
    let status = Command::new("MP4Box")
        .args(&[
            "-splitx",
            &format!("{:.3}:{:.3}", start_time, end_time),
            "-out",
            output_file,
            input_file,
        ])
        .status()?;

    if !status.success() {
        return Err(anyhow!(
            "Failed to extract segment to {}",
            output_file
        ));
    }

    Ok(())
}

// Extract audio-only segment using stream copy (no re-encoding)
pub fn extract_audio_segment(
    input_file: &str,
    output_file: &str,
    start_time: f64,
    end_time: f64,
    song_title: Option<&str>,
    concertdata: &concert::SetMetaData,
    track_number: Option<usize>,
) -> Result<()> {
    let mut ffmpeg = create_ffmpeg_command();
    ffmpeg
        .args(&[
            "-i", input_file, "-vn", // No video
            "-acodec", "copy", // Copy audio stream without re-encoding
            "-map", "0:a",
        ])
        .from_to(start_time, end_time);
    let mut cmd = ffmpeg.cmd();

    // Add metadata
    add_metadata_to_cmd(&mut cmd, song_title, concertdata, track_number);

    cmd.args(&[
        "-y", // Overwrite output file
        output_file,
    ]);

    let status = cmd.status()?;

    if !status.success() {
        return Err(anyhow!(
            "Failed to extract audio segment to {}",
            output_file
        ));
    }

    Ok(())
}

// Add common metadata fields to an FFmpeg command
pub fn add_metadata_to_cmd(
    cmd: &mut std::process::Command,
    song_title: Option<&str>,
    concertdata: &concert::SetMetaData,
    track_number: Option<usize>,
) {
    // Add artist metadata
    cmd.args(&["-metadata", &format!("artist={}", concertdata.artist)]);

    // Add title metadata if available
    if let Some(title) = song_title {
        cmd.args(&["-metadata", &format!("title={}", title)]);
    }

    // Add album metadata if available
    if let Some(ref album) = concertdata.album {
        cmd.args(&["-metadata", &format!("album={}", album)]);
    }

    // Add year metadata if available
    if let Some(year_value) = concertdata.year() {
        if !year_value.is_empty() {
            cmd.args(&["-metadata", &format!("date={}", year_value)]);
        }
    }

    // Add track number metadata if available
    if let Some(track) = track_number {
        cmd.args(&["-metadata", &format!("track={}", track)]);
    }
}