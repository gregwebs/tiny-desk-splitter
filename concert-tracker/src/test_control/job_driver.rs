//! Test Control Job Driver — a deterministic [`JobRunner`] for test-control
//! builds. Hurl scenarios configure per-step outcomes (succeed/fail/block)
//! instead of injecting fake shell commands, and [`Observation`] counts give
//! Hurl a way to assert on concurrency/dependency-edge behavior that has no
//! public HTTP surface. See docs/change/2026-07-15-job-driver-plan.md.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::Mutex;

use serde::Deserialize;

use crate::db::seeds::{
    fake_analysis_timestamps, write_interlude_sentinels, write_legacy_timestamps_json,
    write_track_sentinels, SENTINEL_BYTES,
};
use crate::jobs::{
    DownloadJob, JobRunFuture, JobRunner, JobStepOutcome, OpenMediaOutcome, SplitJob, SplitMode,
};
use crate::model::{concert_dir, sanitize_album};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStepKind {
    Download,
    Split,
    Open,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepOutcome {
    Succeed,
    Fail,
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobPlan {
    pub download: StepOutcome,
    pub split: StepOutcome,
    pub open: StepOutcome,
}

impl Default for JobPlan {
    fn default() -> Self {
        Self {
            download: StepOutcome::Succeed,
            split: StepOutcome::Succeed,
            open: StepOutcome::Succeed,
        }
    }
}

impl JobPlan {
    fn outcome_for(&self, kind: JobStepKind) -> StepOutcome {
        match kind {
            JobStepKind::Download => self.download,
            JobStepKind::Split => self.split,
            JobStepKind::Open => self.open,
        }
    }

    fn apply(
        &mut self,
        download: Option<StepOutcome>,
        split: Option<StepOutcome>,
        open: Option<StepOutcome>,
    ) {
        if let Some(d) = download {
            self.download = d;
        }
        if let Some(s) = split {
            self.split = s;
        }
        if let Some(o) = open {
            self.open = o;
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    pub started: u32,
    pub completed: u32,
    pub failed: u32,
    pub blocked: u32,
    pub released: u32,
}

/// Holds the release channel for a step currently parked in
/// [`JobDriver::run_step`]'s `Block` branch.
struct BlockedStep {
    tx: tokio::sync::oneshot::Sender<StepOutcome>,
}

/// Test Control's job-behavior configuration and concurrency observations.
/// Shared (via `Arc`) between the [`TestControlJobRunner`] driving actual job
/// execution and the Test Control RPC methods that configure/inspect it.
#[derive(Default)]
pub struct JobDriver {
    default_plan: Mutex<JobPlan>,
    concert_plans: Mutex<HashMap<i64, JobPlan>>,
    observations: Mutex<HashMap<(i64, JobStepKind), Observation>>,
    blocked: Mutex<HashMap<(i64, JobStepKind), BlockedStep>>,
}

/// `open` cannot be blocked: `watch`/`watch_track` await `open_media`
/// synchronously inline in the HTTP handler (unlike download/split, which run
/// in a detached spawned task and return `200` immediately). Hurl executes
/// requests strictly sequentially within a file, so nothing could ever call
/// `job_release` while an earlier request is still awaiting its response —
/// a blocked `open` would hang forever, not just currently be unneeded.
const OPEN_BLOCK_REJECTED: &str = "open cannot be set to block: watch/watch_track await \
     open_media synchronously in the HTTP handler, so a blocked open could never be \
     released within Hurl's sequential request execution";

impl JobDriver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update the default plan's present fields, leaving absent fields
    /// unchanged. Rejects `open = block` — see [`OPEN_BLOCK_REJECTED`].
    pub fn set_default_plan(
        &self,
        download: Option<StepOutcome>,
        split: Option<StepOutcome>,
        open: Option<StepOutcome>,
    ) -> anyhow::Result<()> {
        if open == Some(StepOutcome::Block) {
            anyhow::bail!(OPEN_BLOCK_REJECTED);
        }
        self.default_plan
            .lock()
            .unwrap()
            .apply(download, split, open);
        Ok(())
    }

    /// Update `concert_id`'s override plan's present fields, leaving absent
    /// fields unchanged (materializing from the current default plan the
    /// first time an override is set for this concert). Rejects
    /// `open = block` — see [`OPEN_BLOCK_REJECTED`].
    pub fn set_concert_plan(
        &self,
        concert_id: i64,
        download: Option<StepOutcome>,
        split: Option<StepOutcome>,
        open: Option<StepOutcome>,
    ) -> anyhow::Result<()> {
        if open == Some(StepOutcome::Block) {
            anyhow::bail!(OPEN_BLOCK_REJECTED);
        }
        let default = *self.default_plan.lock().unwrap();
        let mut plans = self.concert_plans.lock().unwrap();
        let plan = plans.entry(concert_id).or_insert(default);
        plan.apply(download, split, open);
        Ok(())
    }

    fn resolve_plan(&self, concert_id: i64) -> JobPlan {
        self.concert_plans
            .lock()
            .unwrap()
            .get(&concert_id)
            .copied()
            .unwrap_or_else(|| *self.default_plan.lock().unwrap())
    }

    fn bump(&self, concert_id: i64, kind: JobStepKind, f: impl FnOnce(&mut Observation)) {
        let mut obs = self.observations.lock().unwrap();
        f(obs.entry((concert_id, kind)).or_default());
    }

    pub fn observation(&self, concert_id: i64, kind: JobStepKind) -> Observation {
        self.observations
            .lock()
            .unwrap()
            .get(&(concert_id, kind))
            .copied()
            .unwrap_or_default()
    }

    /// Release a step currently blocked at `(concert_id, kind)`. Errors if no
    /// step is blocked there — Hurl scenarios must poll
    /// `test.assert_job_observation` for `blocked=1` before releasing rather
    /// than racing the step's registration; see
    /// docs/change/2026-07-15-job-driver-plan.md's "Blocked-step release
    /// protocol". `outcome = block` is rejected: a release always resolves a
    /// block, it cannot re-block.
    pub fn release(
        &self,
        concert_id: i64,
        kind: JobStepKind,
        outcome: StepOutcome,
    ) -> anyhow::Result<()> {
        if outcome == StepOutcome::Block {
            anyhow::bail!("release outcome cannot be block");
        }
        let entry = self.blocked.lock().unwrap().remove(&(concert_id, kind));
        match entry {
            Some(BlockedStep { tx }) => {
                // The receiver side only errors if the job task already
                // exited some other way (e.g. process shutdown mid-block);
                // nothing further to do here in that case.
                let _ = tx.send(outcome);
                Ok(())
            }
            None => anyhow::bail!(
                "no blocked {kind:?} step for concert {concert_id}; poll \
                 test.assert_job_observation for blocked=1 before releasing"
            ),
        }
    }

    /// Clear plans back to defaults and clear observations. Does **not**
    /// silently strand blocked steps: dropping their senders resolves each
    /// parked `run_step` call to a deterministic `Failed` outcome (see
    /// `run_step`'s `Err(RecvError)` arm) instead of leaving it hung forever.
    pub fn reset(&self) {
        *self.default_plan.lock().unwrap() = JobPlan::default();
        self.concert_plans.lock().unwrap().clear();
        self.observations.lock().unwrap().clear();
        self.blocked.lock().unwrap().clear();
    }

    /// Shared step-outcome resolution for download/split/open: looks up the
    /// effective plan, bumps observations, and — on an eventual `Succeed`
    /// (immediate or via release) — runs `on_succeed` to produce the
    /// domain-level output files/effects a real job step would have created.
    async fn run_step<F, Fut>(
        &self,
        concert_id: i64,
        kind: JobStepKind,
        on_succeed: F,
    ) -> JobStepOutcome
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = anyhow::Result<()>>,
    {
        let plan = self.resolve_plan(concert_id);
        self.bump(concert_id, kind, |o| o.started += 1);

        let outcome = match plan.outcome_for(kind) {
            StepOutcome::Succeed => Ok(()),
            StepOutcome::Fail => Err(format!("test-control: {kind:?} plan=fail")),
            StepOutcome::Block => {
                // Register the release channel *before* bumping `blocked` —
                // a Hurl scenario polling `assert_job_observation` must never
                // be able to observe `blocked=1` while the entry is not yet
                // in the map, or a `job_release` sent right after that poll
                // would spuriously fail (adversarial review finding).
                let (tx, rx) = tokio::sync::oneshot::channel();
                self.blocked
                    .lock()
                    .unwrap()
                    .insert((concert_id, kind), BlockedStep { tx });
                self.bump(concert_id, kind, |o| o.blocked += 1);
                match rx.await {
                    Ok(StepOutcome::Succeed) => {
                        self.bump(concert_id, kind, |o| o.released += 1);
                        Ok(())
                    }
                    Ok(StepOutcome::Fail) => {
                        self.bump(concert_id, kind, |o| o.released += 1);
                        Err(format!("test-control: {kind:?} released as fail"))
                    }
                    Ok(StepOutcome::Block) => {
                        unreachable!("JobDriver::release rejects a block release outcome")
                    }
                    Err(_recv_error) => {
                        // The sender was dropped without a release (test.reset
                        // ran while this step was blocked) — resolve cleanly
                        // rather than hang or panic. Not counted as
                        // `released`: no release ever actually happened.
                        Err("test-control: block cancelled (server reset while blocked)"
                            .to_string())
                    }
                }
            }
        };

        match outcome {
            Ok(()) => match on_succeed().await {
                Ok(()) => {
                    self.bump(concert_id, kind, |o| o.completed += 1);
                    JobStepOutcome::Succeeded
                }
                Err(e) => {
                    self.bump(concert_id, kind, |o| o.failed += 1);
                    JobStepOutcome::Failed {
                        message: format!("test-control: fixture write failed: {e}"),
                    }
                }
            },
            Err(message) => {
                self.bump(concert_id, kind, |o| o.failed += 1);
                JobStepOutcome::Failed { message }
            }
        }
    }
}

/// [`JobRunner`] implementation backed by a [`JobDriver`]. Test-control's
/// stand-in for `ProductionJobRunner`'s real subprocesses/library calls: on a `Succeed`
/// outcome it writes the same sentinel files the real download/split
/// commands would have produced, since the existing job lifecycle code in
/// `jobs/download.rs`/`jobs/split.rs` reads the filesystem immediately after
/// `JobStepOutcome::Succeeded`.
pub struct TestControlJobRunner {
    driver: std::sync::Arc<JobDriver>,
}

impl TestControlJobRunner {
    pub fn new(driver: std::sync::Arc<JobDriver>) -> Self {
        Self { driver }
    }
}

impl JobRunner for TestControlJobRunner {
    fn run_download<'a>(
        &'a self,
        job: &'a DownloadJob,
        _log_file: Option<&'a Path>,
    ) -> JobRunFuture<'a, JobStepOutcome> {
        Box::pin(async move {
            self.driver
                .run_step(job.concert_id, JobStepKind::Download, || async move {
                    write_download_sentinel(&job.working_dir, &job.album)
                })
                .await
        })
    }

    fn run_split<'a>(
        &'a self,
        job: &'a SplitJob,
        _log_file: Option<&'a Path>,
    ) -> JobRunFuture<'a, JobStepOutcome> {
        Box::pin(async move {
            self.driver
                .run_step(job.concert_id, JobStepKind::Split, || async move {
                    write_split_output(job)
                })
                .await
        })
    }

    fn open_media<'a>(
        &'a self,
        concert_id: i64,
        _path: &'a Path,
    ) -> JobRunFuture<'a, OpenMediaOutcome> {
        Box::pin(async move {
            match self
                .driver
                .run_step(concert_id, JobStepKind::Open, || async { Ok(()) })
                .await
            {
                JobStepOutcome::Succeeded => OpenMediaOutcome::Succeeded,
                JobStepOutcome::Failed { message } => OpenMediaOutcome::Failed { message },
            }
        })
    }
}

fn write_download_sentinel(working_dir: &Path, album: &str) -> anyhow::Result<()> {
    let cd = concert_dir(working_dir, album);
    std::fs::create_dir_all(&cd)?;
    let path = cd.join(format!("{}.mp4", sanitize_album(album)));
    std::fs::write(path, SENTINEL_BYTES)?;
    Ok(())
}

/// Minimal view of the splitter-input JSON `jobs::split::write_splitter_input`
/// writes to `job.json_path` — only the `set_list` titles are needed here
/// (`SplitMode::Analyze` has no other source of the set list; unknown fields
/// are ignored by default, so this does not need to track that struct's full
/// shape).
#[derive(Deserialize)]
struct SplitterInputSetListOnly {
    set_list: Vec<SplitterSongTitleOnly>,
}

#[derive(Deserialize)]
struct SplitterSongTitleOnly {
    title: String,
}

fn read_set_list_titles(json_path: &Path) -> anyhow::Result<Vec<String>> {
    let content = std::fs::read_to_string(json_path)?;
    let parsed: SplitterInputSetListOnly = serde_json::from_str(&content)?;
    Ok(parsed.set_list.into_iter().map(|s| s.title).collect())
}

/// Write the domain-level output files a successful split would have
/// created, branching on [`SplitMode`] exactly like the real splitter's
/// `--timestamps-file`/`--emit-interludes` flags do (see
/// `JobConfig::production`'s split command construction).
fn write_split_output(job: &SplitJob) -> anyhow::Result<()> {
    std::fs::create_dir_all(&job.output_dir)?;
    match &job.mode {
        SplitMode::Analyze => {
            let titles = read_set_list_titles(&job.json_path)?;
            let songs = fake_analysis_timestamps(&titles);
            write_track_sentinels(
                &job.output_dir,
                songs.iter().map(|s| s.title.as_str()),
                "m4a",
            )?;
            write_legacy_timestamps_json(&job.output_dir, &songs)?;
        }
        SplitMode::UserTimestamps { ts, media_duration } => {
            write_track_sentinels(
                &job.output_dir,
                ts.songs().iter().map(|s| s.title.as_str()),
                "m4a",
            )?;
            write_interlude_sentinels(&job.output_dir, ts.songs(), *media_duration)?;
        }
        SplitMode::ResetToAuto(ts) => {
            write_track_sentinels(
                &job.output_dir,
                ts.songs().iter().map(|s| s.title.as_str()),
                "m4a",
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::split_timestamps::{TimestampPayloadSong, ValidatedTimestamps};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn download_job(concert_id: i64, working_dir: &Path, album: &str) -> DownloadJob {
        DownloadJob {
            concert_id,
            source_url: "https://npr.org/test".to_string(),
            album: album.to_string(),
            working_dir: working_dir.to_path_buf(),
        }
    }

    /// Placeholder `ConcertInfo` for `SplitJob` fixtures below. `write_split_output`
    /// (the fake splitter these tests drive) reads `job.json_path`/`job.mode`, not
    /// `job.concert` — this field only needs to type-check here, not match content.
    fn test_concert_info() -> concert_types::ConcertInfo {
        concert_types::ConcertInfo {
            artist: "Test Artist".to_string(),
            source: String::new(),
            show: String::new(),
            date: None,
            album: "Test Album".to_string(),
            description: None,
            set_list: vec![],
            musicians: vec![],
            preview_image_url: None,
            teaser: None,
            timestamps: None,
        }
    }

    fn analyze_split_job(concert_id: i64, output_dir: &Path, titles: &[&str]) -> SplitJob {
        let json = serde_json::json!({ "set_list": titles.iter().map(|t| serde_json::json!({"title": t})).collect::<Vec<_>>() });
        let mut json_file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut json_file, json.to_string().as_bytes()).unwrap();
        SplitJob {
            concert_id,
            concert: test_concert_info(),
            json_path: json_file.path().to_path_buf(),
            input_file: PathBuf::from("/nonexistent/source.mp4"),
            output_dir: output_dir.to_path_buf(),
            mode: SplitMode::Analyze,
            _temp_file: json_file,
            _timestamps_temp_file: None,
            timestamps_path: None,
        }
    }

    fn user_timestamps_split_job(
        concert_id: i64,
        output_dir: &Path,
        set_list: &[String],
        payload: Vec<TimestampPayloadSong>,
        media_duration: f64,
    ) -> SplitJob {
        let ts = ValidatedTimestamps::validate(set_list, None, &payload).unwrap();
        SplitJob {
            concert_id,
            concert: test_concert_info(),
            json_path: PathBuf::from("/unused"),
            input_file: PathBuf::from("/nonexistent/source.mp4"),
            output_dir: output_dir.to_path_buf(),
            mode: SplitMode::UserTimestamps { ts, media_duration },
            _temp_file: tempfile::NamedTempFile::new().unwrap(),
            _timestamps_temp_file: None,
            timestamps_path: None,
        }
    }

    // ---------- JobPlan / JobDriver plan resolution ----------

    #[test]
    fn default_plan_is_all_succeed() {
        let driver = JobDriver::new();
        let plan = driver.resolve_plan(1);
        assert_eq!(plan.download, StepOutcome::Succeed);
        assert_eq!(plan.split, StepOutcome::Succeed);
        assert_eq!(plan.open, StepOutcome::Succeed);
    }

    #[test]
    fn set_default_plan_updates_only_present_fields() {
        let driver = JobDriver::new();
        driver
            .set_default_plan(Some(StepOutcome::Fail), None, None)
            .unwrap();
        let plan = driver.resolve_plan(1);
        assert_eq!(plan.download, StepOutcome::Fail);
        assert_eq!(plan.split, StepOutcome::Succeed);
        assert_eq!(plan.open, StepOutcome::Succeed);
    }

    #[test]
    fn set_default_plan_rejects_open_block() {
        let driver = JobDriver::new();
        let err = driver
            .set_default_plan(None, None, Some(StepOutcome::Block))
            .unwrap_err();
        assert!(err.to_string().contains("open cannot be set to block"));
    }

    #[test]
    fn set_concert_plan_rejects_open_block() {
        let driver = JobDriver::new();
        let err = driver
            .set_concert_plan(1, None, None, Some(StepOutcome::Block))
            .unwrap_err();
        assert!(err.to_string().contains("open cannot be set to block"));
    }

    #[test]
    fn concert_override_takes_precedence_over_default() {
        let driver = JobDriver::new();
        driver
            .set_default_plan(Some(StepOutcome::Fail), None, None)
            .unwrap();
        driver
            .set_concert_plan(42, Some(StepOutcome::Succeed), None, None)
            .unwrap();

        assert_eq!(driver.resolve_plan(42).download, StepOutcome::Succeed);
        assert_eq!(
            driver.resolve_plan(99).download,
            StepOutcome::Fail,
            "concerts without an override still see the default"
        );
    }

    #[test]
    fn concert_override_materializes_from_default_then_updates_independently() {
        let driver = JobDriver::new();
        driver
            .set_default_plan(Some(StepOutcome::Fail), None, None)
            .unwrap();
        // First override call only touches `split`; `download` should
        // materialize from the default (Fail) at this point.
        driver
            .set_concert_plan(1, None, Some(StepOutcome::Fail), None)
            .unwrap();
        assert_eq!(driver.resolve_plan(1).download, StepOutcome::Fail);

        // A later default change must not retroactively affect the
        // already-materialized override.
        driver
            .set_default_plan(Some(StepOutcome::Succeed), None, None)
            .unwrap();
        assert_eq!(
            driver.resolve_plan(1).download,
            StepOutcome::Fail,
            "materialized override is a snapshot, not a live fallback"
        );
    }

    // ---------- run_step: succeed / fail / block+release ----------

    #[tokio::test]
    async fn run_step_succeed_calls_on_succeed_and_bumps_started_and_completed() {
        let driver = JobDriver::new();
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_clone = called.clone();

        let outcome = driver
            .run_step(1, JobStepKind::Download, || async move {
                called_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
            .await;

        assert!(matches!(outcome, JobStepOutcome::Succeeded));
        assert!(called.load(std::sync::atomic::Ordering::SeqCst));
        let obs = driver.observation(1, JobStepKind::Download);
        assert_eq!(obs.started, 1);
        assert_eq!(obs.completed, 1);
        assert_eq!(obs.failed, 0);
    }

    #[tokio::test]
    async fn run_step_fail_bumps_started_and_failed_without_calling_on_succeed() {
        let driver = JobDriver::new();
        driver
            .set_default_plan(Some(StepOutcome::Fail), None, None)
            .unwrap();
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_clone = called.clone();

        let outcome = driver
            .run_step(1, JobStepKind::Download, || async move {
                called_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
            .await;

        assert!(matches!(outcome, JobStepOutcome::Failed { .. }));
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
        let obs = driver.observation(1, JobStepKind::Download);
        assert_eq!(obs.started, 1);
        assert_eq!(obs.failed, 1);
        assert_eq!(obs.completed, 0);
    }

    #[tokio::test]
    async fn run_step_block_then_release_succeed_completes() {
        let driver = Arc::new(JobDriver::new());
        driver
            .set_default_plan(Some(StepOutcome::Block), None, None)
            .unwrap();

        let driver_for_task = driver.clone();
        let handle = tokio::spawn(async move {
            driver_for_task
                .run_step(1, JobStepKind::Download, || async { Ok(()) })
                .await
        });

        // Poll until the step registers as blocked (mirrors the poll-first
        // protocol Hurl scenarios must use before releasing).
        for _ in 0..100 {
            if driver.observation(1, JobStepKind::Download).blocked == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(driver.observation(1, JobStepKind::Download).blocked, 1);

        driver
            .release(1, JobStepKind::Download, StepOutcome::Succeed)
            .unwrap();
        let outcome = handle.await.unwrap();

        assert!(matches!(outcome, JobStepOutcome::Succeeded));
        let obs = driver.observation(1, JobStepKind::Download);
        assert_eq!(obs.released, 1);
        assert_eq!(obs.completed, 1);
    }

    #[tokio::test]
    async fn run_step_block_then_release_fail_fails() {
        let driver = Arc::new(JobDriver::new());
        driver
            .set_default_plan(None, Some(StepOutcome::Block), None)
            .unwrap();

        let driver_for_task = driver.clone();
        let handle = tokio::spawn(async move {
            driver_for_task
                .run_step(1, JobStepKind::Split, || async { Ok(()) })
                .await
        });
        for _ in 0..100 {
            if driver.observation(1, JobStepKind::Split).blocked == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        driver
            .release(1, JobStepKind::Split, StepOutcome::Fail)
            .unwrap();
        let outcome = handle.await.unwrap();

        assert!(matches!(outcome, JobStepOutcome::Failed { .. }));
        let obs = driver.observation(1, JobStepKind::Split);
        assert_eq!(obs.released, 1);
        assert_eq!(obs.failed, 1);
    }

    #[test]
    fn release_without_a_blocked_step_errors() {
        let driver = JobDriver::new();
        let err = driver
            .release(1, JobStepKind::Download, StepOutcome::Succeed)
            .unwrap_err();
        assert!(err.to_string().contains("no blocked"));
    }

    #[test]
    fn release_with_block_outcome_errors() {
        let driver = JobDriver::new();
        let err = driver
            .release(1, JobStepKind::Download, StepOutcome::Block)
            .unwrap_err();
        assert!(err.to_string().contains("cannot be block"));
    }

    #[tokio::test]
    async fn reset_unblocks_a_parked_step_as_a_failure_instead_of_hanging() {
        let driver = Arc::new(JobDriver::new());
        driver
            .set_default_plan(Some(StepOutcome::Block), None, None)
            .unwrap();

        let driver_for_task = driver.clone();
        let handle = tokio::spawn(async move {
            driver_for_task
                .run_step(1, JobStepKind::Download, || async { Ok(()) })
                .await
        });
        for _ in 0..100 {
            if driver.observation(1, JobStepKind::Download).blocked == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        driver.reset();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("reset must unblock the parked step promptly, not hang")
            .unwrap();

        assert!(matches!(outcome, JobStepOutcome::Failed { .. }));
    }

    #[test]
    fn reset_clears_plans_and_observations() {
        let driver = JobDriver::new();
        driver
            .set_default_plan(Some(StepOutcome::Fail), None, None)
            .unwrap();
        driver
            .set_concert_plan(1, Some(StepOutcome::Fail), None, None)
            .unwrap();
        driver.bump(1, JobStepKind::Download, |o| o.started += 1);

        driver.reset();

        assert_eq!(driver.resolve_plan(1).download, StepOutcome::Succeed);
        assert_eq!(driver.observation(1, JobStepKind::Download).started, 0);
    }

    // ---------- TestControlJobRunner: file ownership ----------

    #[tokio::test]
    async fn download_succeed_writes_a_sentinel_findable_by_find_downloaded_file() {
        let tmp = tempfile::tempdir().unwrap();
        let driver = Arc::new(JobDriver::new());
        let runner = TestControlJobRunner::new(driver);
        let job = download_job(1, tmp.path(), "Sentinel Album");

        let outcome = runner.run_download(&job, None).await;

        assert!(matches!(outcome, JobStepOutcome::Succeeded));
        assert!(crate::concert_media::find_downloaded_file(tmp.path(), "Sentinel Album").is_some());
    }

    #[tokio::test]
    async fn download_fail_writes_no_file() {
        let tmp = tempfile::tempdir().unwrap();
        let driver = Arc::new(JobDriver::new());
        driver
            .set_default_plan(Some(StepOutcome::Fail), None, None)
            .unwrap();
        let runner = TestControlJobRunner::new(driver);
        let job = download_job(1, tmp.path(), "No File Album");

        let outcome = runner.run_download(&job, None).await;

        assert!(matches!(outcome, JobStepOutcome::Failed { .. }));
        assert!(crate::concert_media::find_downloaded_file(tmp.path(), "No File Album").is_none());
    }

    #[tokio::test]
    async fn split_analyze_succeed_writes_tracks_and_readable_timestamps_json() {
        let tmp = tempfile::tempdir().unwrap();
        let output_dir = tmp.path().join("out");
        let driver = Arc::new(JobDriver::new());
        let runner = TestControlJobRunner::new(driver);
        let job = analyze_split_job(1, &output_dir, &["Song A", "Song B"]);

        let outcome = runner.run_split(&job, None).await;

        assert!(matches!(outcome, JobStepOutcome::Succeeded));
        assert!(output_dir.join("Song A.m4a").exists());
        assert!(output_dir.join("Song B.m4a").exists());
        let timestamps = crate::jobs::split::read_analysis_timestamps(&output_dir).unwrap();
        assert_eq!(timestamps.len(), 2);
        assert_eq!(timestamps[0].title, "Song A");
    }

    #[tokio::test]
    async fn split_user_timestamps_with_gaps_writes_interlude_sentinels() {
        let tmp = tempfile::tempdir().unwrap();
        let output_dir = tmp.path().join("out");
        let driver = Arc::new(JobDriver::new());
        let runner = TestControlJobRunner::new(driver);
        let set_list = vec!["Alpha".to_string()];
        let payload = vec![TimestampPayloadSong {
            title: "Alpha".to_string(),
            start_time: 20.0,
            end_time: 100.0,
        }];
        // media_duration 200 leaves a head gap [0,20) and a tail gap [100,200)
        // — both well over MIN_INTERLUDE_SECONDS.
        let job = user_timestamps_split_job(1, &output_dir, &set_list, payload, 200.0);

        let outcome = runner.run_split(&job, None).await;

        assert!(matches!(outcome, JobStepOutcome::Succeeded));
        assert!(output_dir.join("Alpha.m4a").exists());
        assert!(
            output_dir.join("interlude_01.m4a").exists(),
            "head gap should produce interlude_01"
        );
        assert!(
            output_dir.join("interlude_02.m4a").exists(),
            "tail gap should produce interlude_02"
        );
    }

    #[tokio::test]
    async fn split_reset_to_auto_writes_tracks_but_no_interludes() {
        use crate::split_timestamps::ValidatedTimestamps;

        let tmp = tempfile::tempdir().unwrap();
        let output_dir = tmp.path().join("out");
        let driver = Arc::new(JobDriver::new());
        let runner = TestControlJobRunner::new(driver);
        let set_list = vec!["Alpha".to_string()];
        let payload = vec![TimestampPayloadSong {
            title: "Alpha".to_string(),
            start_time: 0.0,
            end_time: 90.0,
        }];
        let ts = ValidatedTimestamps::validate(&set_list, None, &payload).unwrap();
        let job = SplitJob {
            concert_id: 1,
            concert: test_concert_info(),
            json_path: PathBuf::from("/unused"),
            input_file: PathBuf::from("/nonexistent/source.mp4"),
            output_dir: output_dir.clone(),
            mode: SplitMode::ResetToAuto(ts),
            _temp_file: tempfile::NamedTempFile::new().unwrap(),
            _timestamps_temp_file: None,
            timestamps_path: None,
        };

        let outcome = runner.run_split(&job, None).await;

        assert!(matches!(outcome, JobStepOutcome::Succeeded));
        assert!(output_dir.join("Alpha.m4a").exists());
        assert!(!output_dir.join("interlude_01.m4a").exists());
    }

    #[tokio::test]
    async fn open_media_succeed_and_fail() {
        let driver = Arc::new(JobDriver::new());
        let runner = TestControlJobRunner::new(driver.clone());
        let path = Path::new("/irrelevant");

        assert!(matches!(
            runner.open_media(1, path).await,
            OpenMediaOutcome::Succeeded
        ));

        driver
            .set_concert_plan(2, None, None, Some(StepOutcome::Fail))
            .unwrap();
        assert!(matches!(
            runner.open_media(2, path).await,
            OpenMediaOutcome::Failed { .. }
        ));
    }
}
