//! Cutting one track out of the concert video.
//!
//! Three strategies are available (see [`VideoCutMode`]). All keep audio and
//! video in sync; they trade cut precision against speed and quality:
//!
//! | mode     | speed (whole concert) | start of track                          |
//! |----------|-----------------------|-----------------------------------------|
//! | copy     | ~1.5s                 | up to one GOP early (previous song tail)|
//! | smart    | ~7s                   | frame-accurate                          |
//! | reencode | ~3min                 | frame-accurate                          |
//!
//! Smart mode re-encodes only the head of the track (from the cut point to the
//! next keyframe, at most one GOP), stream-copies the rest, and concatenates:
//!
//! ```text
//! probe next keyframe kf >= start
//!       |
//!       |- source not h264 ............................. ReencodeWhole
//!       |- no keyframe in (start, end) .................. ReencodeWhole
//!       |- kf within half a frame of start .............. CopyWhole (cut is exact)
//!       |- otherwise:                                     Spliced
//!            head  = re-encode  [start, kf)   video only
//!            tail  = stream-copy [kf, end]    video only
//!            audio = stream-copy [start, end] exact
//!            concat head+tail video, mux with audio
//! ```

use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, ValueEnum};

use crate::ffmpeg;
use concert_types::ConcertInfo;

/// How to cut the video stream for each track. All modes keep audio and video in
/// sync; they trade cut precision against speed/quality.
#[derive(Parser, Debug, Clone, Copy, ValueEnum, PartialEq)]
#[clap(rename_all = "lowercase")]
#[derive(Default)]
pub enum VideoCutMode {
    /// Stream-copy (fastest, lossless). Snaps each cut back to the nearest preceding
    /// keyframe, so a track may start up to one GOP (a few seconds) early.
    Copy,
    /// Stream-copy everything except the head of the track (cut point to next
    /// keyframe), which is re-encoded. Frame-accurate start at near-copy speed.
    #[default]
    Smart,
    /// Re-encode the whole video so each cut is frame-accurate at the detected start
    /// (slowest, slight quality change).
    Reencode,
}

/// x264 encoding parameters used by [`VideoCutMode::Reencode`] and for the head
/// segment of [`VideoCutMode::Smart`].
pub const REENCODE_PRESET: &str = "veryfast";
pub const REENCODE_CRF: &str = "18";

/// How far past the cut point to look for the next keyframe. Must exceed the
/// source's GOP length (NPR sources use 4s keyframe intervals).
const KEYFRAME_PROBE_WINDOW_SECS: f64 = 30.0;

/// How far to rewind the fast input seek when re-encoding the head segment.
/// ffmpeg's input seek is DTS-based and can overshoot onto the next keyframe when
/// the requested time is within a few frames of it (B-frame DTS runs ahead of PTS),
/// silently dropping the frames before the keyframe. Seeking this much early and
/// discarding precisely with an accurate output-side seek avoids that.
const HEAD_SEEK_REWIND_SECS: f64 = 1.0;

/// Tolerance when matching a probed keyframe timestamp against the cut point
/// (guards float noise in ffprobe output).
const KEYFRAME_MATCH_EPS: f64 = 0.001;

/// The head encode's `-t` is shortened by this fraction of a frame so the keyframe
/// at its exclusive end can never be included: ffmpeg's `-t` boundary is not exact,
/// and a head window holding no real frames (a cut right before the keyframe)
/// otherwise emits the keyframe itself, duplicating the tail's first frame.
/// Legitimate head frames sit at least a full frame before the keyframe, so a
/// quarter-frame trim cannot drop one.
const HEAD_END_GUARD_FRAME_FRACTION: f64 = 0.25;

/// File names of the splice parts inside a track's private work directory. The
/// concat list must reference the video parts by these bare names because the
/// concat demuxer resolves relative entries against the *list file's* directory
/// (see [`concat_list_entry`]); the same constants build the full paths the parts
/// are written to, so the two spellings cannot drift apart. Parallel extraction
/// stays safe: every track gets its own `<output>.mp4.work/` directory.
const HEAD_FILE_NAME: &str = "head.mp4";
const TAIL_FILE_NAME: &str = "tail.mp4";
const AUDIO_FILE_NAME: &str = "audio.m4a";
const CONCAT_LIST_FILE_NAME: &str = "concat.txt";

/// Video stream properties of the source file, probed once per input. The smart-cut
/// head segment is encoded with matching properties so the concat demuxer can splice
/// it onto the stream-copied tail.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceVideoParams {
    pub codec_name: String,
    /// h264 profile as reported by ffprobe, e.g. "High".
    pub profile: Option<String>,
    /// h264 level as reported by ffprobe, e.g. 40 (= level 4.0).
    pub level: Option<i64>,
    pub pix_fmt: Option<String>,
    /// Frames per second; used to decide whether a cut already lands on a keyframe.
    pub fps: f64,
    /// Denominator of the stream time base, e.g. 90000 for 1/90000. The head and
    /// tail are written with this timescale so concat keeps exact timestamps.
    pub time_base_den: Option<u32>,
}

impl SourceVideoParams {
    /// Half a frame: a cut within this distance of a keyframe is "on" the keyframe.
    fn half_frame_secs(&self) -> f64 {
        0.5 / self.fps.max(1.0)
    }
}

/// The strategy chosen for one track under [`VideoCutMode::Smart`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SmartCutPlan {
    /// The cut lands on a keyframe, so a plain stream copy is already exact.
    CopyWhole,
    /// No usable keyframe after the cut (or a non-h264 source): re-encode the
    /// whole segment.
    ReencodeWhole,
    /// Re-encode `[start, keyframe)`, stream-copy `[keyframe, end]`, concat.
    Spliced { keyframe: f64 },
}

/// Decide how to smart-cut `[start, end]` given the next keyframe at/after `start`.
pub fn plan_smart_cut(
    start: f64,
    end: f64,
    next_keyframe: Option<f64>,
    params: &SourceVideoParams,
) -> SmartCutPlan {
    if params.codec_name != "h264" {
        // The head encoder writes h264; splicing it onto another codec's stream
        // can't work, so fall back to a full re-encode.
        return SmartCutPlan::ReencodeWhole;
    }
    match next_keyframe {
        None => SmartCutPlan::ReencodeWhole,
        Some(kf) if kf >= end => SmartCutPlan::ReencodeWhole,
        Some(kf) if kf - start <= params.half_frame_secs() => SmartCutPlan::CopyWhole,
        Some(kf) => SmartCutPlan::Spliced { keyframe: kf },
    }
}

/// Build the ffmpeg seek/codec arguments for cutting `input_file` to `[start, end]`
/// with a single ffmpeg command ([`VideoCutMode::Copy`] / [`VideoCutMode::Reencode`]).
/// These slot in after the ffmpeg base flags and before the per-track metadata and
/// output path.
///
/// Both modes use input-side seeking (`-ss` before `-i`) so audio and video start
/// together. The previous command placed `-ss` *after* `-i` with `-c copy`, which
/// left the video starting at the first keyframe *after* the cut while the audio
/// started exactly at the cut — desyncing every track not cut on a keyframe by up to
/// one GOP (e.g. ~1.7s on a 4s-keyframe source). See
/// https://superuser.com/questions/1850814/how-to-cut-a-video-with-ffmpeg-with-no-or-minimal-re-encoding
pub fn build_cut_args(mode: VideoCutMode, input_file: &str, start: f64, end: f64) -> Vec<String> {
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
        // Smart mode runs several ffmpeg commands; see extract_segment_smart.
        VideoCutMode::Smart => {
            unreachable!("smart mode is multi-command; use extract_segment_smart")
        }
    }
}

/// Map an ffprobe h264 profile name to the value x264's `-profile:v` accepts,
/// or `None` for profiles x264 can't produce (the flag is then omitted and x264
/// picks a compatible profile itself).
fn x264_profile_for(probed: &str) -> Option<String> {
    let lower = probed.to_lowercase();
    match lower.as_str() {
        "baseline" | "constrained baseline" => Some("baseline".into()),
        "main" => Some("main".into()),
        "high" => Some("high".into()),
        "high 10" => Some("high10".into()),
        "high 4:2:2" => Some("high422".into()),
        "high 4:4:4 predictive" => Some("high444".into()),
        _ => None,
    }
}

/// Arguments to re-encode the head `[start, keyframe)`, video only.
///
/// Two-stage seek: a fast input `-ss` to [`HEAD_SEEK_REWIND_SECS`] before the cut,
/// then an accurate output-side `-ss` that decodes and discards up to the true
/// start (see [`HEAD_SEEK_REWIND_SECS`] for why a direct input seek is wrong here).
/// The encode mirrors the source's stream properties so concat can splice it onto
/// the stream-copied tail.
fn build_smart_head_args(
    input_file: &str,
    start: f64,
    keyframe: f64,
    params: &SourceVideoParams,
) -> Vec<String> {
    let pre_seek = (start - HEAD_SEEK_REWIND_SECS).max(0.0);
    let mut args: Vec<String> = vec![
        "-ss".into(),
        format!("{:.6}", pre_seek),
        "-i".into(),
        input_file.into(),
        "-ss".into(),
        format!("{:.6}", start - pre_seek),
        "-t".into(),
        format!(
            "{:.6}",
            keyframe - start - HEAD_END_GUARD_FRAME_FRACTION / params.fps.max(1.0)
        ),
        "-an".into(),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        REENCODE_PRESET.into(),
        "-crf".into(),
        REENCODE_CRF.into(),
    ];
    if let Some(profile) = params.profile.as_deref().and_then(x264_profile_for) {
        args.push("-profile:v".into());
        args.push(profile);
    }
    if let Some(level) = params.level.filter(|l| *l > 0) {
        // ffprobe reports the integer level_idc (e.g. 40 for level 4.0); x264's
        // -level accepts that form directly (verified end-to-end on a level-40
        // source), so it is passed through unconverted.
        args.push("-level".into());
        args.push(level.to_string());
    }
    if let Some(pix_fmt) = &params.pix_fmt {
        args.push("-pix_fmt".into());
        args.push(pix_fmt.clone());
    }
    if let Some(den) = params.time_base_den {
        args.push("-video_track_timescale".into());
        args.push(den.to_string());
    }
    args
}

/// Arguments to stream-copy the tail `[keyframe, end]`, video only. The input seek
/// lands exactly on the keyframe; `-copyts` keeps the original timeline so `-to`
/// ends at the true `end`, then `make_zero` rebases the segment to start at ~0.
fn build_smart_tail_args(
    input_file: &str,
    keyframe: f64,
    end: f64,
    params: &SourceVideoParams,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-ss".into(),
        format!("{:.6}", keyframe),
        "-i".into(),
        input_file.into(),
        "-to".into(),
        format!("{:.6}", end),
        "-an".into(),
        "-c:v".into(),
        "copy".into(),
        "-copyts".into(),
        "-avoid_negative_ts".into(),
        "make_zero".into(),
    ];
    if let Some(den) = params.time_base_den {
        args.push("-video_track_timescale".into());
        args.push(den.to_string());
    }
    args
}

/// Arguments to stream-copy the audio `[start, end]` exactly.
///
/// The input seek alone is not enough: with `-c copy` it snaps the *demuxer* — audio
/// packets included — back to the preceding **video** keyframe, which would prepend
/// a few seconds of the previous song's audio and desync the spliced output. With
/// `-copyts` the original timeline survives the input seek, so the accurate
/// output-side `-ss` at the same absolute time discards exactly the early packets.
fn build_smart_audio_args(input_file: &str, start: f64, end: f64) -> Vec<String> {
    vec![
        "-ss".into(),
        format!("{:.6}", start),
        "-i".into(),
        input_file.into(),
        "-copyts".into(),
        "-ss".into(),
        format!("{:.6}", start),
        "-to".into(),
        format!("{:.6}", end),
        "-vn".into(),
        "-c:a".into(),
        "copy".into(),
        "-avoid_negative_ts".into(),
        "make_zero".into(),
    ]
}

/// Arguments to concatenate the head/tail video parts and mux them with the audio.
/// Everything is stream-copied; per-track metadata and the output path are appended
/// by the caller.
fn build_smart_concat_args(list_file: &str, audio_file: &str) -> Vec<String> {
    vec![
        "-f".into(),
        "concat".into(),
        "-safe".into(),
        "0".into(),
        "-i".into(),
        list_file.into(),
        "-i".into(),
        audio_file.into(),
        "-map".into(),
        "0:v".into(),
        "-map".into(),
        "1:a".into(),
        "-c".into(),
        "copy".into(),
    ]
}

/// One `file` line for an ffmpeg concat list. The concat demuxer resolves relative
/// entries against the *list file's* directory, not the process working directory,
/// so entries must be bare filenames of the parts sitting next to the list (an
/// output-dir-relative path would be resolved against the work dir and double up).
/// Paths go inside single quotes, where the only character needing escape is the
/// single quote itself (close, escape, reopen: `'\''`).
fn concat_list_entry(path: &str) -> String {
    format!("file '{}'\n", path.replace('\'', r"'\''"))
}

/// The full concat list for the splice; entries are filenames relative to the list
/// file's directory (see [`concat_list_entry`]). The head entry carries an explicit
/// `duration` directive so the tail is offset by exactly the head's share of the
/// track (`keyframe - start`): the head's own container duration can come out
/// short (its last frame may carry a truncated duration), which would slide the
/// tail back and stack two frames on one timestamp at the splice.
fn build_concat_list(head: Option<(&str, f64)>, tail_path: &str) -> String {
    let mut list = String::new();
    if let Some((head_path, head_duration)) = head {
        list.push_str(&concat_list_entry(head_path));
        list.push_str(&format!("duration {:.6}\n", head_duration));
    }
    list.push_str(&concat_list_entry(tail_path));
    list
}

/// Find the first keyframe timestamp at or after `start` in ffprobe CSV frame
/// output. ffprobe appends a trailing comma to frame lines that carry side data,
/// so each line is split rather than parsed whole.
fn parse_next_keyframe(ffprobe_csv: &str, start: f64) -> Option<f64> {
    ffprobe_csv
        .lines()
        .filter_map(|line| line.split(',').next()?.trim().parse::<f64>().ok())
        .find(|&pts| pts >= start - KEYFRAME_MATCH_EPS)
}

/// Probe the source's video stream properties (see [`SourceVideoParams`]). Run once
/// per input file.
pub fn probe_source_video_params(input_file: &str) -> Result<SourceVideoParams> {
    let output = ffmpeg::create_ffprobe_command()
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,profile,level,pix_fmt,r_frame_rate,time_base",
            "-of",
            "json",
            input_file,
        ])
        .output()
        .context("running ffprobe for source video parameters")?;
    if !output.status.success() {
        return Err(anyhow!(
            "ffprobe failed probing video parameters of {}: {}",
            input_file,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let info: serde_json::Value = serde_json::from_str(&String::from_utf8(output.stdout)?)?;
    let stream = &info["streams"][0];

    let codec_name = stream["codec_name"]
        .as_str()
        .ok_or_else(|| anyhow!("missing codec_name in ffprobe output for {}", input_file))?
        .to_string();
    let fps = stream["r_frame_rate"]
        .as_str()
        .and_then(parse_frame_rate)
        .ok_or_else(|| anyhow!("missing r_frame_rate in ffprobe output for {}", input_file))?;
    let time_base_den = stream["time_base"]
        .as_str()
        .and_then(|tb| tb.split_once('/'))
        .and_then(|(_, den)| den.parse::<u32>().ok());

    let params = SourceVideoParams {
        codec_name,
        profile: stream["profile"].as_str().map(str::to_string),
        level: stream["level"].as_i64(),
        pix_fmt: stream["pix_fmt"].as_str().map(str::to_string),
        fps,
        time_base_den,
    };
    println!("Source video parameters: {:?}", params);
    Ok(params)
}

/// Parse an ffprobe rational frame rate like "24/1" into frames per second.
fn parse_frame_rate(rate: &str) -> Option<f64> {
    let (num, den) = rate.split_once('/')?;
    let num: f64 = num.parse().ok()?;
    let den: f64 = den.parse().ok()?;
    if den > 0.0 && num > 0.0 {
        Some(num / den)
    } else {
        None
    }
}

/// Probe the first keyframe at/after `start`, or `None` if there is none within
/// [`KEYFRAME_PROBE_WINDOW_SECS`].
fn probe_next_keyframe(input_file: &str, start: f64) -> Result<Option<f64>> {
    let output = ffmpeg::create_ffprobe_command()
        .args([
            "-v",
            "error",
            "-read_intervals",
            &format!("{:.6}%+{}", start, KEYFRAME_PROBE_WINDOW_SECS),
            "-skip_frame",
            "nokey",
            "-select_streams",
            "v:0",
            "-show_entries",
            "frame=pts_time",
            "-of",
            "csv=p=0",
            input_file,
        ])
        .output()
        .context("running ffprobe for keyframe lookup")?;
    if !output.status.success() {
        return Err(anyhow!(
            "ffprobe failed finding keyframes after {:.3}s in {}: {}",
            start,
            input_file,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(parse_next_keyframe(
        &String::from_utf8(output.stdout)?,
        start,
    ))
}

/// Number of video frames in `path`, or 0 when the file has no video stream (an
/// ffmpeg run whose seek window contained no frames still writes a valid, empty
/// container).
fn probe_video_frame_count(path: &str) -> Result<u64> {
    let output = ffmpeg::create_ffprobe_command()
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=nb_frames",
            "-of",
            "csv=p=0",
            path,
        ])
        .output()
        .context("running ffprobe for frame count")?;
    if !output.status.success() {
        return Err(anyhow!(
            "ffprobe failed counting frames in {}: {}",
            path,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8(output.stdout)?
        .split(',')
        .next()
        .unwrap_or("")
        .trim()
        .parse::<u64>()
        .unwrap_or(0))
}

/// Run one ffmpeg command (built from `args` + `-y output`) and fail loudly.
fn run_ffmpeg(args: &[String], output_file: &str, what: &str) -> Result<()> {
    let mut ffmpeg_cmd = ffmpeg::create_ffmpeg_command();
    ffmpeg_cmd.args(args);
    let mut cmd = ffmpeg_cmd.cmd();
    cmd.args(["-y", output_file]);
    let status = cmd
        .status()
        .with_context(|| format!("spawning ffmpeg for {}", what))?;
    if !status.success() {
        return Err(anyhow!("ffmpeg failed building {} ({})", what, output_file));
    }
    Ok(())
}

/// Extract `[start_time, end_time]` with a single ffmpeg command
/// ([`VideoCutMode::Copy`] or [`VideoCutMode::Reencode`]).
#[allow(clippy::too_many_arguments)] // All arguments are required ffmpeg segment parameters
pub fn extract_segment(
    input_file: &str,
    output_file: &str,
    start_time: f64,
    end_time: f64,
    cut_mode: VideoCutMode,
    song_title: Option<&str>,
    concertdata: &ConcertInfo,
    track_number: Option<usize>,
) -> Result<()> {
    if cut_mode == VideoCutMode::Smart {
        return Err(anyhow!(
            "smart mode is multi-command; call extract_segment_smart"
        ));
    }
    let mut ffmpeg_cmd = ffmpeg::create_ffmpeg_command();
    ffmpeg_cmd.args(build_cut_args(cut_mode, input_file, start_time, end_time));
    let mut cmd = ffmpeg_cmd.cmd();

    // Add metadata
    ffmpeg::add_metadata_to_cmd(&mut cmd, song_title, concertdata, track_number);

    cmd.args([
        "-y", // Overwrite output file
        output_file,
    ]);

    let status = cmd.status()?;

    if !status.success() {
        return Err(anyhow!("Failed to extract segment to {}", output_file));
    }

    Ok(())
}

/// Extract `[start_time, end_time]` under [`VideoCutMode::Smart`]: plan against the
/// next keyframe, then either delegate to a single-command mode or build the
/// head/tail/audio splice in a `<output>.work` directory (removed afterwards).
#[allow(clippy::too_many_arguments)] // All arguments are required ffmpeg segment parameters
pub fn extract_segment_smart(
    input_file: &str,
    output_file: &str,
    start_time: f64,
    end_time: f64,
    params: &SourceVideoParams,
    song_title: Option<&str>,
    concertdata: &ConcertInfo,
    track_number: Option<usize>,
) -> Result<()> {
    let next_keyframe = probe_next_keyframe(input_file, start_time)?;
    let plan = plan_smart_cut(start_time, end_time, next_keyframe, params);
    println!(
        "Smart cut for {:.3}s..{:.3}s: next keyframe {:?} -> {:?}",
        start_time, end_time, next_keyframe, plan
    );

    let keyframe = match plan {
        SmartCutPlan::CopyWhole => {
            return extract_segment(
                input_file,
                output_file,
                start_time,
                end_time,
                VideoCutMode::Copy,
                song_title,
                concertdata,
                track_number,
            );
        }
        SmartCutPlan::ReencodeWhole => {
            println!(
                "Smart cut falling back to a full re-encode for \"{}\"",
                song_title.unwrap_or("?")
            );
            return extract_segment(
                input_file,
                output_file,
                start_time,
                end_time,
                VideoCutMode::Reencode,
                song_title,
                concertdata,
                track_number,
            );
        }
        SmartCutPlan::Spliced { keyframe } => keyframe,
    };

    let work_dir = format!("{}.work", output_file);
    fs::create_dir_all(&work_dir)
        .with_context(|| format!("creating smart cut work directory {}", work_dir))?;
    let result = splice_segment(
        input_file,
        output_file,
        start_time,
        end_time,
        keyframe,
        params,
        &work_dir,
        song_title,
        concertdata,
        track_number,
    );
    if let Err(e) = fs::remove_dir_all(&work_dir) {
        println!("Warning: failed to remove {}: {}", work_dir, e);
    }
    result
}

/// Build the head/tail/audio parts in `work_dir` and concat-mux them into
/// `output_file`. See the module docs for the splice layout.
#[allow(clippy::too_many_arguments)] // All arguments are required ffmpeg splice parameters
fn splice_segment(
    input_file: &str,
    output_file: &str,
    start_time: f64,
    end_time: f64,
    keyframe: f64,
    params: &SourceVideoParams,
    work_dir: &str,
    song_title: Option<&str>,
    concertdata: &ConcertInfo,
    track_number: Option<usize>,
) -> Result<()> {
    let head_file = format!("{}/{}", work_dir, HEAD_FILE_NAME);
    let tail_file = format!("{}/{}", work_dir, TAIL_FILE_NAME);
    let audio_file = format!("{}/{}", work_dir, AUDIO_FILE_NAME);
    let list_file = format!("{}/{}", work_dir, CONCAT_LIST_FILE_NAME);

    run_ffmpeg(
        &build_smart_head_args(input_file, start_time, keyframe, params),
        &head_file,
        "smart cut head",
    )?;
    run_ffmpeg(
        &build_smart_tail_args(input_file, keyframe, end_time, params),
        &tail_file,
        "smart cut tail",
    )?;
    // The tail carries (nearly) the whole track; an empty one means the cut plan
    // was wrong for this source and must fail loudly, not mux audio over nothing.
    if probe_video_frame_count(&tail_file)? == 0 {
        return Err(anyhow!(
            "smart cut tail has no video frames for {} ({}s..{}s, keyframe {}s)",
            output_file,
            start_time,
            end_time,
            keyframe
        ));
    }
    run_ffmpeg(
        &build_smart_audio_args(input_file, start_time, end_time),
        &audio_file,
        "smart cut audio",
    )?;

    // A track whose first frame is the keyframe itself (e.g. the source's very
    // first frame) yields an empty head: splice the tail alone.
    let head_frames = probe_video_frame_count(&head_file)?;
    let head = if head_frames > 0 {
        Some((HEAD_FILE_NAME, keyframe - start_time))
    } else {
        println!("Smart cut head is empty (cut on first frame); using tail only");
        None
    };
    fs::write(&list_file, build_concat_list(head, TAIL_FILE_NAME))
        .with_context(|| format!("writing {}", list_file))?;

    let mut ffmpeg_cmd = ffmpeg::create_ffmpeg_command();
    ffmpeg_cmd.args(build_smart_concat_args(&list_file, &audio_file));
    let mut cmd = ffmpeg_cmd.cmd();
    ffmpeg::add_metadata_to_cmd(&mut cmd, song_title, concertdata, track_number);
    cmd.args(["-y", output_file]);
    let status = cmd
        .status()
        .context("spawning ffmpeg for smart cut concat")?;
    if !status.success() {
        return Err(anyhow!("Failed to splice segment to {}", output_file));
    }
    if !Path::new(output_file).exists() {
        return Err(anyhow!("Smart cut did not produce {}", output_file));
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
            assert!(
                ss < i,
                "{:?}: -ss must precede -i (got ss={ss}, i={i})",
                mode
            );
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
    fn smart_is_the_default_mode() {
        assert_eq!(VideoCutMode::default(), VideoCutMode::Smart);
    }
}

#[cfg(test)]
mod tests_smart_cut {
    use super::*;

    fn h264_params() -> SourceVideoParams {
        SourceVideoParams {
            codec_name: "h264".into(),
            profile: Some("High".into()),
            level: Some(40),
            pix_fmt: Some("yuv420p".into()),
            fps: 24.0,
            time_base_den: Some(90000),
        }
    }

    fn value_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .map(|s| s.as_str())
    }

    // --- plan_smart_cut ---

    #[test]
    fn plan_splices_mid_gop_cut() {
        // "Say Yes": cut at 434.338, next keyframe 436.046.
        let plan = plan_smart_cut(434.338, 770.921, Some(436.046), &h264_params());
        assert_eq!(plan, SmartCutPlan::Spliced { keyframe: 436.046 });
    }

    #[test]
    fn plan_copies_when_cut_is_on_a_keyframe() {
        // Within half a frame (1/48s at 24fps) counts as on the keyframe.
        let plan = plan_smart_cut(104.713, 258.504, Some(104.720), &h264_params());
        assert_eq!(plan, SmartCutPlan::CopyWhole);
        // ...but a full frame away does not.
        let plan = plan_smart_cut(104.713, 258.504, Some(104.755), &h264_params());
        assert_eq!(plan, SmartCutPlan::Spliced { keyframe: 104.755 });
    }

    #[test]
    fn plan_reencodes_without_a_usable_keyframe() {
        let params = h264_params();
        // No keyframe found in the probe window.
        assert_eq!(
            plan_smart_cut(10.0, 12.0, None, &params),
            SmartCutPlan::ReencodeWhole
        );
        // Next keyframe is past the end of the track.
        assert_eq!(
            plan_smart_cut(10.0, 12.0, Some(13.0), &params),
            SmartCutPlan::ReencodeWhole
        );
    }

    #[test]
    fn plan_reencodes_non_h264_sources() {
        let params = SourceVideoParams {
            codec_name: "vp9".into(),
            ..h264_params()
        };
        assert_eq!(
            plan_smart_cut(10.0, 20.0, Some(12.0), &params),
            SmartCutPlan::ReencodeWhole
        );
    }

    // --- arg builders ---

    #[test]
    fn head_uses_two_stage_seek() {
        let args = build_smart_head_args("in.mp4", 434.338, 436.046, &h264_params());
        // Fast input seek 1s early...
        let input = args.iter().position(|a| a == "-i").unwrap();
        assert_eq!(args[input - 1], "433.338000");
        assert_eq!(args[input - 2], "-ss");
        // ...then an accurate output-side seek for the remainder.
        let out_ss = args[input..].iter().position(|a| a == "-ss").unwrap() + input;
        assert_eq!(args[out_ss + 1], "1.000000");
        // Duration covers [start, keyframe) less the quarter-frame guard that keeps
        // the keyframe itself out: 1.708 - 0.25/24.
        assert_eq!(value_after(&args, "-t"), Some("1.697583"));
        // Video only, encoded to match the source stream.
        assert!(args.iter().any(|a| a == "-an"));
        assert_eq!(value_after(&args, "-c:v"), Some("libx264"));
        assert_eq!(value_after(&args, "-profile:v"), Some("high"));
        assert_eq!(value_after(&args, "-level"), Some("40"));
        assert_eq!(value_after(&args, "-pix_fmt"), Some("yuv420p"));
        assert_eq!(value_after(&args, "-video_track_timescale"), Some("90000"));
    }

    #[test]
    fn head_seek_clamps_at_file_start() {
        let args = build_smart_head_args("in.mp4", 0.3, 4.046, &h264_params());
        let input = args.iter().position(|a| a == "-i").unwrap();
        assert_eq!(args[input - 1], "0.000000");
        let out_ss = args[input..].iter().position(|a| a == "-ss").unwrap() + input;
        assert_eq!(args[out_ss + 1], "0.300000");
    }

    #[test]
    fn head_omits_unknown_encoder_params() {
        let params = SourceVideoParams {
            profile: Some("Exotic Profile".into()),
            level: Some(-99),
            pix_fmt: None,
            time_base_den: None,
            ..h264_params()
        };
        let args = build_smart_head_args("in.mp4", 10.0, 12.0, &params);
        assert!(!args.iter().any(|a| a == "-profile:v"));
        assert!(!args.iter().any(|a| a == "-level"));
        assert!(!args.iter().any(|a| a == "-pix_fmt"));
        assert!(!args.iter().any(|a| a == "-video_track_timescale"));
    }

    #[test]
    fn tail_stream_copies_from_the_keyframe() {
        let args = build_smart_tail_args("in.mp4", 436.046, 770.921, &h264_params());
        let input = args.iter().position(|a| a == "-i").unwrap();
        assert_eq!(args[input - 1], "436.046000");
        assert_eq!(args[input - 2], "-ss");
        assert_eq!(value_after(&args, "-to"), Some("770.921000"));
        assert_eq!(value_after(&args, "-c:v"), Some("copy"));
        assert!(args.iter().any(|a| a == "-an"));
        assert!(args.iter().any(|a| a == "-copyts"));
        assert_eq!(value_after(&args, "-avoid_negative_ts"), Some("make_zero"));
    }

    // The audio cut must pair -copyts with an *output-side* -ss at the same absolute
    // time: the input seek alone rewinds the demuxer (audio included) to the
    // preceding video keyframe, which would desync the splice by up to one GOP.
    #[test]
    fn audio_uses_copyts_with_output_side_seek() {
        let args = build_smart_audio_args("in.mp4", 434.338, 770.921);
        let input = args.iter().position(|a| a == "-i").unwrap();
        let seeks: Vec<usize> = args
            .iter()
            .enumerate()
            .filter(|(_, a)| *a == "-ss")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(seeks.len(), 2, "needs both input and output seeks");
        assert!(seeks[0] < input && seeks[1] > input);
        assert_eq!(args[seeks[0] + 1], args[seeks[1] + 1]);
        assert!(args.iter().any(|a| a == "-copyts"));
        assert!(args.iter().any(|a| a == "-vn"));
        assert_eq!(value_after(&args, "-c:a"), Some("copy"));
    }

    #[test]
    fn concat_maps_concat_video_and_external_audio() {
        let args = build_smart_concat_args("list.txt", "audio.m4a");
        assert_eq!(value_after(&args, "-f"), Some("concat"));
        assert_eq!(value_after(&args, "-safe"), Some("0"));
        assert_eq!(value_after(&args, "-map"), Some("0:v"));
        assert!(args.windows(2).any(|w| w[0] == "-map" && w[1] == "1:a"));
        assert_eq!(value_after(&args, "-c"), Some("copy"));
    }

    // --- helpers ---

    #[test]
    fn concat_list_entries_escape_single_quotes() {
        assert_eq!(concat_list_entry("/tmp/head.mp4"), "file '/tmp/head.mp4'\n");
        assert_eq!(
            concat_list_entry("/out/Don't Stop.mp4.work/tail.mp4"),
            "file '/out/Don'\\''t Stop.mp4.work/tail.mp4'\n"
        );
    }

    // The head entry must pin its duration so the tail lands exactly at the
    // keyframe; an empty head leaves only the tail. Entries are bare filenames
    // resolved against the list file's directory (an output-dir-relative path
    // would be resolved against the work dir and double up).
    #[test]
    fn concat_list_pins_head_duration() {
        assert_eq!(
            build_concat_list(Some(("head.mp4", 1.708)), "tail.mp4"),
            "file 'head.mp4'\nduration 1.708000\nfile 'tail.mp4'\n"
        );
        assert_eq!(build_concat_list(None, "tail.mp4"), "file 'tail.mp4'\n");
    }

    // ffprobe emits a trailing comma on frame lines carrying side data; the parser
    // must survive that and skip keyframes before the cut (read_intervals starts
    // listing at the keyframe *preceding* the seek point).
    #[test]
    fn next_keyframe_parses_ffprobe_quirks() {
        let csv = "1172.046000,\n1176.046000\n1180.046000,\n";
        assert_eq!(parse_next_keyframe(csv, 1175.963), Some(1176.046));
        // A keyframe a hair before `start` still matches (float noise tolerance).
        assert_eq!(parse_next_keyframe(csv, 1176.0465), Some(1176.046));
        assert_eq!(parse_next_keyframe("", 10.0), None);
        assert_eq!(parse_next_keyframe("garbage\n", 10.0), None);
    }

    #[test]
    fn frame_rate_parsing() {
        assert_eq!(parse_frame_rate("24/1"), Some(24.0));
        assert_eq!(parse_frame_rate("30000/1001"), Some(30000.0 / 1001.0));
        assert_eq!(parse_frame_rate("0/0"), None);
        assert_eq!(parse_frame_rate("bogus"), None);
    }

    #[test]
    fn x264_profile_mapping() {
        assert_eq!(x264_profile_for("High"), Some("high".into()));
        assert_eq!(
            x264_profile_for("Constrained Baseline"),
            Some("baseline".into())
        );
        assert_eq!(x264_profile_for("Something New"), None);
    }
}
