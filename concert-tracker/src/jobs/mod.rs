pub mod download;
pub mod split;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::task::JoinHandle;

use crate::model::concert_dir;
use crate::model::sanitize_album;
use crate::model::Concert;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JobKind {
    Download,
    Split,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JobKey {
    pub concert_id: i64,
    pub kind: JobKind,
}

pub struct JobRegistry {
    running: Mutex<HashMap<JobKey, JoinHandle<()>>>,
}

impl JobRegistry {
    pub fn new() -> Self {
        JobRegistry {
            running: Mutex::new(HashMap::new()),
        }
    }

    pub fn is_running(&self, key: &JobKey) -> bool {
        let map = self.running.lock().unwrap();
        map.get(key).map(|h| !h.is_finished()).unwrap_or(false)
    }

    pub fn insert(&self, key: JobKey, handle: JoinHandle<()>) {
        self.running.lock().unwrap().insert(key, handle);
    }
}

impl Default for JobRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub struct DownloadJob {
    pub concert_id: i64,
    pub source_url: String,
    pub album: String,
    pub working_dir: PathBuf,
}

pub struct SplitJob {
    pub concert_id: i64,
    pub json_path: PathBuf,
    pub input_file: PathBuf,
    /// Directory the splitter writes per-song files into. With the
    /// `concerts/<album>/` layout this is the concert directory itself.
    pub output_dir: PathBuf,
    pub _temp_file: tempfile::NamedTempFile,
}

#[derive(Clone)]
pub struct JobConfig {
    pub working_dir: PathBuf,
    pub download_cmd: Arc<dyn Fn(&DownloadJob) -> Command + Send + Sync>,
    pub split_cmd: Arc<dyn Fn(&SplitJob) -> Command + Send + Sync>,
}

/// Default location of the splitter binary. Looks for `live-set-splitter`
/// as a sibling of the currently running executable, so `cargo run` and
/// `cargo install` both place it correctly. Falls back to PATH lookup.
pub fn default_splitter_bin() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sibling = parent.join("live-set-splitter");
            if sibling.exists() {
                return sibling;
            }
        }
    }
    PathBuf::from("live-set-splitter")
}

impl JobConfig {
    pub fn production(working_dir: PathBuf, splitter_bin: PathBuf) -> Self {
        let wd = working_dir.clone();
        JobConfig {
            working_dir: working_dir.clone(),
            download_cmd: Arc::new(move |job: &DownloadJob| {
                let cd = concert_dir(&wd, &job.album);
                let _ = std::fs::create_dir_all(&cd);
                let out = cd
                    .join(format!("{}.%(ext)s", sanitize_album(&job.album)))
                    .to_string_lossy()
                    .to_string();
                let mut cmd = Command::new("yt-dlp");
                cmd.arg("-o").arg(out).arg(&job.source_url);
                cmd
            }),
            split_cmd: Arc::new(move |job: &SplitJob| {
                let mut cmd = Command::new(&splitter_bin);
                cmd.arg(&job.json_path)
                    .arg("--input-file")
                    .arg(&job.input_file)
                    .arg("--output-dir")
                    .arg(&job.output_dir);
                cmd
            }),
        }
    }
}

/// Find the downloaded media file for an album inside its concert dir
/// (`{working_dir}/concerts/{sanitize_album(album)}/`).
///
/// yt-dlp writes the file as `{sanitize_album(album)}.{ext}` where `ext` is
/// picked at runtime (typically `mp4`). We don't know the extension up front,
/// so we list the directory and return the first entry whose file stem matches
/// the sanitized album. `.json` is excluded as a safety belt against scraped
/// metadata sidecars.
pub fn find_downloaded_file(working_dir: &Path, album: &str) -> Option<PathBuf> {
    let expected_stem = sanitize_album(album);
    let cd = concert_dir(working_dir, album);
    let entries = std::fs::read_dir(&cd).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem != expected_stem {
            continue;
        }
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if ext.eq_ignore_ascii_case("json") {
            continue;
        }
        return Some(path);
    }
    None
}

/// How many of the most recent stderr lines to retain for the DB error message
/// when a child process exits non-zero. Bounded so a chatty subprocess can't
/// blow up memory or the `*_errors` column.
const STDERR_TAIL_LINES: usize = 64;

/// Spawn `cmd` with both stdout and stderr piped, stream every line through
/// `tracing::info!` so it appears in concert-web's log, and return the exit
/// status plus the last [`STDERR_TAIL_LINES`] lines of stderr joined by `\n`.
/// The stderr tail is what gets written into the DB error column on failure,
/// preserving the inline error shown in the UI.
pub async fn run_with_logging(
    mut cmd: Command,
    kind: &'static str,
    concert_id: i64,
) -> std::io::Result<(ExitStatus, String)> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    let stdout_task: JoinHandle<()> = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::info!(
                target: "concert_tracker::jobs::child",
                kind = kind,
                concert_id = concert_id,
                stream = "stdout",
                "{}",
                line
            );
        }
    });

    let stderr_task: JoinHandle<Vec<String>> = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut tail: std::collections::VecDeque<String> =
            std::collections::VecDeque::with_capacity(STDERR_TAIL_LINES);
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::info!(
                target: "concert_tracker::jobs::child",
                kind = kind,
                concert_id = concert_id,
                stream = "stderr",
                "{}",
                line
            );
            if tail.len() == STDERR_TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(line);
        }
        tail.into_iter().collect()
    });

    let status = child.wait().await?;
    let _ = stdout_task.await;
    let tail_lines = stderr_task.await.unwrap_or_default();
    Ok((status, tail_lines.join("\n")))
}

pub fn download_job_from_concert(
    concert: &Concert,
    working_dir: &Path,
) -> anyhow::Result<DownloadJob> {
    let album = concert
        .album
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Concert {} has no album", concert.id))?;
    Ok(DownloadJob {
        concert_id: concert.id,
        source_url: concert.source_url.clone(),
        album: album.clone(),
        working_dir: working_dir.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};

    fn make_concert_dir(working_dir: &Path, album: &str) -> PathBuf {
        let cd = concert_dir(working_dir, album);
        fs::create_dir_all(&cd).unwrap();
        cd
    }

    #[test]
    fn find_downloaded_file_returns_match_for_known_extension() {
        let dir = tempfile::tempdir().unwrap();
        let cd = make_concert_dir(dir.path(), "Foo Album");
        File::create(cd.join("Foo Album.mp4")).unwrap();
        let found = find_downloaded_file(dir.path(), "Foo Album").unwrap();
        assert_eq!(found, cd.join("Foo Album.mp4"));
    }

    #[test]
    fn find_downloaded_file_ignores_json_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let cd = make_concert_dir(dir.path(), "Foo Album");
        File::create(cd.join("Foo Album.json")).unwrap();
        File::create(cd.join("Foo Album.mp4")).unwrap();
        let found = find_downloaded_file(dir.path(), "Foo Album").unwrap();
        assert_eq!(found, cd.join("Foo Album.mp4"));
    }

    #[test]
    fn find_downloaded_file_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_downloaded_file(dir.path(), "Foo Album").is_none());
    }

    #[test]
    fn find_downloaded_file_handles_colons_in_album() {
        let dir = tempfile::tempdir().unwrap();
        let cd = make_concert_dir(dir.path(), "A: B");
        File::create(cd.join("A B.mp4")).unwrap();
        let found = find_downloaded_file(dir.path(), "A: B").unwrap();
        assert_eq!(found, cd.join("A B.mp4"));
    }

    #[test]
    fn find_downloaded_file_returns_none_when_only_json_exists() {
        let dir = tempfile::tempdir().unwrap();
        let cd = make_concert_dir(dir.path(), "Foo Album");
        File::create(cd.join("Foo Album.json")).unwrap();
        assert!(find_downloaded_file(dir.path(), "Foo Album").is_none());
    }

    #[tokio::test]
    async fn run_with_logging_captures_stderr_tail_and_exit_code() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo out1; echo out2; echo err1 >&2; echo err2 >&2; exit 5"]);
        let (status, stderr_tail) = run_with_logging(cmd, "test", 42).await.unwrap();
        assert_eq!(status.code(), Some(5));
        assert_eq!(stderr_tail, "err1\nerr2");
    }

    #[tokio::test]
    async fn run_with_logging_caps_stderr_tail_to_last_lines() {
        let total = STDERR_TAIL_LINES + 10;
        let script = format!("for i in $(seq 1 {}); do echo err$i >&2; done; exit 1", total);
        let mut cmd = Command::new("sh");
        cmd.args(["-c", &script]);
        let (status, stderr_tail) = run_with_logging(cmd, "test", 0).await.unwrap();
        assert_eq!(status.code(), Some(1));
        let lines: Vec<&str> = stderr_tail.lines().collect();
        assert_eq!(lines.len(), STDERR_TAIL_LINES);
        assert_eq!(*lines.first().unwrap(), format!("err{}", total - STDERR_TAIL_LINES + 1));
        assert_eq!(*lines.last().unwrap(), format!("err{}", total));
    }
}
