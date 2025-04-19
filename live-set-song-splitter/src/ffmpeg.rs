use std::ffi::OsStr;
use std::process::Command;

use anyhow::Result;

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
        return Err(anyhow::anyhow!(
            "Failed to extract segment to {}",
            output_file
        ));
    }

    Ok(())
}
