use anyhow::Result;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::db;
use crate::jobs::run::{self, Admission, JobCancellation, JobRequest};
use crate::jobs::{
    download_job_from_concert, DownloadJob, JobConfig, JobKey, JobKind, JobRegistry, JobRunFuture,
    JobStepOutcome,
};

pub enum StartOutcome {
    Spawned,
    AlreadyRunning,
}

/// Start a download job for the given concert. Returns Spawned or
/// AlreadyRunning. Goes through the Job Run engine (`jobs::run`): race-safe
/// admission, exactly one terminal outcome, and Failed Job history on any
/// unsuccessful outcome including cancellation. See `docs/jobs.md`.
pub async fn start_download(
    db: Arc<Mutex<Connection>>,
    registry: Arc<JobRegistry>,
    config: JobConfig,
    concert_id: i64,
) -> Result<StartOutcome> {
    let request = DownloadRequest::new(concert_id, config);
    match run::submit(db, registry, request).await? {
        Admission::Accepted => Ok(StartOutcome::Spawned),
        Admission::AlreadyRunning => Ok(StartOutcome::AlreadyRunning),
    }
}

/// The download [`JobRequest`]. `Setup` is the identity of `Input`
/// (`DownloadJob`) — download has no separate post-acceptance preparation
/// step; split uses `Setup` for temp files and output paths.
///
/// `pub(crate)` so `crate::lifecycle::cancel_job` can build one to route a
/// user-initiated cancellation through [`run::cancel`].
pub(crate) struct DownloadRequest {
    concert_id: i64,
    config: JobConfig,
}

impl DownloadRequest {
    pub(crate) fn new(concert_id: i64, config: JobConfig) -> Self {
        DownloadRequest { concert_id, config }
    }
}

pub(crate) struct DownloadCancellation {
    concert_id: i64,
}

impl DownloadCancellation {
    pub(crate) fn new(concert_id: i64) -> Self {
        Self { concert_id }
    }
}

impl JobCancellation for DownloadCancellation {
    fn key(&self) -> JobKey {
        JobKey {
            concert_id: self.concert_id,
            kind: JobKind::Download,
        }
    }

    fn job_name(&self) -> &'static str {
        "download"
    }

    fn record_failure(&self, conn: &Connection, error: &str) -> Result<()> {
        db::lifecycle::mark_download_failed(conn, self.concert_id, error)
    }

    fn has_stale_in_progress(&self, conn: &Connection) -> Result<bool> {
        Ok(conn.query_row(
            "SELECT download_started_at IS NOT NULL FROM concerts WHERE id = ?1",
            [self.concert_id],
            |row| row.get(0),
        )?)
    }
}

impl JobCancellation for DownloadRequest {
    fn key(&self) -> JobKey {
        JobKey {
            concert_id: self.concert_id,
            kind: JobKind::Download,
        }
    }

    fn job_name(&self) -> &'static str {
        "download"
    }

    fn record_failure(&self, conn: &Connection, error: &str) -> Result<()> {
        db::lifecycle::mark_download_failed(conn, self.concert_id, error)
    }

    fn has_stale_in_progress(&self, conn: &Connection) -> Result<bool> {
        Ok(conn.query_row(
            "SELECT download_started_at IS NOT NULL FROM concerts WHERE id = ?1",
            [self.concert_id],
            |row| row.get(0),
        )?)
    }
}

impl JobRequest for DownloadRequest {
    type Input = DownloadJob;
    type Setup = DownloadJob;
    /// The downloaded file's extension, resolved from disk after `execute`
    /// succeeds.
    type Facts = String;

    fn validate(&self, conn: &Connection) -> Result<DownloadJob> {
        let concert = db::concerts::get_concert(conn, self.concert_id)?;
        download_job_from_concert(&concert, &self.config.working_dir)
    }

    fn try_mark_started(&self, conn: &Connection) -> Result<bool> {
        db::lifecycle::try_mark_download_started(conn, self.concert_id)
    }

    fn setup(&self, input: DownloadJob) -> Result<DownloadJob> {
        Ok(input)
    }

    fn execute<'a>(
        &'a self,
        setup: &'a DownloadJob,
        log_file: Option<&'a Path>,
    ) -> JobRunFuture<'a, JobStepOutcome> {
        Box::pin(self.config.run_download(setup, log_file))
    }

    fn gather_success_facts(&self, setup: &DownloadJob) -> Result<String> {
        let ext =
            crate::concert_media::find_downloaded_file(&self.config.working_dir, &setup.album)
                .and_then(|p| {
                    p.extension()
                        .and_then(|e| e.to_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| "mp4".to_string());
        Ok(ext)
    }

    fn commit_success(&self, conn: &Connection, extension: String) -> Result<()> {
        db::lifecycle::mark_download_succeeded(conn, self.concert_id, &extension)
    }

    fn log_dir(&self) -> Option<PathBuf> {
        Some(self.config.log_dir())
    }

    fn spawn_dependents(&self, db: Arc<Mutex<Connection>>, registry: Arc<JobRegistry>) {
        crate::jobs::spawn_dependents(db, registry, self.config.clone(), &self.key());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::jobs::{JobConfig, JobRegistry};
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::process::Command;

    fn config_success() -> JobConfig {
        JobConfig::from_commands(
            PathBuf::from("/tmp"),
            Arc::new(|_job: &DownloadJob| Command::new("true")),
            Arc::new(|_| unreachable!()),
            Arc::new(|_| Command::new("true")),
        )
    }

    fn config_failure() -> JobConfig {
        JobConfig::from_commands(
            PathBuf::from("/tmp"),
            Arc::new(|_| {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", "echo boom >&2; exit 7"]);
                cmd
            }),
            Arc::new(|_| unreachable!()),
            Arc::new(|_| Command::new("true")),
        )
    }

    fn seeded_db_with_set_list(set_list: Vec<String>) -> (Arc<Mutex<Connection>>, i64) {
        let conn = db::connection::open_in_memory().unwrap();
        let id = db::seeds::SeedContext::new(&conn)
            .seed_scraped_concert(db::seeds::SeedScrapedConcert {
                source_url: Some("https://npr.org/test/dl".to_string()),
                title: Some("Test Concert".to_string()),
                concert_date: None,
                artist: Some("Test Artist".to_string()),
                album: Some("Test Album".to_string()),
                set_list: Some(set_list),
            })
            .unwrap()
            .id;
        (Arc::new(Mutex::new(conn)), id)
    }

    fn seeded_db() -> (Arc<Mutex<Connection>>, i64) {
        seeded_db_with_set_list(vec![])
    }

    /// Poll `check` every 50ms until it returns true or ~5s elapse.
    async fn wait_for(
        db: &Arc<Mutex<Connection>>,
        id: i64,
        check: impl Fn(&crate::model::Concert) -> bool,
    ) {
        for _ in 0..100 {
            {
                let conn = db.lock().unwrap();
                if let Ok(c) = db::concerts::get_concert(&conn, id) {
                    if check(&c) {
                        return;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn successful_download_marks_downloaded_at() {
        let (db, id) = seeded_db();
        let registry = Arc::new(JobRegistry::new());
        start_download(db.clone(), registry, config_success(), id)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let conn = db.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, id).unwrap();
        assert!(concert.downloaded_at.is_some());
        assert!(concert.download_errors.is_empty());
    }

    #[tokio::test]
    async fn failed_download_records_error() {
        let (db, id) = seeded_db();
        let registry = Arc::new(JobRegistry::new());
        start_download(db.clone(), registry, config_failure(), id)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let conn = db.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, id).unwrap();
        assert!(concert.downloaded_at.is_none());
        assert!(!concert.download_errors.is_empty());
    }

    #[tokio::test]
    async fn download_success_starts_dependent_split() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, id) = seeded_db_with_set_list(vec!["Song A".to_string(), "Song B".to_string()]);
        // The no-op download command creates no file, so place the "downloaded"
        // source file where find_downloaded_file expects it.
        let cd = crate::model::concert_dir(tmp.path(), "Test Album");
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(cd.join("Test Album.mp4"), b"video").unwrap();

        let config = JobConfig::from_commands(
            tmp.path().to_path_buf(),
            Arc::new(|_| Command::new("true")),
            // Real command (no mock): the "splitter" creates the per-song
            // files and required analysis timestamps.
            Arc::new(|job: &crate::jobs::SplitJob| {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(format!(
                    "touch '{0}/Song A.m4a' '{0}/Song B.m4a'; \
                     printf '%s' '{1}' > '{0}/timestamps.json'",
                    job.output_dir.display(),
                    r#"{"artist":"A","source":"","show":"","album":"","set_list":[],"musicians":[],"timestamps":[{"title":"Song A","start_time":0.0,"end_time":10.0,"duration":10.0},{"title":"Song B","start_time":10.0,"end_time":20.0,"duration":10.0}]}"#
                ));
                cmd
            }),
            Arc::new(|_| Command::new("true")),
        );

        let registry = Arc::new(JobRegistry::new());
        let download_key = JobKey {
            concert_id: id,
            kind: JobKind::Download,
        };
        let split_key = JobKey {
            concert_id: id,
            kind: JobKind::Split,
        };
        registry.add_dependent(download_key, split_key);

        start_download(db.clone(), registry.clone(), config, id)
            .await
            .unwrap();
        wait_for(&db, id, |c| c.split_at.is_some()).await;

        let conn = db.lock().unwrap();
        let c = db::concerts::get_concert(&conn, id).unwrap();
        assert!(c.downloaded_at.is_some(), "download should have succeeded");
        assert!(c.split_at.is_some(), "dependent split should have run");
        assert_eq!(c.tracks_present, vec![true, true]);
        assert!(c.split_errors.is_empty());
    }

    #[tokio::test]
    async fn download_failure_drops_dependent_split() {
        let (db, id) = seeded_db_with_set_list(vec!["Song A".to_string()]);
        let registry = Arc::new(JobRegistry::new());
        let download_key = JobKey {
            concert_id: id,
            kind: JobKind::Download,
        };
        let split_key = JobKey {
            concert_id: id,
            kind: JobKind::Split,
        };
        registry.add_dependent(download_key.clone(), split_key.clone());

        start_download(db.clone(), registry.clone(), config_failure(), id)
            .await
            .unwrap();
        wait_for(&db, id, |c| !c.download_errors.is_empty()).await;

        let conn = db.lock().unwrap();
        let c = db::concerts::get_concert(&conn, id).unwrap();
        assert!(!c.download_errors.is_empty());
        assert!(c.split_started_at.is_none(), "split must never start");
        assert!(c.split_at.is_none());
        assert!(
            !registry.has_dependent(&download_key, &split_key),
            "queued split should be dropped on download failure"
        );
    }

    #[tokio::test]
    async fn released_split_intent_revalidates_current_download_state() {
        let (db, id) = seeded_db_with_set_list(vec!["Song A".to_string()]);
        let registry = Arc::new(JobRegistry::new());
        let download_key = JobKey {
            concert_id: id,
            kind: JobKind::Download,
        };
        let split_key = JobKey {
            concert_id: id,
            kind: JobKind::Split,
        };
        registry.add_dependent(download_key.clone(), split_key.clone());
        {
            let conn = db.lock().unwrap();
            db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();
            db::lifecycle::clear_download_state(&conn, id).unwrap();
        }

        crate::jobs::spawn_dependents(
            db.clone(),
            registry.clone(),
            config_success(),
            &download_key,
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let conn = db.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, id).unwrap();
        assert!(concert.split_started_at.is_none());
        assert!(concert.split_errors.is_empty());
        assert!(db::failed_jobs::list_failed_jobs(&conn, 10)
            .unwrap()
            .is_empty());
        assert!(!registry.has_dependent(&download_key, &split_key));
    }

    #[tokio::test]
    async fn released_split_intent_rejects_a_deleted_concert() {
        let (db, id) = seeded_db_with_set_list(vec!["Song A".to_string()]);
        let registry = Arc::new(JobRegistry::new());
        let download_key = JobKey {
            concert_id: id,
            kind: JobKind::Download,
        };
        registry.add_dependent(
            download_key.clone(),
            JobKey {
                concert_id: id,
                kind: JobKind::Split,
            },
        );
        db.lock()
            .unwrap()
            .execute("DELETE FROM concerts WHERE id = ?1", [id])
            .unwrap();

        crate::jobs::spawn_dependents(
            db.clone(),
            registry.clone(),
            config_success(),
            &download_key,
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(db::failed_jobs::list_failed_jobs(&db.lock().unwrap(), 10)
            .unwrap()
            .is_empty());
        assert!(!registry.is_running(&JobKey {
            concert_id: id,
            kind: JobKind::Split
        }));
    }

    #[tokio::test]
    async fn released_split_intent_builds_input_from_changed_set_list() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, id) = seeded_db_with_set_list(vec!["Old Song".to_string()]);
        let album = "Test Album";
        let output_dir = crate::model::concert_dir(tmp.path(), album);
        std::fs::create_dir_all(&output_dir).unwrap();
        std::fs::write(output_dir.join("Test Album.mp4"), b"video").unwrap();
        {
            let conn = db.lock().unwrap();
            db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();
            db::concerts::update_metadata(
                &conn,
                id,
                &db::concerts::MetadataUpdate {
                    artist: "Changed Artist".to_string(),
                    album: album.to_string(),
                    description: None,
                    set_list: vec!["New Song".to_string()],
                    musicians: vec![],
                },
            )
            .unwrap();
        }
        let config = JobConfig::from_commands(
            tmp.path().to_path_buf(),
            Arc::new(|_| Command::new("true")),
            Arc::new(|job: &crate::jobs::SplitJob| {
                let mut command = Command::new("sh");
                command.arg("-c").arg(format!(
                    "grep -q 'New Song' '{}'; touch '{}/New Song.m4a'; \
                     printf '%s' '{}' > '{}/timestamps.json'",
                    job.json_path.display(),
                    job.output_dir.display(),
                    r#"{"artist":"A","source":"","show":"","album":"","set_list":[],"musicians":[],"timestamps":[{"title":"New Song","start_time":0.0,"end_time":10.0,"duration":10.0}]}"#,
                    job.output_dir.display()
                ));
                command
            }),
            Arc::new(|_| Command::new("true")),
        );
        let registry = Arc::new(JobRegistry::new());
        let download_key = JobKey {
            concert_id: id,
            kind: JobKind::Download,
        };
        registry.add_dependent(
            download_key.clone(),
            JobKey {
                concert_id: id,
                kind: JobKind::Split,
            },
        );

        crate::jobs::spawn_dependents(db.clone(), registry, config, &download_key);
        wait_for(&db, id, |concert| concert.split_at.is_some()).await;

        let concert = db::concerts::get_concert(&db.lock().unwrap(), id).unwrap();
        assert_eq!(concert.set_list, vec!["New Song"]);
        assert_eq!(concert.tracks_present, vec![true]);
    }

    #[tokio::test]
    async fn duplicate_start_returns_already_running() {
        let (db, id) = seeded_db();
        let registry = Arc::new(JobRegistry::new());
        let config = JobConfig::from_commands(
            PathBuf::from("/tmp"),
            Arc::new(|_| {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", "sleep 10"]);
                cmd
            }),
            Arc::new(|_| unreachable!()),
            Arc::new(|_| Command::new("true")),
        );
        let r1 = start_download(db.clone(), registry.clone(), config.clone(), id)
            .await
            .unwrap();
        assert!(matches!(r1, StartOutcome::Spawned));
        let r2 = start_download(db.clone(), registry.clone(), config, id)
            .await
            .unwrap();
        assert!(matches!(r2, StartOutcome::AlreadyRunning));
    }
}
