use anyhow::{Context, Result};
use concert_types::ConcertInfo;
use rusqlite::Connection;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tempfile::NamedTempFile;

use crate::concert_media::{find_downloaded_file, tracks_present_on_disk};
use crate::db;
use crate::jobs::run::{self, Admission, JobCancellation, JobRequest};
use crate::jobs::{
    JobConfig, JobKey, JobKind, JobRegistry, JobRunFuture, JobStepFailure, JobStepOutcome,
    SplitJob, SplitMode,
};
use crate::model::{concert_dir, Concert, Musician};
use crate::split_timestamps::ValidatedTimestamps;

#[derive(Debug)]
pub enum StartOutcome {
    Spawned,
    AlreadyRunning,
    NotDownloaded,
}

#[derive(Debug)]
enum SplitValidationError {
    NotDownloaded,
}

impl std::fmt::Display for SplitValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Concert source file not downloaded")
    }
}

impl std::error::Error for SplitValidationError {}

pub(crate) struct SplitInput {
    concert: Concert,
    mode: SplitMode,
}

pub(crate) struct SplitSetup {
    concert: Concert,
    execution: SplitExecution,
}

enum SplitExecution {
    // Boxed: `SplitJob` now carries a typed `ConcertInfo` (#141), which made
    // this variant much larger than `ExistingTracksRecovery`.
    Run(Box<SplitJob>),
    ExistingTracksRecovery { output_dir: PathBuf },
}

pub(crate) enum SplitCompletionFacts {
    Analyzed {
        tracks_present: Vec<bool>,
        timestamps: Vec<concert_types::SongTimestamp>,
    },
    UserTimestamps {
        tracks_present: Vec<bool>,
        timestamps: ValidatedTimestamps,
        media_duration: f64,
    },
    ResetToAuto {
        tracks_present: Vec<bool>,
    },
    ExistingTracksRecovery {
        tracks_present: Vec<bool>,
        auto_timestamps: Option<Vec<concert_types::SongTimestamp>>,
    },
}

/// Build the typed [`ConcertInfo`] the splitter consumes, from concert-web's
/// own `Concert` domain model. Used by both adapters: the library adapter
/// passes this directly to `ConcertSplitRequest` (`jobs::split_library`); the
/// CLI adapter's subprocess reads the JSON `write_concert_info_json` below
/// serializes to `SplitJob::json_path` (positionally, as its first argument —
/// see `build_cli_split_command` in `jobs::mod`). `ConcertInfo` is a superset
/// of the splitter's on-disk shape (extra fields default), so one typed value
/// serves both without a separate transport DTO.
fn build_concert_info(concert: &Concert) -> ConcertInfo {
    ConcertInfo {
        artist: concert.artist.clone().unwrap_or_default(),
        source: concert.source_url.clone(),
        show: "Tiny Desk Concerts".to_string(),
        date: concert.concert_date.clone(),
        album: concert.album.clone().unwrap_or_default(),
        description: concert.description.clone(),
        set_list: concert
            .set_list
            .iter()
            .map(|title| concert_types::Song {
                title: title.clone(),
            })
            .collect(),
        musicians: concert
            .musicians
            .iter()
            .map(|m: &Musician| concert_types::Musician {
                name: m.name.clone(),
                instruments: m.instruments.clone(),
            })
            .collect(),
        preview_image_url: None,
        teaser: None,
        timestamps: None,
    }
}

fn write_concert_info_json(info: &ConcertInfo) -> Result<NamedTempFile> {
    let mut file = NamedTempFile::new()?;
    let json = serde_json::to_string(info)?;
    file.write_all(json.as_bytes())?;
    Ok(file)
}

/// Start a split job for the given concert. Requires downloaded_at IS NOT NULL.
pub async fn start_split(
    db: Arc<Mutex<Connection>>,
    registry: Arc<JobRegistry>,
    config: JobConfig,
    concert_id: i64,
    mode: SplitMode,
) -> Result<StartOutcome> {
    let request = SplitRequest::new(concert_id, mode, config);
    match run::submit(db, registry, request).await {
        Ok(Admission::Accepted) => Ok(StartOutcome::Spawned),
        Ok(Admission::AlreadyRunning) => Ok(StartOutcome::AlreadyRunning),
        Err(error) if error.downcast_ref::<SplitValidationError>().is_some() => {
            Ok(StartOutcome::NotDownloaded)
        }
        Err(error) => Err(error),
    }
}

pub(crate) struct SplitRequest {
    concert_id: i64,
    mode: SplitMode,
    config: JobConfig,
}

impl SplitRequest {
    pub(crate) fn new(concert_id: i64, mode: SplitMode, config: JobConfig) -> Self {
        Self {
            concert_id,
            mode,
            config,
        }
    }
}

pub(crate) struct SplitCancellation {
    concert_id: i64,
}

impl SplitCancellation {
    pub(crate) fn new(concert_id: i64) -> Self {
        Self { concert_id }
    }
}

impl JobCancellation for SplitCancellation {
    fn key(&self) -> JobKey {
        JobKey {
            concert_id: self.concert_id,
            kind: JobKind::Split,
        }
    }

    fn job_name(&self) -> &'static str {
        "split"
    }

    fn record_failure(&self, conn: &Connection, error: &str) -> Result<()> {
        db::lifecycle::mark_split_failed(conn, self.concert_id, error)
    }

    fn has_stale_in_progress(&self, conn: &Connection) -> Result<bool> {
        Ok(conn.query_row(
            "SELECT split_started_at IS NOT NULL FROM concerts WHERE id = ?1",
            [self.concert_id],
            |row| row.get(0),
        )?)
    }
}

impl JobCancellation for SplitRequest {
    fn key(&self) -> JobKey {
        JobKey {
            concert_id: self.concert_id,
            kind: JobKind::Split,
        }
    }

    fn job_name(&self) -> &'static str {
        "split"
    }

    fn record_failure(&self, conn: &Connection, error: &str) -> Result<()> {
        db::lifecycle::mark_split_failed(conn, self.concert_id, error)
    }

    fn has_stale_in_progress(&self, conn: &Connection) -> Result<bool> {
        Ok(conn.query_row(
            "SELECT split_started_at IS NOT NULL FROM concerts WHERE id = ?1",
            [self.concert_id],
            |row| row.get(0),
        )?)
    }
}

impl JobRequest for SplitRequest {
    type Input = SplitInput;
    type Setup = SplitSetup;
    type Facts = SplitCompletionFacts;

    fn validate(&self, conn: &Connection) -> Result<SplitInput> {
        let concert = db::concerts::get_concert(conn, self.concert_id)?;
        if concert.downloaded_at.is_none() {
            return Err(SplitValidationError::NotDownloaded.into());
        }
        let mode_ts = match &self.mode {
            SplitMode::UserTimestamps { ts, .. } | SplitMode::ResetToAuto(ts) => Some(ts),
            SplitMode::Analyze => None,
        };
        if let Some(ts) = mode_ts {
            anyhow::ensure!(
                ts.songs().len() == concert.set_list.len(),
                "Timestamp count {} does not match set_list length {} for concert {}",
                ts.songs().len(),
                concert.set_list.len(),
                self.concert_id
            );
        }
        Ok(SplitInput {
            concert,
            mode: self.mode.clone(),
        })
    }

    fn try_mark_started(&self, conn: &Connection) -> Result<bool> {
        db::lifecycle::try_mark_split_started(conn, self.concert_id)
    }

    fn setup(&self, input: SplitInput) -> Result<SplitSetup> {
        let album = input.concert.album.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "Concert {} has no album, cannot locate input file",
                self.concert_id
            )
        })?;
        let output_dir = concert_dir(&self.config.working_dir, album);
        if let Some(input_file) = find_downloaded_file(&self.config.working_dir, album) {
            let concert_info = build_concert_info(&input.concert);
            let temp_file = write_concert_info_json(&concert_info)?;
            let (timestamps_temp_file, timestamps_path) = match &input.mode {
                SplitMode::Analyze => (None, None),
                SplitMode::UserTimestamps { ts, .. } | SplitMode::ResetToAuto(ts) => {
                    let file = write_timestamps_file(ts)?;
                    let path = file.path().to_path_buf();
                    (Some(file), Some(path))
                }
            };
            let outcome_file = NamedTempFile::new()?;
            let outcome_path = outcome_file.path().to_path_buf();
            let job = SplitJob {
                concert_id: input.concert.id,
                concert: concert_info,
                json_path: temp_file.path().to_path_buf(),
                input_file,
                output_dir,
                mode: input.mode,
                _temp_file: temp_file,
                _timestamps_temp_file: timestamps_temp_file,
                timestamps_path,
                outcome_path,
                _outcome_file: outcome_file,
            };
            return Ok(SplitSetup {
                concert: input.concert,
                execution: SplitExecution::Run(Box::new(job)),
            });
        }
        if matches!(input.mode, SplitMode::Analyze)
            && !input.concert.set_list.is_empty()
            && crate::scan::has_split_tracks(&output_dir, album)
        {
            return Ok(SplitSetup {
                concert: input.concert,
                execution: SplitExecution::ExistingTracksRecovery { output_dir },
            });
        }
        anyhow::bail!(
            "Downloaded file for concert {} (album {:?}) not found in {}",
            self.concert_id,
            album,
            self.config.working_dir.display()
        )
    }

    fn execute<'a>(
        &'a self,
        setup: &'a SplitSetup,
        log_file: Option<&'a Path>,
    ) -> JobRunFuture<'a, JobStepOutcome> {
        match &setup.execution {
            SplitExecution::Run(job) => Box::pin(self.config.run_split(job, log_file)),
            SplitExecution::ExistingTracksRecovery { .. } => {
                Box::pin(async { JobStepOutcome::Succeeded })
            }
        }
    }

    fn gather_success_facts(&self, setup: &SplitSetup) -> Result<SplitCompletionFacts> {
        let album = setup.concert.album.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "Concert {} lost its album during split completion",
                self.concert_id
            )
        })?;
        let output_dir = concert_dir(&self.config.working_dir, album);
        let _published_split =
            live_set_splitter::publication::SharedPublicationLock::acquire(&output_dir)?;
        let tracks_present =
            tracks_present_on_disk(&self.config.working_dir, album, &setup.concert.set_list);
        match &setup.execution {
            SplitExecution::Run(job) => match &job.mode {
                SplitMode::Analyze => Ok(SplitCompletionFacts::Analyzed {
                    tracks_present,
                    timestamps: read_analysis_timestamps(&job.output_dir)?,
                }),
                SplitMode::UserTimestamps { ts, media_duration } => {
                    Ok(SplitCompletionFacts::UserTimestamps {
                        tracks_present,
                        timestamps: ts.clone(),
                        media_duration: *media_duration,
                    })
                }
                SplitMode::ResetToAuto(_) => {
                    Ok(SplitCompletionFacts::ResetToAuto { tracks_present })
                }
            },
            SplitExecution::ExistingTracksRecovery { output_dir } => {
                let auto_timestamps = match read_analysis_timestamps(output_dir) {
                    Ok(timestamps) => Some(timestamps),
                    Err(error) => {
                        tracing::warn!(
                            concert_id = self.concert_id,
                            "split recovery could not backfill timestamps: {:#}",
                            error
                        );
                        None
                    }
                };
                Ok(SplitCompletionFacts::ExistingTracksRecovery {
                    tracks_present,
                    auto_timestamps,
                })
            }
        }
    }

    fn commit_success(&self, conn: &Connection, facts: SplitCompletionFacts) -> Result<()> {
        match facts {
            SplitCompletionFacts::Analyzed {
                tracks_present,
                timestamps,
            } => {
                db::split_timestamps::set_tracks_present(conn, self.concert_id, &tracks_present)?;
                db::split_timestamps::set_auto_split_timestamps(
                    conn,
                    self.concert_id,
                    &timestamps,
                )?;
                db::split_timestamps::clear_user_split_timestamps(conn, self.concert_id)?;
            }
            SplitCompletionFacts::UserTimestamps {
                tracks_present,
                timestamps,
                media_duration,
            } => {
                db::split_timestamps::set_tracks_present(conn, self.concert_id, &tracks_present)?;
                db::split_timestamps::set_user_split_timestamps(
                    conn,
                    self.concert_id,
                    timestamps.songs(),
                )?;
                db::split_timestamps::set_media_duration(conn, self.concert_id, media_duration)?;
            }
            SplitCompletionFacts::ResetToAuto { tracks_present } => {
                db::split_timestamps::set_tracks_present(conn, self.concert_id, &tracks_present)?;
                db::split_timestamps::clear_user_split_timestamps(conn, self.concert_id)?;
            }
            SplitCompletionFacts::ExistingTracksRecovery {
                tracks_present,
                auto_timestamps,
            } => {
                db::split_timestamps::set_tracks_present(conn, self.concert_id, &tracks_present)?;
                if let Some(timestamps) = auto_timestamps {
                    db::split_timestamps::set_auto_split_timestamps(
                        conn,
                        self.concert_id,
                        &timestamps,
                    )?;
                }
            }
        }
        db::lifecycle::mark_split_succeeded(conn, self.concert_id)
    }

    fn record_step_failure(&self, conn: &Connection, failure: &JobStepFailure) -> Result<()> {
        match failure {
            JobStepFailure::RecoverablePartialSplit { message, tracks } => {
                let concert = db::concerts::get_concert(conn, self.concert_id)?;
                anyhow::ensure!(
                    concert.split_at.is_none(),
                    "Recoverable Partial Split cannot replace successful split state"
                );
                let mut seen = std::collections::BTreeSet::new();
                let mut tracks_present = vec![false; concert.set_list.len()];
                for title in tracks {
                    anyhow::ensure!(seen.insert(title), "duplicate partial track title");
                    let index = concert
                        .set_list
                        .iter()
                        .position(|candidate| candidate == title)
                        .ok_or_else(|| anyhow::anyhow!("unknown partial track title {title:?}"))?;
                    tracks_present[index] = true;
                }
                db::split_timestamps::set_tracks_present(conn, self.concert_id, &tracks_present)?;
                db::lifecycle::mark_split_failed(conn, self.concert_id, message)
            }
            JobStepFailure::Ordinary { message } => {
                db::lifecycle::mark_split_failed(conn, self.concert_id, message)
            }
        }
    }

    fn log_dir(&self) -> Option<PathBuf> {
        Some(self.config.log_dir())
    }

    fn spawn_dependents(&self, db: Arc<Mutex<Connection>>, registry: Arc<JobRegistry>) {
        crate::jobs::spawn_dependents(db, registry, self.config.clone(), &self.key());
    }
}

fn write_timestamps_file(ts: &ValidatedTimestamps) -> Result<NamedTempFile> {
    let file_data = ts.to_timestamps_file();
    let json = serde_json::to_string(&file_data)?;
    let mut file = NamedTempFile::new()?;
    file.write_all(json.as_bytes())?;
    Ok(file)
}

/// Read the automated timestamps from the `timestamps.json` the splitter writes
/// into `output_dir` after analysis. Returns an error on I/O or parse failure.
pub fn read_analysis_timestamps(output_dir: &Path) -> Result<Vec<concert_types::SongTimestamp>> {
    let path = output_dir.join("timestamps.json");
    let file =
        std::fs::File::open(&path).with_context(|| format!("Failed to open {}", path.display()))?;
    let info: ConcertInfo = serde_json::from_reader(std::io::BufReader::new(file))
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    info.timestamps
        .ok_or_else(|| anyhow::anyhow!("timestamps.json has no timestamps field"))
}

/// Return automated timestamps for a concert from the DB, falling back to
/// parsing `{concert_dir}/timestamps.json` from disk (lazy backfill for
/// concerts split before this feature). Persists the backfilled value into
/// the DB column on success.
pub fn auto_timestamps_with_backfill(
    conn: &Connection,
    working_dir: &Path,
    concert: &Concert,
) -> Result<Option<Vec<concert_types::SongTimestamp>>> {
    let stored = db::split_timestamps::get_split_timestamps(conn, concert.id)?;
    if stored.auto.is_some() {
        return Ok(stored.auto);
    }
    // Attempt disk backfill
    let album = match concert.album.as_deref() {
        Some(a) => a,
        None => return Ok(None),
    };
    let output_dir = concert_dir(working_dir, album);
    match live_set_splitter::publication::with_shared_lock(&output_dir, || {
        let timestamps = read_analysis_timestamps(&output_dir)?;
        db::split_timestamps::set_auto_split_timestamps(conn, concert.id, &timestamps)?;
        Ok(timestamps)
    }) {
        Ok(ts) => {
            tracing::debug!(
                "auto_timestamps_with_backfill: backfilled {} timestamps for concert {} from disk",
                ts.len(),
                concert.id
            );
            Ok(Some(ts))
        }
        Err(e) => {
            tracing::debug!(
                "auto_timestamps_with_backfill: no timestamps.json for concert {}: {}",
                concert.id,
                e
            );
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{
        DownloadJob, JobConfig, JobRegistry, JobRunFuture, JobRunner, JobStepOutcome,
        OpenMediaOutcome, SplitBackend,
    };
    use crate::split_timestamps::TimestampPayloadSong;
    use std::fs;
    use std::path::PathBuf;
    use tokio::process::Command;

    fn config_for(working_dir: PathBuf) -> JobConfig {
        JobConfig::from_commands(
            working_dir,
            Arc::new(|_: &DownloadJob| Command::new("true")),
            // Long sleep so a real spawn would be visibly running. Tests that
            // exercise the auto-recover path expect the splitter to never run;
            // tests that exercise the spawn path check the registry, not exit.
            Arc::new(|_| {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", "sleep 10"]);
                cmd
            }),
            Arc::new(|_| Command::new("true")),
        )
    }

    struct PartialRunner;

    impl JobRunner for PartialRunner {
        fn run_download<'a>(
            &'a self,
            _job: &'a DownloadJob,
            _log_file: Option<&'a Path>,
        ) -> JobRunFuture<'a, JobStepOutcome> {
            Box::pin(async { JobStepOutcome::Succeeded })
        }

        fn run_split<'a>(
            &'a self,
            _job: &'a SplitJob,
            _log_file: Option<&'a Path>,
        ) -> JobRunFuture<'a, JobStepOutcome> {
            Box::pin(async {
                JobStepOutcome::Failed(JobStepFailure::RecoverablePartialSplit {
                    message: "second track cut failed".to_string(),
                    tracks: vec!["First".to_string()],
                })
            })
        }

        fn open_media<'a>(
            &'a self,
            _concert_id: i64,
            _path: &'a Path,
        ) -> JobRunFuture<'a, OpenMediaOutcome> {
            Box::pin(async { OpenMediaOutcome::Succeeded })
        }
    }

    /// Delegates the scraped-concert arrangement to `SeedContext`, then keeps
    /// the `set_downloaded_at_if_missing` backfill call local — that function
    /// (the scan-backfill path: no `downloaded_extension`, no Download event)
    /// is not equivalent to `seed_lifecycle_concert { downloaded: true }`.
    fn seeded_db(album: &str, set_list: Vec<String>) -> (Arc<Mutex<Connection>>, i64) {
        let conn = db::connection::open_in_memory().unwrap();
        let id = db::seeds::SeedContext::new(&conn)
            .seed_scraped_concert(db::seeds::SeedScrapedConcert {
                source_url: Some(format!("https://npr.org/c/{}", album)),
                title: Some(album.to_string()),
                concert_date: None,
                artist: Some("Test Artist".to_string()),
                album: Some(album.to_string()),
                set_list: Some(set_list),
            })
            .unwrap()
            .id;
        db::lifecycle::set_downloaded_at_if_missing(&conn, id, "2024-01-01T00:00:00Z").unwrap();
        (Arc::new(Mutex::new(conn)), id)
    }

    async fn wait_until_finished(registry: &JobRegistry, concert_id: i64) {
        let key = JobKey {
            concert_id,
            kind: JobKind::Split,
        };
        for _ in 0..100 {
            if !registry.is_running(&key) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("split job did not finish");
    }

    #[tokio::test]
    async fn recoverable_partial_availability_commits_with_job_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Partial Job Album";
        let set_list = vec!["First".to_string(), "Second".to_string()];
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        fs::write(cd.join(format!("{album}.m4a")), b"source").unwrap();
        let (database, id) = seeded_db(album, set_list);
        let registry = Arc::new(JobRegistry::new());
        let config = JobConfig::with_runner(tmp.path().to_path_buf(), Arc::new(PartialRunner));

        let started = start_split(
            database.clone(),
            registry.clone(),
            config,
            id,
            SplitMode::Analyze,
        )
        .await
        .unwrap();
        assert!(matches!(started, StartOutcome::Spawned));
        wait_until_finished(&registry, id).await;

        let conn = database.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, id).unwrap();
        assert_eq!(concert.tracks_present, vec![true, false]);
        assert!(concert.split_at.is_none());
        assert!(concert.split_started_at.is_none());
        assert_eq!(concert.split_errors.len(), 1);
        assert_eq!(concert.split_errors[0].error, "second track cut failed");
        let failed = db::failed_jobs::list_failed_jobs(&conn, 10).unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].concert_id, id);
        assert_eq!(failed[0].failure_message, "second track cut failed");
    }

    #[tokio::test]
    async fn auto_recovers_when_tracks_present_and_source_missing() {
        // Imported-concert scenario: no source file in the concert dir, but
        // all set_list tracks already exist as audio sidecars. Album includes
        // a colon to also exercise the sanitize_album path end-to-end.
        let tmp = tempfile::tempdir().unwrap();
        let album = "Bloc Party: Tiny Desk Concert";
        let set_list = vec![
            "Banquet".to_string(),
            "Signs".to_string(),
            "Mercury".to_string(),
            "Blue".to_string(),
        ];
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        for t in &set_list {
            fs::write(cd.join(format!("{}.m4a", t)), b"audio").unwrap();
            fs::write(cd.join(format!("{}.mp4", t)), b"video").unwrap();
        }

        let (db, id) = seeded_db(album, set_list.clone());
        let stored_before = db::split_timestamps::tests::make_timestamps();
        {
            let conn = db.lock().unwrap();
            db::split_timestamps::set_auto_split_timestamps(&conn, id, &stored_before).unwrap();
            db::split_timestamps::set_user_split_timestamps(&conn, id, &stored_before).unwrap();
        }
        let registry = Arc::new(JobRegistry::new());
        let outcome = start_split(
            db.clone(),
            registry.clone(),
            config_for(tmp.path().to_path_buf()),
            id,
            SplitMode::Analyze,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, StartOutcome::Spawned));
        wait_until_finished(&registry, id).await;
        let conn = db.lock().unwrap();
        let c = db::concerts::get_concert(&conn, id).unwrap();
        assert!(c.split_at.is_some(), "split_at should be set");
        assert_eq!(c.tracks_present, vec![true; 4]);
        assert!(c.split_errors.is_empty(), "no error should be recorded");
        let stored_after = db::split_timestamps::get_split_timestamps(&conn, id).unwrap();
        assert_eq!(stored_after.auto, Some(stored_before.clone()));
        assert_eq!(stored_after.user, Some(stored_before));
        // Auto-recovery must not spawn a splitter job.
        assert!(!registry.is_running(&JobKey {
            concert_id: id,
            kind: JobKind::Split,
        }));
    }

    #[tokio::test]
    async fn auto_recovers_partial_tracks() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Partial Album";
        let set_list = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        // Only A and C present on disk.
        fs::write(cd.join("A.m4a"), b"audio").unwrap();
        fs::write(cd.join("C.m4a"), b"audio").unwrap();
        fs::write(cd.join("timestamps.json"), b"not json").unwrap();

        let (db, id) = seeded_db(album, set_list);
        let stored_before = db::split_timestamps::tests::make_timestamps();
        {
            let conn = db.lock().unwrap();
            db::split_timestamps::set_auto_split_timestamps(&conn, id, &stored_before).unwrap();
            db::split_timestamps::set_user_split_timestamps(&conn, id, &stored_before).unwrap();
        }
        let registry = Arc::new(JobRegistry::new());
        let outcome = start_split(
            db.clone(),
            registry.clone(),
            config_for(tmp.path().to_path_buf()),
            id,
            SplitMode::Analyze,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, StartOutcome::Spawned));
        wait_until_finished(&registry, id).await;
        let conn = db.lock().unwrap();
        let c = db::concerts::get_concert(&conn, id).unwrap();
        assert!(c.split_at.is_some());
        assert_eq!(c.tracks_present, vec![true, false, true]);
        let stored_after = db::split_timestamps::get_split_timestamps(&conn, id).unwrap();
        assert_eq!(stored_after.auto, Some(stored_before.clone()));
        assert_eq!(stored_after.user, Some(stored_before));
    }

    #[tokio::test]
    async fn errors_when_no_source_and_no_tracks() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Empty Album";
        let set_list = vec!["Song".to_string()];
        // Concert dir does not exist at all — no source, no tracks.

        let (db, id) = seeded_db(album, set_list);
        let registry = Arc::new(JobRegistry::new());
        let outcome = start_split(
            db.clone(),
            registry.clone(),
            config_for(tmp.path().to_path_buf()),
            id,
            SplitMode::Analyze,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, StartOutcome::Spawned));
        wait_until_finished(&registry, id).await;
        let conn = db.lock().unwrap();
        let c = db::concerts::get_concert(&conn, id).unwrap();
        assert!(c.split_at.is_none());
        assert!(!c.split_errors.is_empty(), "split error should be recorded");
        let failed = db::failed_jobs::list_failed_jobs(&conn, 10).unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].name, "split");
        assert!(failed[0].failure_message.contains("Downloaded file"));
    }

    #[tokio::test]
    async fn not_downloaded_is_rejected_without_split_history() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let id = db::seeds::SeedContext::new(&conn)
            .seed_scraped_concert(db::seeds::SeedScrapedConcert {
                source_url: Some("https://npr.org/c/not-downloaded".to_string()),
                title: Some("Not Downloaded".to_string()),
                concert_date: None,
                artist: Some("Test Artist".to_string()),
                album: Some("Not Downloaded".to_string()),
                set_list: Some(vec!["Song".to_string()]),
            })
            .unwrap()
            .id;
        let db = Arc::new(Mutex::new(conn));
        let registry = Arc::new(JobRegistry::new());

        let outcome = start_split(
            db.clone(),
            registry.clone(),
            config_for(tmp.path().to_path_buf()),
            id,
            SplitMode::Analyze,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, StartOutcome::NotDownloaded));
        assert!(!registry.is_running(&JobKey {
            concert_id: id,
            kind: JobKind::Split
        }));
        let conn = db.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, id).unwrap();
        assert!(concert.split_started_at.is_none());
        assert!(concert.split_errors.is_empty());
        assert!(db::failed_jobs::list_failed_jobs(&conn, 10)
            .unwrap()
            .is_empty());
        assert!(!crate::events::list_for_concert(&conn, id)
            .iter()
            .any(|event| matches!(event.event.as_str(), "split_started" | "split_error")));
    }

    #[tokio::test]
    async fn duplicate_split_request_accepts_only_one_job_run() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Duplicate Split";
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        fs::write(cd.join("Duplicate Split.mp4"), b"video").unwrap();
        let (db, id) = seeded_db(album, vec!["Song".to_string()]);
        let registry = Arc::new(JobRegistry::new());
        let config = config_for(tmp.path().to_path_buf());

        let first = start_split(
            db.clone(),
            registry.clone(),
            config.clone(),
            id,
            SplitMode::Analyze,
        )
        .await
        .unwrap();
        let second = start_split(db, registry.clone(), config, id, SplitMode::Analyze)
            .await
            .unwrap();

        assert!(matches!(first, StartOutcome::Spawned));
        assert!(matches!(second, StartOutcome::AlreadyRunning));
        // Stop the background sleeper so the test exits promptly.
        registry.cancel_all();
    }

    /// End-to-end coverage of the in-process library adapter (#141), driven
    /// through the same `start_split`/Job Run engine production uses — not
    /// just `jobs::split_library`'s own pure-function unit tests. Uses
    /// UserTimestamps (not Analyze) so no OCR backend/models are needed:
    /// `concert_split::run`'s Detect (OCR) phase only runs when timestamps are
    /// absent. A real `ffmpeg -f lavfi` fixture is still required — the
    /// library's Inspect phase ffprobes the input file unconditionally, and
    /// the Cut phase invokes real ffmpeg. See
    /// `live-set-song-splitter/src/concert_split.rs`'s own `fixture` test
    /// helper, which this mirrors.
    #[tokio::test]
    async fn library_backend_splits_user_timestamps_end_to_end() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Library Backend E2E";
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        let media_path = cd.join(format!("{}.mp4", album));
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=6:size=320x240:rate=25",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=6",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
                media_path.to_str().unwrap(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("failed to spawn ffmpeg to build the test fixture");
        assert!(status.success(), "ffmpeg failed to build the test fixture");

        let set_list = vec!["Song".to_string()];
        let (db, id) = seeded_db(album, set_list.clone());
        let payload = vec![TimestampPayloadSong {
            title: "Song".to_string(),
            start_time: 0.0,
            end_time: 4.0,
        }];
        let ts = ValidatedTimestamps::validate(&set_list, Some(6.0), &payload).unwrap();

        let registry = Arc::new(JobRegistry::new());
        let config = JobConfig::with_split_backend(
            tmp.path().to_path_buf(),
            Arc::new(|_: &DownloadJob| Command::new("true")),
            SplitBackend::Library,
            Arc::new(|_| Command::new("true")),
        );

        let outcome = start_split(
            db.clone(),
            registry.clone(),
            config,
            id,
            SplitMode::UserTimestamps {
                ts,
                media_duration: 6.0,
            },
        )
        .await
        .unwrap();
        assert!(matches!(outcome, StartOutcome::Spawned));
        wait_until_finished(&registry, id).await;

        let conn = db.lock().unwrap();
        let c = db::concerts::get_concert(&conn, id).unwrap();
        assert!(
            c.split_at.is_some(),
            "split_at should be set; errors={:?}",
            c.split_errors
        );
        assert_eq!(c.tracks_present, vec![true]);
        assert!(
            c.split_errors.is_empty(),
            "no error should be recorded: {:?}",
            c.split_errors
        );
        assert!(cd.join("Song.m4a").exists());
        assert!(cd.join("Song.mp4").exists());
        // UserTimestamps mode never writes concert.json (only Analyze does —
        // see `jobs::split_library::write_concert_json_if_analyze`).
        assert!(!cd.join("concert.json").exists());
    }

    #[tokio::test]
    async fn split_runner_panic_produces_one_failed_terminal_and_allows_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Split Panic";
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        fs::write(cd.join("Split Panic.mp4"), b"video").unwrap();
        let (db, id) = seeded_db(album, vec!["Song".to_string()]);
        let registry = Arc::new(JobRegistry::new());
        let panic_config = JobConfig::from_commands(
            tmp.path().to_path_buf(),
            Arc::new(|_| Command::new("true")),
            Arc::new(|_| panic!("split runner panic")),
            Arc::new(|_| Command::new("true")),
        );

        start_split(
            db.clone(),
            registry.clone(),
            panic_config,
            id,
            SplitMode::Analyze,
        )
        .await
        .unwrap();
        wait_until_finished(&registry, id).await;

        {
            let conn = db.lock().unwrap();
            let failed = db::failed_jobs::list_failed_jobs(&conn, 10).unwrap();
            assert_eq!(failed.len(), 1);
            assert!(failed[0]
                .failure_message
                .contains("panicked during execution"));
        }
        let retry = start_split(
            db,
            registry.clone(),
            config_for(tmp.path().to_path_buf()),
            id,
            SplitMode::Analyze,
        )
        .await
        .unwrap();
        assert!(matches!(retry, StartOutcome::Spawned));
        // Stop the background sleeper so the test exits promptly.
        registry.cancel_all();
    }

    #[tokio::test]
    async fn spawns_splitter_when_source_present() {
        // Source file IS present — must spawn the splitter job, not auto-recover.
        let tmp = tempfile::tempdir().unwrap();
        let album = "Has Source";
        let set_list = vec!["S1".to_string()];
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        fs::write(cd.join("Has Source.mp4"), b"video").unwrap();

        let (db, id) = seeded_db(album, set_list);
        let registry = Arc::new(JobRegistry::new());
        let outcome = start_split(
            db.clone(),
            registry.clone(),
            config_for(tmp.path().to_path_buf()),
            id,
            SplitMode::Analyze,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, StartOutcome::Spawned));
        // The splitter command we configured `sleep 10`, so the job is running.
        assert!(registry.is_running(&JobKey {
            concert_id: id,
            kind: JobKind::Split,
        }));
        // Stop the background sleeper so the test exits promptly.
        registry.cancel_all();
    }

    /// Config whose splitter writes a fake timestamps.json into the output_dir on success.
    fn config_with_fake_analyze(working_dir: PathBuf, set_list: &[String]) -> JobConfig {
        let songs_json: String = set_list
            .iter()
            .enumerate()
            .map(|(i, title)| {
                let start = i as f64 * 100.0;
                let end = start + 90.0;
                format!(
                    r#"{{"title":"{}","start_time":{},"end_time":{},"duration":90.0}}"#,
                    title, start, end
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        // The splitter writes ConcertInfo JSON (with timestamps) to timestamps.json
        let ts_json = format!(
            r#"{{"artist":"A","source":"","show":"","album":"","set_list":[],"musicians":[],"timestamps":[{}]}}"#,
            songs_json
        );
        let script = format!(
            "mkdir -p \"$2\" && printf '{}' > \"$2/timestamps.json\"",
            ts_json.replace('\'', "'\\''")
        );
        JobConfig::from_commands(
            working_dir,
            Arc::new(|_: &DownloadJob| Command::new("true")),
            Arc::new(move |job: &SplitJob| {
                let output_dir = job.output_dir.to_str().unwrap().to_string();
                let mut cmd = Command::new("sh");
                cmd.args(["-c", &script, "--", "", &output_dir]);
                cmd
            }),
            Arc::new(|_| Command::new("true")),
        )
    }

    /// Config whose splitter records the --timestamps-file content and creates track stubs.
    fn config_with_timestamps_check(working_dir: PathBuf, set_list: &[String]) -> JobConfig {
        let touch_cmds: Vec<String> = set_list
            .iter()
            .map(|t| format!("touch \"$2/{}.m4a\"", t))
            .collect();
        let touch = touch_cmds.join("; ");
        let script = format!("mkdir -p \"$2\" && {}", touch);
        JobConfig::from_commands(
            working_dir,
            Arc::new(|_: &DownloadJob| Command::new("true")),
            Arc::new(move |job: &SplitJob| {
                let output_dir = job.output_dir.to_str().unwrap().to_string();
                // Verify --timestamps-file is present in the command args
                let ts_flag = job.timestamps_path.is_some();
                assert!(ts_flag, "user/reset mode must pass --timestamps-file");
                let mut cmd = Command::new("sh");
                cmd.args(["-c", &script, "--", "", &output_dir]);
                cmd
            }),
            Arc::new(|_| Command::new("true")),
        )
    }

    fn reject_event(conn: &Connection, event: &str) {
        conn.execute_batch(&format!(
            "CREATE TRIGGER reject_terminal_event BEFORE INSERT ON events
             WHEN NEW.event = '{event}'
             BEGIN SELECT RAISE(ABORT, 'rejected terminal event'); END;"
        ))
        .unwrap();
    }

    #[tokio::test]
    async fn analyze_mode_stores_auto_timestamps_on_success() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Analyze Album";
        let set_list = vec!["Track One".to_string(), "Track Two".to_string()];
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        fs::write(cd.join("Analyze Album.mp4"), b"video").unwrap();

        let (db, id) = seeded_db(album, set_list.clone());
        let registry = Arc::new(JobRegistry::new());
        let config = config_with_fake_analyze(tmp.path().to_path_buf(), &set_list);
        let outcome = start_split(db.clone(), registry.clone(), config, id, SplitMode::Analyze)
            .await
            .unwrap();
        assert!(matches!(outcome, StartOutcome::Spawned));

        // Wait for job to finish
        for _ in 0..100 {
            if !registry.is_running(&JobKey {
                concert_id: id,
                kind: JobKind::Split,
            }) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let conn = db.lock().unwrap();
        let stored = db::split_timestamps::get_split_timestamps(&conn, id).unwrap();
        assert!(stored.auto.is_some(), "auto timestamps should be stored");
        assert!(stored.user.is_none(), "user timestamps should be cleared");
        let auto = stored.auto.unwrap();
        assert_eq!(auto.len(), 2);
        assert_eq!(auto[0].title, "Track One");
    }

    #[tokio::test]
    async fn split_event_failure_rolls_back_analyze_completion_facts() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Analyze Event Failure";
        let set_list = vec!["Track".to_string()];
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        fs::write(cd.join("Analyze Event Failure.mp4"), b"video").unwrap();
        let (db, id) = seeded_db(album, set_list.clone());
        reject_event(&db.lock().unwrap(), "split");
        let registry = Arc::new(JobRegistry::new());

        start_split(
            db.clone(),
            registry.clone(),
            config_with_fake_analyze(tmp.path().to_path_buf(), &set_list),
            id,
            SplitMode::Analyze,
        )
        .await
        .unwrap();
        wait_until_finished(&registry, id).await;

        let conn = db.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, id).unwrap();
        assert!(concert.split_at.is_none());
        assert!(concert.tracks_present.is_empty());
        assert!(db::split_timestamps::get_split_timestamps(&conn, id)
            .unwrap()
            .auto
            .is_none());
        assert_eq!(
            db::failed_jobs::list_failed_jobs(&conn, 10).unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn user_timestamps_mode_stores_user_column() {
        use crate::split_timestamps::{TimestampPayloadSong, ValidatedTimestamps};

        let tmp = tempfile::tempdir().unwrap();
        let album = "User Album";
        let set_list = vec!["Alpha".to_string(), "Beta".to_string()];
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        fs::write(cd.join("User Album.mp4"), b"video").unwrap();

        let (db, id) = seeded_db(album, set_list.clone());
        let registry = Arc::new(JobRegistry::new());
        let config = config_with_timestamps_check(tmp.path().to_path_buf(), &set_list);

        let payload = vec![
            TimestampPayloadSong {
                title: "Alpha".to_string(),
                start_time: 0.0,
                end_time: 95.0,
            },
            TimestampPayloadSong {
                title: "Beta".to_string(),
                start_time: 100.0,
                end_time: 200.0,
            },
        ];
        let ts = ValidatedTimestamps::validate(&set_list, None, &payload).unwrap();

        let outcome = start_split(
            db.clone(),
            registry.clone(),
            config,
            id,
            SplitMode::UserTimestamps {
                ts,
                media_duration: 200.0,
            },
        )
        .await
        .unwrap();
        assert!(matches!(outcome, StartOutcome::Spawned));

        for _ in 0..100 {
            if !registry.is_running(&JobKey {
                concert_id: id,
                kind: JobKind::Split,
            }) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let conn = db.lock().unwrap();
        let stored = db::split_timestamps::get_split_timestamps(&conn, id).unwrap();
        assert!(stored.user.is_some(), "user timestamps should be stored");
        let user = stored.user.unwrap();
        assert_eq!(user.len(), 2);
        assert_eq!(user[0].title, "Alpha");
        assert_eq!(user[0].start_time, 0.0);
        assert_eq!(user[0].end_time, 95.0);
    }

    #[tokio::test]
    async fn user_timestamp_event_failure_rolls_back_user_completion_facts() {
        use crate::split_timestamps::{TimestampPayloadSong, ValidatedTimestamps};

        let tmp = tempfile::tempdir().unwrap();
        let album = "User Event Failure";
        let set_list = vec!["Track".to_string()];
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        fs::write(cd.join("User Event Failure.mp4"), b"video").unwrap();
        let (db, id) = seeded_db(album, set_list.clone());
        reject_event(&db.lock().unwrap(), "split_timestamps_user");
        let timestamps = ValidatedTimestamps::validate(
            &set_list,
            None,
            &[TimestampPayloadSong {
                title: "Track".to_string(),
                start_time: 0.0,
                end_time: 10.0,
            }],
        )
        .unwrap();
        let registry = Arc::new(JobRegistry::new());

        start_split(
            db.clone(),
            registry.clone(),
            config_with_timestamps_check(tmp.path().to_path_buf(), &set_list),
            id,
            SplitMode::UserTimestamps {
                ts: timestamps,
                media_duration: 10.0,
            },
        )
        .await
        .unwrap();
        wait_until_finished(&registry, id).await;

        let conn = db.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, id).unwrap();
        assert!(concert.split_at.is_none());
        assert!(concert.tracks_present.is_empty());
        assert!(db::split_timestamps::get_split_timestamps(&conn, id)
            .unwrap()
            .user
            .is_none());
        assert_eq!(
            db::failed_jobs::list_failed_jobs(&conn, 10).unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn reset_event_failure_preserves_user_timestamps() {
        use crate::split_timestamps::{TimestampPayloadSong, ValidatedTimestamps};

        let tmp = tempfile::tempdir().unwrap();
        let album = "Reset Event Failure";
        let set_list = vec!["Track".to_string()];
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        fs::write(cd.join("Reset Event Failure.mp4"), b"video").unwrap();
        let (db, id) = seeded_db(album, set_list.clone());
        let timestamps = ValidatedTimestamps::validate(
            &set_list,
            None,
            &[TimestampPayloadSong {
                title: "Track".to_string(),
                start_time: 0.0,
                end_time: 10.0,
            }],
        )
        .unwrap();
        {
            let conn = db.lock().unwrap();
            db::split_timestamps::set_user_split_timestamps(&conn, id, timestamps.songs()).unwrap();
            reject_event(&conn, "split_timestamps_reset");
        }
        let registry = Arc::new(JobRegistry::new());

        start_split(
            db.clone(),
            registry.clone(),
            config_with_timestamps_check(tmp.path().to_path_buf(), &set_list),
            id,
            SplitMode::ResetToAuto(timestamps),
        )
        .await
        .unwrap();
        wait_until_finished(&registry, id).await;

        let conn = db.lock().unwrap();
        assert!(db::concerts::get_concert(&conn, id)
            .unwrap()
            .split_at
            .is_none());
        assert!(db::split_timestamps::get_split_timestamps(&conn, id)
            .unwrap()
            .user
            .is_some());
        assert_eq!(
            db::failed_jobs::list_failed_jobs(&conn, 10).unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn user_mode_does_not_auto_recover_when_source_missing() {
        use crate::split_timestamps::{TimestampPayloadSong, ValidatedTimestamps};

        let tmp = tempfile::tempdir().unwrap();
        let album = "No Source Album";
        let set_list = vec!["Song A".to_string()];
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        // Track exists on disk but NO source file
        fs::write(cd.join("Song A.m4a"), b"audio").unwrap();

        let (db, id) = seeded_db(album, set_list.clone());
        let registry = Arc::new(JobRegistry::new());
        let config = config_for(tmp.path().to_path_buf());

        let payload = vec![TimestampPayloadSong {
            title: "Song A".to_string(),
            start_time: 0.0,
            end_time: 90.0,
        }];
        let ts = ValidatedTimestamps::validate(&set_list, None, &payload).unwrap();

        let outcome = start_split(
            db.clone(),
            registry.clone(),
            config,
            id,
            SplitMode::UserTimestamps {
                ts,
                media_duration: 90.0,
            },
        )
        .await
        .unwrap();

        assert!(matches!(outcome, StartOutcome::Spawned));
        wait_until_finished(&registry, id).await;
        let conn = db.lock().unwrap();
        let failed = db::failed_jobs::list_failed_jobs(&conn, 10).unwrap();
        assert_eq!(failed.len(), 1);
        assert!(failed[0].failure_message.contains("Downloaded file"));
    }

    /// Config whose splitter always exits non-zero, recording a split failure.
    fn config_with_failing_split(working_dir: PathBuf) -> JobConfig {
        JobConfig::from_commands(
            working_dir,
            Arc::new(|_: &DownloadJob| Command::new("true")),
            Arc::new(|_| {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", "exit 1"]);
                cmd
            }),
            Arc::new(|_| Command::new("true")),
        )
    }

    /// Simulate a concert that was already split (split_at IS NOT NULL) and re-split
    /// it successfully. Confirms the outcome is detected as success via split_errors
    /// count (since split_at stays set regardless, the status slug alone is unreliable).
    #[tokio::test]
    async fn resplit_success_reports_ok_via_error_count() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Resplit Success";
        let set_list = vec!["Track One".to_string(), "Track Two".to_string()];
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        fs::write(cd.join("Resplit Success.mp4"), b"video").unwrap();

        let (db, id) = seeded_db(album, set_list.clone());
        // Simulate a prior successful split
        {
            let conn = db.lock().unwrap();
            db::lifecycle::try_mark_split_started(&conn, id).unwrap();
            db::lifecycle::mark_split_succeeded(&conn, id).unwrap();
        }
        let initial_errors = {
            let conn = db.lock().unwrap();
            db::concerts::get_concert(&conn, id)
                .unwrap()
                .split_errors
                .len()
        };

        let registry = Arc::new(JobRegistry::new());
        let config = config_with_fake_analyze(tmp.path().to_path_buf(), &set_list);
        let outcome = start_split(db.clone(), registry.clone(), config, id, SplitMode::Analyze)
            .await
            .unwrap();
        assert!(matches!(outcome, StartOutcome::Spawned));

        let key = JobKey {
            concert_id: id,
            kind: JobKind::Split,
        };
        for _ in 0..100 {
            if !registry.is_running(&key) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let post_errors = {
            let conn = db.lock().unwrap();
            db::concerts::get_concert(&conn, id)
                .unwrap()
                .split_errors
                .len()
        };
        assert_eq!(
            post_errors, initial_errors,
            "successful re-split must not add errors"
        );
    }

    /// Simulate a concert that was already split and re-split it with a failing
    /// splitter. Confirms the outcome is detected as failure via split_errors count
    /// (the old split_at stays set, so status slug alone would misreport this).
    #[tokio::test]
    async fn resplit_failure_detected_via_error_count() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Resplit Failure";
        let set_list = vec!["Song".to_string()];
        let cd = concert_dir(tmp.path(), album);
        fs::create_dir_all(&cd).unwrap();
        fs::write(cd.join("Resplit Failure.mp4"), b"video").unwrap();

        let (db, id) = seeded_db(album, set_list.clone());
        // Simulate a prior successful split
        {
            let conn = db.lock().unwrap();
            db::lifecycle::try_mark_split_started(&conn, id).unwrap();
            db::lifecycle::mark_split_succeeded(&conn, id).unwrap();
        }
        let initial_errors = {
            let conn = db.lock().unwrap();
            db::concerts::get_concert(&conn, id)
                .unwrap()
                .split_errors
                .len()
        };

        let registry = Arc::new(JobRegistry::new());
        let config = config_with_failing_split(tmp.path().to_path_buf());
        let outcome = start_split(db.clone(), registry.clone(), config, id, SplitMode::Analyze)
            .await
            .unwrap();
        assert!(matches!(outcome, StartOutcome::Spawned));

        let key = JobKey {
            concert_id: id,
            kind: JobKind::Split,
        };
        for _ in 0..100 {
            if !registry.is_running(&key) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let post_errors = {
            let conn = db.lock().unwrap();
            db::concerts::get_concert(&conn, id)
                .unwrap()
                .split_errors
                .len()
        };
        assert!(
            post_errors > initial_errors,
            "failing re-split must append to split_errors (initial={}, post={})",
            initial_errors,
            post_errors
        );
    }
}
