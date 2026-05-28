pub mod archive;
pub mod download;
pub mod split;

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::task::JoinHandle;

use crate::model::concert_dir;
use crate::model::sanitize_album;
use crate::model::Concert;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobKind {
    Download,
    Split,
    Archive,
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

    /// Abort the task for `key` if it exists and is still running.
    /// Returns `true` if a running task was aborted.
    pub fn cancel(&self, key: &JobKey) -> bool {
        let mut map = self.running.lock().unwrap();
        if let Some(handle) = map.remove(key) {
            if !handle.is_finished() {
                handle.abort();
                return true;
            }
        }
        false
    }

    /// Abort all running tasks. Returns the number of tasks aborted.
    pub fn cancel_all(&self) -> usize {
        let mut map = self.running.lock().unwrap();
        let mut count = 0;
        for (_, handle) in map.drain() {
            if !handle.is_finished() {
                handle.abort();
                count += 1;
            }
        }
        count
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

fn binary_exists(path: &Path) -> bool {
    if path.is_absolute() || path.components().count() > 1 {
        path.exists()
    } else {
        which::which(path).is_ok()
    }
}

/// Check that required external binaries are available. Returns a list of
/// human-readable warnings for any that are missing.
pub fn check_dependencies(splitter_bin: &Path) -> Vec<String> {
    let mut warnings = Vec::new();

    if !binary_exists(splitter_bin) {
        warnings.push(format!(
            "splitter binary not found at '{}'. Splitting will fail. \
             Build it with: cargo build --bin live-set-splitter",
            splitter_bin.display()
        ));
    }

    if which::which("yt-dlp").is_err() {
        warnings.push(
            "yt-dlp not found in PATH. Downloads will fail. \
             Install it: https://github.com/yt-dlp/yt-dlp#installation"
                .to_string(),
        );
    }

    if which::which("ffmpeg").is_err() {
        warnings.push(
            "ffmpeg not found in PATH. Splitting will fail. \
             Install it: https://ffmpeg.org/download.html"
                .to_string(),
        );
    }

    warnings
}

impl JobConfig {
    pub fn log_dir(&self) -> PathBuf {
        self.working_dir.join("log").join("job")
    }

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
                    .arg(&job.output_dir)
                    // Silence Leptonica's stderr chatter (e.g. "boxClipToRectangle:
                    // box outside rectangle") emitted during OCR refinement on
                    // near-empty frames. 4 == L_SEVERITY_NONE.
                    .env("LEPT_MSG_SEVERITY", "4");
                cmd
            }),
        }
    }
}

const MEDIA_EXTENSIONS: &[&str] = &[
    "mp4", "m4a", "webm", "mkv", "mp3", "ogg", "opus", "wav", "flac",
];

/// Find the downloaded media file for an album inside its concert dir
/// (`{working_dir}/concerts/{sanitize_album(album)}/`).
///
/// yt-dlp writes the file as `{sanitize_album(album)}.{ext}` where `ext` is
/// picked at runtime (typically `mp4`). We don't know the extension up front,
/// so we list the directory and return the first entry whose file stem matches
/// the sanitized album and has a known media extension.
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
        if !MEDIA_EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
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

macro_rules! log_child_line {
    ($kind:expr, $concert_id:expr, $stream:expr, $line:expr) => {
        match $kind {
            "split" => tracing::info!(
                target: "concert_tracker::jobs::split",
                kind = $kind,
                concert_id = $concert_id,
                stream = $stream,
                "{}",
                $line
            ),
            "download" => tracing::info!(
                target: "concert_tracker::jobs::download",
                kind = $kind,
                concert_id = $concert_id,
                stream = $stream,
                "{}",
                $line
            ),
            _ => tracing::info!(
                target: "concert_tracker::jobs",
                kind = $kind,
                concert_id = $concert_id,
                stream = $stream,
                "{}",
                $line
            ),
        }
    };
}

/// Spawn `cmd` with both stdout and stderr piped, stream every line through
/// `tracing::info!` so it appears in concert-web's log, and return the exit
/// status plus the last [`STDERR_TAIL_LINES`] lines of stderr joined by `\n`.
/// The stderr tail is what gets written into the DB error column on failure,
/// preserving the inline error shown in the UI.
///
/// When `log_file` is `Some`, every line is also written to that file
/// (prefixed with `[stdout]` or `[stderr]`). I/O errors on the log file
/// are warned but do not fail the job.
pub async fn run_with_logging(
    mut cmd: Command,
    kind: &'static str,
    concert_id: i64,
    log_file: Option<&Path>,
) -> std::io::Result<(ExitStatus, String)> {
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn()?;

    let log_handle: Option<Arc<Mutex<std::fs::File>>> =
        log_file.and_then(|path| match std::fs::File::create(path) {
            Ok(f) => Some(Arc::new(Mutex::new(f))),
            Err(e) => {
                tracing::warn!("failed to create job log file {}: {}", path.display(), e);
                None
            }
        });

    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    let log_for_stdout = log_handle.clone();
    let stdout_task: JoinHandle<()> = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            log_child_line!(kind, concert_id, "stdout", line);
            if let Some(ref f) = log_for_stdout {
                if let Ok(mut f) = f.lock() {
                    let _ = writeln!(f, "[stdout] {}", line);
                }
            }
        }
    });

    let log_for_stderr = log_handle;
    let stderr_task: JoinHandle<Vec<String>> = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut tail: std::collections::VecDeque<String> =
            std::collections::VecDeque::with_capacity(STDERR_TAIL_LINES);
        while let Ok(Some(line)) = lines.next_line().await {
            log_child_line!(kind, concert_id, "stderr", line);
            if let Some(ref f) = log_for_stderr {
                if let Ok(mut f) = f.lock() {
                    let _ = writeln!(f, "[stderr] {}", line);
                }
            }
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

pub fn persist_job_log(
    conn: &rusqlite::Connection,
    concert_id: i64,
    name: &str,
    error: &str,
    temp_file: Option<tempfile::NamedTempFile>,
    log_dir: &Path,
) {
    match crate::db::insert_failed_job(conn, concert_id, name, error) {
        Ok(job_id) => {
            if let Some(tf) = temp_file {
                let final_path = log_dir.join(format!("{}.log", job_id));
                if let Err(e) = tf.persist(&final_path) {
                    tracing::warn!(
                        "failed to persist job log to {}: {}",
                        final_path.display(),
                        e
                    );
                }
            }
        }
        Err(e) => tracing::warn!("failed to insert failed job record: {}", e),
    }
}

pub fn download_job_from_concert(
    concert: &Concert,
    working_dir: &Path,
) -> anyhow::Result<DownloadJob> {
    let album = concert.album.as_deref().unwrap_or(&concert.title);
    Ok(DownloadJob {
        concert_id: concert.id,
        source_url: concert.source_url.clone(),
        album: album.to_string(),
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

    #[test]
    fn find_downloaded_file_skips_jpg_preview_image() {
        let dir = tempfile::tempdir().unwrap();
        let cd = make_concert_dir(dir.path(), "Foo Album");
        File::create(cd.join("Foo Album.jpg")).unwrap();
        File::create(cd.join("Foo Album.mp4")).unwrap();
        let found = find_downloaded_file(dir.path(), "Foo Album").unwrap();
        assert_eq!(found, cd.join("Foo Album.mp4"));
    }

    #[test]
    fn find_downloaded_file_returns_none_when_only_image_exists() {
        let dir = tempfile::tempdir().unwrap();
        let cd = make_concert_dir(dir.path(), "Foo Album");
        File::create(cd.join("Foo Album.jpg")).unwrap();
        assert!(find_downloaded_file(dir.path(), "Foo Album").is_none());
    }

    #[tokio::test]
    async fn run_with_logging_captures_stderr_tail_and_exit_code() {
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            "echo out1; echo out2; echo err1 >&2; echo err2 >&2; exit 5",
        ]);
        let (status, stderr_tail) = run_with_logging(cmd, "test", 42, None).await.unwrap();
        assert_eq!(status.code(), Some(5));
        assert_eq!(stderr_tail, "err1\nerr2");
    }

    #[tokio::test]
    async fn run_with_logging_caps_stderr_tail_to_last_lines() {
        let total = STDERR_TAIL_LINES + 10;
        let script = format!(
            "for i in $(seq 1 {}); do echo err$i >&2; done; exit 1",
            total
        );
        let mut cmd = Command::new("sh");
        cmd.args(["-c", &script]);
        let (status, stderr_tail) = run_with_logging(cmd, "test", 0, None).await.unwrap();
        assert_eq!(status.code(), Some(1));
        let lines: Vec<&str> = stderr_tail.lines().collect();
        assert_eq!(lines.len(), STDERR_TAIL_LINES);
        assert_eq!(
            *lines.first().unwrap(),
            format!("err{}", total - STDERR_TAIL_LINES + 1)
        );
        assert_eq!(*lines.last().unwrap(), format!("err{}", total));
    }

    #[tokio::test]
    async fn cancel_aborts_running_task() {
        let registry = JobRegistry::new();
        let key = JobKey {
            concert_id: 1,
            kind: JobKind::Download,
        };
        let handle = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        });
        registry.insert(key.clone(), handle);
        assert!(registry.is_running(&key));

        assert!(registry.cancel(&key));
        assert!(!registry.is_running(&key));
    }

    #[test]
    fn cancel_returns_false_for_unknown_key() {
        let registry = JobRegistry::new();
        let key = JobKey {
            concert_id: 99,
            kind: JobKind::Split,
        };
        assert!(!registry.cancel(&key));
    }

    #[tokio::test]
    async fn cancel_all_aborts_all_running_tasks() {
        let registry = JobRegistry::new();
        for id in 1..=3 {
            let key = JobKey {
                concert_id: id,
                kind: JobKind::Download,
            };
            let handle = tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            });
            registry.insert(key, handle);
        }
        assert_eq!(registry.cancel_all(), 3);
        assert_eq!(registry.cancel_all(), 0);
    }

    #[tokio::test]
    async fn run_with_logging_writes_log_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("test.log");
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            "echo out1; echo out2; echo err1 >&2; echo err2 >&2; exit 0",
        ]);
        let (status, _) = run_with_logging(cmd, "test", 1, Some(&log_path))
            .await
            .unwrap();
        assert!(status.success());
        let content = std::fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("[stdout] out1"));
        assert!(content.contains("[stdout] out2"));
        assert!(content.contains("[stderr] err1"));
        assert!(content.contains("[stderr] err2"));
    }

    #[tokio::test]
    async fn run_with_logging_without_log_file_still_works() {
        let cmd = Command::new("true");
        let (status, _) = run_with_logging(cmd, "test", 1, None).await.unwrap();
        assert!(status.success());
    }

    #[test]
    fn binary_exists_finds_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("my-tool");
        File::create(&bin).unwrap();
        assert!(binary_exists(&bin));
    }

    #[test]
    fn binary_exists_rejects_missing_absolute_path() {
        assert!(!binary_exists(Path::new("/nonexistent/binary")));
    }

    #[test]
    fn binary_exists_finds_command_on_path() {
        assert!(binary_exists(Path::new("sh")));
    }

    #[test]
    fn binary_exists_rejects_unknown_command() {
        assert!(!binary_exists(Path::new("nonexistent-binary-xyz-123")));
    }

    #[test]
    fn check_dependencies_warns_for_missing_splitter() {
        let warnings = check_dependencies(Path::new("/nonexistent/live-set-splitter"));
        assert!(warnings
            .iter()
            .any(|w| w.contains("splitter binary not found")));
    }

    #[test]
    fn check_dependencies_no_splitter_warning_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("live-set-splitter");
        File::create(&bin).unwrap();
        let warnings = check_dependencies(&bin);
        assert!(!warnings.iter().any(|w| w.contains("splitter")));
    }
}
