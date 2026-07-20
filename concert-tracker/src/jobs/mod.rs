pub mod archive;
pub mod download;
pub mod prepare;
pub mod run;
pub mod scrape_queue;
pub mod split;

use std::collections::HashMap;
use std::future::Future;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

pub use crate::concert_media::find_downloaded_file;
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

/// Arbitrates exactly one terminal outcome (succeeded / failed / cancelled)
/// for a single Job Run. `claim` is a one-shot compare-and-swap: the first
/// caller to succeed owns writing the terminal state and releasing the
/// registry slot; every later caller must write nothing. See the Job Run
/// invariants in `docs/jobs.md`.
pub struct TerminalGate(AtomicBool);

impl TerminalGate {
    fn new() -> Self {
        TerminalGate(AtomicBool::new(false))
    }

    /// Attempt to claim the gate. Returns `true` for exactly one caller
    /// across this gate's lifetime.
    pub fn claim(&self) -> bool {
        self.0
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

struct JobSlot {
    /// `None` while the slot is Reserved (admission in progress, not yet an
    /// accepted Job Run); `Some` once the spawned run task's handle has been
    /// attached via [`JobReservation::activate`].
    handle: Option<JoinHandle<()>>,
    terminal: Arc<TerminalGate>,
}

type SlotsMap = Arc<Mutex<HashMap<JobKey, JobSlot>>>;

/// A reservation for a not-yet-accepted Job Run. Dropping it without calling
/// [`activate`](Self::activate) rolls the reservation back (removes the
/// slot) — this is admission rollback for synchronous rejection or an
/// acceptance failure between `try_reserve` and spawning the run task.
pub struct JobReservation {
    key: JobKey,
    slots: SlotsMap,
    terminal: Arc<TerminalGate>,
    activate_tx: Option<oneshot::Sender<()>>,
    activated: bool,
}

impl JobReservation {
    pub fn terminal_gate(&self) -> Arc<TerminalGate> {
        self.terminal.clone()
    }

    /// Attach the spawned task's handle to the registry slot and release the
    /// paired [`ActivationSignal`] so the task can begin work. Must be
    /// called exactly once, after `tokio::spawn` returns — both steps are
    /// infallible, which is what lets `try_mark_started` remain the last
    /// fallible step of admission (its `download_started` event cannot be
    /// rolled back; see [`run::submit`]).
    pub fn activate(mut self, handle: JoinHandle<()>) {
        self.activated = true;
        {
            let mut slots = self.slots.lock().unwrap();
            if let Some(slot) = slots.get_mut(&self.key) {
                slot.handle = Some(handle);
            }
        }
        if let Some(tx) = self.activate_tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for JobReservation {
    fn drop(&mut self) {
        if !self.activated {
            self.slots.lock().unwrap().remove(&self.key);
        }
    }
}

/// Awaited by the spawned run task before it does any work (setup, execute,
/// terminal commit). This closes the race where a trivially fast run could
/// reach `release` before [`JobReservation::activate`] has even attached its
/// handle to the registry slot.
pub struct ActivationSignal(oneshot::Receiver<()>);

impl ActivationSignal {
    pub async fn wait(self) {
        let _ = self.0.await;
    }
}

pub struct JobRegistry {
    slots: SlotsMap,
    /// dependents[upstream] = jobs to start when `upstream` completes
    /// successfully. The reverse view (dependent → upstream) is the
    /// dependent's `depends_on`; a queued dependent has no spawned task
    /// until its upstream succeeds. On upstream failure or cancellation the
    /// queued dependents are dropped (they never run).
    dependents: Mutex<HashMap<JobKey, Vec<JobKey>>>,
}

impl JobRegistry {
    pub fn new() -> Self {
        JobRegistry {
            slots: Arc::new(Mutex::new(HashMap::new())),
            dependents: Mutex::new(HashMap::new()),
        }
    }

    /// True for a Reserved slot (admission in progress) or an unfinished
    /// accepted Job Run — i.e. whether a new request for `key` must be
    /// rejected as a duplicate. Preserves `prepare.rs`'s existing semantics.
    pub fn is_running(&self, key: &JobKey) -> bool {
        let slots = self.slots.lock().unwrap();
        match slots.get(key) {
            None => false,
            Some(slot) => match &slot.handle {
                None => true,
                Some(h) => !h.is_finished(),
            },
        }
    }

    /// Reserve `key` for a new Job Run before any DB acceptance work runs.
    /// `None` means `key` is already reserved or running. A slot left by a
    /// finished handle is treated as free and replaced. Returns the
    /// reservation guard plus the signal the spawned run task must await.
    pub fn try_reserve(&self, key: JobKey) -> Option<(JobReservation, ActivationSignal)> {
        let mut slots = self.slots.lock().unwrap();
        if let Some(existing) = slots.get(&key) {
            let occupied = match &existing.handle {
                None => true,
                Some(h) => !h.is_finished(),
            };
            if occupied {
                return None;
            }
        }
        let terminal = Arc::new(TerminalGate::new());
        let (tx, rx) = oneshot::channel();
        slots.insert(
            key.clone(),
            JobSlot {
                handle: None,
                terminal: terminal.clone(),
            },
        );
        Some((
            JobReservation {
                key,
                slots: self.slots.clone(),
                terminal,
                activate_tx: Some(tx),
                activated: false,
            },
            ActivationSignal(rx),
        ))
    }

    /// Remove `key`'s slot outright. Called by whichever party wins the
    /// terminal gate (the run task on success/failure, `run::cancel` on a
    /// won cancel), after the terminal transaction commits and dependency
    /// handling completes.
    pub fn release(&self, key: &JobKey) {
        self.slots.lock().unwrap().remove(key);
    }

    /// Abort `key`'s handle if it is still running, then remove the slot.
    /// Used only by a caller that has already won the terminal gate for
    /// `key` — the abort is a courtesy to stop wasted work, not what makes
    /// the terminal outcome exclusive (the gate does that).
    pub fn abort_and_release(&self, key: &JobKey) {
        if let Some(slot) = self.slots.lock().unwrap().remove(key) {
            if let Some(handle) = slot.handle {
                if !handle.is_finished() {
                    handle.abort();
                }
            }
        }
    }

    /// The terminal gate for `key`, if `key` names an *accepted* Job Run
    /// (a slot with an attached handle). A merely Reserved slot (admission
    /// still in progress) has no cancellable Job Run yet, so this returns
    /// `None` for it — see [`run::cancel`].
    pub fn terminal_gate(&self, key: &JobKey) -> Option<Arc<TerminalGate>> {
        let slots = self.slots.lock().unwrap();
        let slot = slots.get(key)?;
        slot.handle.as_ref()?;
        Some(slot.terminal.clone())
    }

    /// Register `dependent` to start when `upstream` completes successfully.
    /// Deduplicated; returns `true` when newly added.
    pub fn add_dependent(&self, upstream: JobKey, dependent: JobKey) -> bool {
        let mut map = self.dependents.lock().unwrap();
        let deps = map.entry(upstream).or_default();
        if deps.contains(&dependent) {
            return false;
        }
        tracing::debug!(?dependent, "queued dependent job");
        deps.push(dependent);
        true
    }

    /// Remove and return all queued dependents of `upstream`. Called when
    /// `upstream` finishes: on success the caller starts them, on failure
    /// the caller drops them.
    pub fn take_dependents(&self, upstream: &JobKey) -> Vec<JobKey> {
        self.dependents
            .lock()
            .unwrap()
            .remove(upstream)
            .unwrap_or_default()
    }

    /// Whether `dependent` is queued to run after `upstream`.
    pub fn has_dependent(&self, upstream: &JobKey, dependent: &JobKey) -> bool {
        self.dependents
            .lock()
            .unwrap()
            .get(upstream)
            .map(|deps| deps.contains(dependent))
            .unwrap_or(false)
    }

    /// Drop every dependency edge touching `key`: jobs queued behind it, and
    /// its own queued entry under any upstream. Called when `key` fails or is
    /// cancelled so queued dependents never run.
    pub fn drop_dependency_edges(&self, key: &JobKey) -> bool {
        let mut map = self.dependents.lock().unwrap();
        let mut dropped_any = false;
        if let Some(dropped) = map.remove(key) {
            dropped_any = true;
            tracing::info!(?key, ?dropped, "dropped queued dependents");
        }
        for deps in map.values_mut() {
            let before = deps.len();
            deps.retain(|d| d != key);
            dropped_any |= deps.len() != before;
        }
        map.retain(|_, deps| !deps.is_empty());
        dropped_any
    }

    /// Abort all running tasks and drop all queued dependents. Returns the
    /// number of tasks aborted.
    pub fn cancel_all(&self) -> usize {
        self.dependents.lock().unwrap().clear();
        let mut slots = self.slots.lock().unwrap();
        let mut count = 0;
        for (_, slot) in slots.drain() {
            if let Some(handle) = slot.handle {
                if !handle.is_finished() {
                    handle.abort();
                    count += 1;
                }
            }
        }
        count
    }
}

/// Start every job queued behind `upstream`, which has just completed
/// successfully. Synchronous on purpose: each dependent start runs in its own
/// spawned task, so the `run_download`/`run_split` future types never contain
/// each other.
pub fn spawn_dependents(
    db: Arc<Mutex<rusqlite::Connection>>,
    registry: Arc<JobRegistry>,
    config: JobConfig,
    upstream: &JobKey,
) {
    for dep in registry.take_dependents(upstream) {
        tracing::info!(?upstream, ?dep, "starting dependent job");
        let upstream = upstream.clone();
        let db = db.clone();
        let registry = registry.clone();
        let config = config.clone();
        tokio::spawn(async move {
            let result = match dep.kind {
                JobKind::Download => download::start_download(db, registry, config, dep.concert_id)
                    .await
                    .map(|_| ()),
                JobKind::Split => match split::start_split(
                    db,
                    registry,
                    config,
                    dep.concert_id,
                    SplitMode::Analyze,
                )
                .await
                {
                    Ok(split::StartOutcome::Spawned | split::StartOutcome::AlreadyRunning) => {
                        Ok(())
                    }
                    Ok(split::StartOutcome::NotDownloaded) => {
                        tracing::warn!(
                            ?upstream,
                            ?dep,
                            reason = "not_downloaded",
                            "dependent split rejected after current-state validation"
                        );
                        Ok(())
                    }
                    Err(error) => Err(error),
                },
                JobKind::Archive => {
                    tracing::warn!(?dep, "archive jobs cannot be chained; skipping");
                    Ok(())
                }
            };
            if let Err(e) = result {
                tracing::warn!(
                    ?upstream,
                    ?dep,
                    reason = "submission_error",
                    error = %e,
                    "dependent job failed to start"
                );
            }
        });
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

/// How tracks should be split: automated analysis, user-supplied timestamps, or
/// reset to previously stored automated timestamps.
#[derive(Clone)]
pub enum SplitMode {
    Analyze,
    /// User-supplied timestamps plus the source media duration (from ffprobe at
    /// handler time). The duration is needed so the splitter can derive and cut
    /// interlude files that cover the full `[0, media_duration]` timeline.
    UserTimestamps {
        ts: crate::split_timestamps::ValidatedTimestamps,
        media_duration: f64,
    },
    /// Reset to automated timestamps; the ValidatedTimestamps were resolved by the handler.
    ResetToAuto(crate::split_timestamps::ValidatedTimestamps),
}

impl SplitMode {
    pub fn name(&self) -> &'static str {
        match self {
            SplitMode::Analyze => "analyze",
            SplitMode::UserTimestamps { .. } => "user-timestamps",
            SplitMode::ResetToAuto(_) => "reset-to-auto",
        }
    }
}

pub struct SplitJob {
    pub concert_id: i64,
    pub json_path: PathBuf,
    pub input_file: PathBuf,
    /// Directory the splitter writes per-song files into. With the
    /// `concerts/<album>/` layout this is the concert directory itself.
    pub output_dir: PathBuf,
    pub mode: SplitMode,
    /// Kept alive so the temp file isn't deleted before the splitter reads it.
    pub _temp_file: tempfile::NamedTempFile,
    /// Timestamps temp file for user/reset modes; kept alive alongside _temp_file.
    pub _timestamps_temp_file: Option<tempfile::NamedTempFile>,
    pub timestamps_path: Option<PathBuf>,
}

pub type JobRunFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub enum JobStepOutcome {
    Succeeded,
    Failed { message: String },
}

pub enum OpenMediaOutcome {
    Succeeded,
    Failed { message: String },
}

/// Executes domain job steps. Production uses subprocess commands behind this
/// interface; test-control can replace the runner with deterministic job
/// completion without changing the lifecycle orchestration.
pub trait JobRunner: Send + Sync {
    fn run_download<'a>(
        &'a self,
        job: &'a DownloadJob,
        log_file: Option<&'a Path>,
    ) -> JobRunFuture<'a, JobStepOutcome>;

    fn run_split<'a>(
        &'a self,
        job: &'a SplitJob,
        log_file: Option<&'a Path>,
    ) -> JobRunFuture<'a, JobStepOutcome>;

    fn open_media<'a>(
        &'a self,
        concert_id: i64,
        path: &'a Path,
    ) -> JobRunFuture<'a, OpenMediaOutcome>;
}

pub type DownloadCommandFn = Arc<dyn Fn(&DownloadJob) -> Command + Send + Sync>;
pub type SplitCommandFn = Arc<dyn Fn(&SplitJob) -> Command + Send + Sync>;
pub type OpenCommandFn = Arc<dyn Fn(&Path) -> Command + Send + Sync>;

pub struct CommandJobRunner {
    download_cmd: DownloadCommandFn,
    split_cmd: SplitCommandFn,
    open_cmd: OpenCommandFn,
}

impl CommandJobRunner {
    pub fn new(
        download_cmd: DownloadCommandFn,
        split_cmd: SplitCommandFn,
        open_cmd: OpenCommandFn,
    ) -> Self {
        Self {
            download_cmd,
            split_cmd,
            open_cmd,
        }
    }
}

impl JobRunner for CommandJobRunner {
    fn run_download<'a>(
        &'a self,
        job: &'a DownloadJob,
        log_file: Option<&'a Path>,
    ) -> JobRunFuture<'a, JobStepOutcome> {
        Box::pin(async move {
            let cmd = (self.download_cmd)(job);
            command_job_outcome(
                cmd,
                "download",
                job.concert_id,
                log_file,
                ". Is yt-dlp installed? See: https://github.com/yt-dlp/yt-dlp#installation",
            )
            .await
        })
    }

    fn run_split<'a>(
        &'a self,
        job: &'a SplitJob,
        log_file: Option<&'a Path>,
    ) -> JobRunFuture<'a, JobStepOutcome> {
        Box::pin(async move {
            let cmd = (self.split_cmd)(job);
            command_job_outcome(
                cmd,
                "split",
                job.concert_id,
                log_file,
                ". Is live-set-splitter built? Run: cargo build --bin live-set-splitter",
            )
            .await
        })
    }

    fn open_media<'a>(
        &'a self,
        _concert_id: i64,
        path: &'a Path,
    ) -> JobRunFuture<'a, OpenMediaOutcome> {
        Box::pin(async move {
            let mut cmd = (self.open_cmd)(path);
            match cmd.status().await {
                Ok(status) if status.success() => OpenMediaOutcome::Succeeded,
                Ok(status) => OpenMediaOutcome::Failed {
                    message: format!("`open` exited {:?}", status.code()),
                },
                Err(err) => OpenMediaOutcome::Failed {
                    message: format!("spawn `open` failed: {err}"),
                },
            }
        })
    }
}

async fn command_job_outcome(
    cmd: Command,
    kind: &'static str,
    concert_id: i64,
    log_file: Option<&Path>,
    not_found_hint: &'static str,
) -> JobStepOutcome {
    match run_with_logging(cmd, kind, concert_id, log_file).await {
        Ok((status, _)) if status.success() => JobStepOutcome::Succeeded,
        Ok((status, stderr_tail)) => JobStepOutcome::Failed {
            message: format!("exit {:?}: {}", status.code(), stderr_tail.trim()),
        },
        Err(err) => {
            let hint = if err.kind() == std::io::ErrorKind::NotFound {
                not_found_hint
            } else {
                ""
            };
            JobStepOutcome::Failed {
                message: format!("spawn error: {err}{hint}"),
            }
        }
    }
}

#[derive(Clone)]
pub struct JobConfig {
    pub working_dir: PathBuf,
    runner: Arc<dyn JobRunner>,
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

    pub fn with_runner(working_dir: PathBuf, runner: Arc<dyn JobRunner>) -> Self {
        Self {
            working_dir,
            runner,
        }
    }

    pub fn from_commands(
        working_dir: PathBuf,
        download_cmd: DownloadCommandFn,
        split_cmd: SplitCommandFn,
        open_cmd: OpenCommandFn,
    ) -> Self {
        Self::with_runner(
            working_dir,
            Arc::new(CommandJobRunner::new(download_cmd, split_cmd, open_cmd)),
        )
    }

    pub async fn run_download(&self, job: &DownloadJob, log_file: Option<&Path>) -> JobStepOutcome {
        self.runner.run_download(job, log_file).await
    }

    pub async fn run_split(&self, job: &SplitJob, log_file: Option<&Path>) -> JobStepOutcome {
        self.runner.run_split(job, log_file).await
    }

    pub async fn open_media(&self, concert_id: i64, path: &Path) -> OpenMediaOutcome {
        self.runner.open_media(concert_id, path).await
    }

    /// Test config: every external command is a no-op (`true`), so handlers can
    /// be driven without yt-dlp, the splitter, or a media player on the host.
    pub fn test(working_dir: PathBuf) -> Self {
        Self::from_commands(
            working_dir,
            Arc::new(|_| Command::new("true")),
            Arc::new(|_| Command::new("true")),
            Arc::new(|_| Command::new("true")),
        )
    }

    pub fn production(working_dir: PathBuf, splitter_bin: PathBuf, open_program: String) -> Self {
        let wd = working_dir.clone();
        Self::from_commands(
            working_dir,
            Arc::new(move |job: &DownloadJob| {
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
            Arc::new(move |job: &SplitJob| {
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
                // User/reset modes: skip analysis and cut at the provided boundaries.
                // Deliberately no --refine-timestamps so the splitter doesn't
                // rewrite timestamps.json, preserving the automated record.
                if let Some(ts_path) = &job.timestamps_path {
                    cmd.arg("--timestamps-file").arg(ts_path);
                }
                // For user-timestamps splits, also cut interlude files so the
                // full [0, media_duration] timeline is covered on disk.
                if let SplitMode::UserTimestamps { media_duration, .. } = &job.mode {
                    cmd.arg("--emit-interludes")
                        .arg("--media-duration")
                        .arg(media_duration.to_string());
                }
                cmd
            }),
            Arc::new(move |path: &Path| {
                let mut cmd = Command::new(&open_program);
                cmd.arg(path);
                cmd
            }),
        )
    }
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
    use std::fs::File;

    /// Reserve `key` and immediately activate it with `handle`, producing an
    /// accepted, cancellable slot equivalent to what production admission
    /// (`jobs::run::submit`) creates, without driving the full async engine.
    /// Stands in for the removed legacy `JobRegistry::insert` in tests that
    /// only need a running slot to exist.
    fn reserve_running(registry: &JobRegistry, key: JobKey, handle: JoinHandle<()>) {
        let (reservation, _signal) = registry
            .try_reserve(key)
            .expect("key must not already be reserved/running");
        reservation.activate(handle);
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
    async fn abort_and_release_aborts_running_task_and_frees_the_slot() {
        let registry = JobRegistry::new();
        let key = JobKey {
            concert_id: 1,
            kind: JobKind::Download,
        };
        let handle = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        });
        reserve_running(&registry, key.clone(), handle);
        assert!(registry.is_running(&key));

        registry.abort_and_release(&key);
        assert!(!registry.is_running(&key));
    }

    // ── TerminalGate / JobReservation ───────────────────────────────────────

    #[test]
    fn terminal_gate_claim_wins_exactly_once() {
        let gate = TerminalGate::new();
        assert!(gate.claim());
        assert!(!gate.claim());
        assert!(!gate.claim());
    }

    fn dl_key_n(concert_id: i64) -> JobKey {
        JobKey {
            concert_id,
            kind: JobKind::Download,
        }
    }

    #[test]
    fn try_reserve_marks_key_running_before_acceptance() {
        let registry = JobRegistry::new();
        let key = dl_key_n(1);
        let (_reservation, _signal) = registry.try_reserve(key.clone()).unwrap();
        assert!(
            registry.is_running(&key),
            "a Reserved slot must block duplicate admission"
        );
    }

    #[test]
    fn try_reserve_rejects_when_already_reserved_or_running() {
        let registry = JobRegistry::new();
        let key = dl_key_n(1);
        let (_reservation, _signal) = registry.try_reserve(key.clone()).unwrap();
        assert!(
            registry.try_reserve(key).is_none(),
            "second reservation for the same key must be rejected"
        );
    }

    #[test]
    fn dropping_reservation_without_activating_rolls_back() {
        let registry = JobRegistry::new();
        let key = dl_key_n(1);
        {
            let (_reservation, _signal) = registry.try_reserve(key.clone()).unwrap();
            assert!(registry.is_running(&key));
        } // reservation dropped without activate()
        assert!(
            !registry.is_running(&key),
            "un-activated reservation must roll back on drop"
        );
        assert!(
            registry.terminal_gate(&key).is_none(),
            "rolled-back key has no accepted Job Run"
        );
    }

    #[test]
    fn try_reserve_after_rollback_succeeds() {
        let registry = JobRegistry::new();
        let key = dl_key_n(1);
        drop(registry.try_reserve(key.clone()).unwrap());
        assert!(
            registry.try_reserve(key).is_some(),
            "a rolled-back key must be reservable again"
        );
    }

    #[tokio::test]
    async fn activate_attaches_handle_and_exposes_terminal_gate() {
        let registry = JobRegistry::new();
        let key = dl_key_n(1);
        let (reservation, signal) = registry.try_reserve(key.clone()).unwrap();
        assert!(
            registry.terminal_gate(&key).is_none(),
            "Reserved (not yet activated) slot has no cancellable Job Run"
        );

        let handle = tokio::spawn(async move {
            signal.wait().await;
        });
        reservation.activate(handle);

        assert!(registry.is_running(&key));
        assert!(
            registry.terminal_gate(&key).is_some(),
            "activated slot has a cancellable Job Run"
        );
    }

    #[tokio::test]
    async fn activation_signal_blocks_task_until_activate_is_called() {
        // Guards the activate-vs-fast-finish race: even a task that would
        // finish instantly must not observe completion (here: the shared
        // counter) before `activate` releases it.
        let registry = JobRegistry::new();
        let key = dl_key_n(1);
        let (reservation, signal) = registry.try_reserve(key.clone()).unwrap();

        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ran_in_task = ran.clone();
        let handle = tokio::spawn(async move {
            signal.wait().await;
            ran_in_task.store(true, Ordering::SeqCst);
        });

        // Give the spawned task every chance to run ahead if it weren't
        // parked on the signal.
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            !ran.load(Ordering::SeqCst),
            "task must not proceed before activate() is called"
        );

        reservation.activate(handle);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            ran.load(Ordering::SeqCst),
            "task must proceed once activated"
        );
    }

    fn dl_key(concert_id: i64) -> JobKey {
        JobKey {
            concert_id,
            kind: JobKind::Download,
        }
    }

    fn split_key(concert_id: i64) -> JobKey {
        JobKey {
            concert_id,
            kind: JobKind::Split,
        }
    }

    #[test]
    fn add_dependent_deduplicates() {
        let registry = JobRegistry::new();
        assert!(registry.add_dependent(dl_key(1), split_key(1)));
        assert!(!registry.add_dependent(dl_key(1), split_key(1)));
        assert!(registry.has_dependent(&dl_key(1), &split_key(1)));
        assert_eq!(registry.take_dependents(&dl_key(1)), vec![split_key(1)]);
    }

    #[test]
    fn take_dependents_empties_the_queue() {
        let registry = JobRegistry::new();
        registry.add_dependent(dl_key(1), split_key(1));
        assert_eq!(registry.take_dependents(&dl_key(1)), vec![split_key(1)]);
        assert!(registry.take_dependents(&dl_key(1)).is_empty());
        assert!(!registry.has_dependent(&dl_key(1), &split_key(1)));
    }

    #[test]
    fn take_dependents_returns_empty_for_unknown_upstream() {
        let registry = JobRegistry::new();
        assert!(registry.take_dependents(&dl_key(42)).is_empty());
    }

    #[test]
    fn drop_dependency_edges_drops_queued_dependents_of_the_key() {
        let registry = JobRegistry::new();
        registry.add_dependent(dl_key(1), split_key(1));

        assert!(registry.drop_dependency_edges(&dl_key(1)));
        assert!(!registry.has_dependent(&dl_key(1), &split_key(1)));
    }

    #[test]
    fn drop_dependency_edges_removes_the_key_from_other_upstreams_queues() {
        let registry = JobRegistry::new();
        registry.add_dependent(dl_key(1), split_key(1));
        // Dropping edges for the queued (not yet running) split removes its edge.
        registry.drop_dependency_edges(&split_key(1));
        assert!(!registry.has_dependent(&dl_key(1), &split_key(1)));
    }

    #[test]
    fn cancel_all_clears_all_dependents() {
        let registry = JobRegistry::new();
        registry.add_dependent(dl_key(1), split_key(1));
        registry.add_dependent(dl_key(2), split_key(2));
        registry.cancel_all();
        assert!(!registry.has_dependent(&dl_key(1), &split_key(1)));
        assert!(!registry.has_dependent(&dl_key(2), &split_key(2)));
    }

    #[test]
    fn drop_dependency_edges_returns_false_for_unknown_key() {
        let registry = JobRegistry::new();
        let key = JobKey {
            concert_id: 99,
            kind: JobKind::Split,
        };
        assert!(!registry.drop_dependency_edges(&key));
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
            reserve_running(&registry, key, handle);
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
