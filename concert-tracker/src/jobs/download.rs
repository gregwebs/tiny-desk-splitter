use anyhow::Result;
use rusqlite::Connection;
use std::sync::{Arc, Mutex};
use tempfile::NamedTempFile;

use crate::db;
use crate::jobs::{
    download_job_from_concert, persist_job_log, run_with_logging, DownloadJob, JobConfig, JobKey,
    JobKind, JobRegistry,
};

pub enum StartOutcome {
    Spawned,
    AlreadyRunning,
}

/// Start a download job for the given concert. Returns Spawned or AlreadyRunning.
pub async fn start_download(
    db: Arc<Mutex<Connection>>,
    registry: Arc<JobRegistry>,
    config: JobConfig,
    concert_id: i64,
) -> Result<StartOutcome> {
    let key = JobKey {
        concert_id,
        kind: JobKind::Download,
    };
    if registry.is_running(&key) {
        return Ok(StartOutcome::AlreadyRunning);
    }

    {
        let conn = db.lock().unwrap();
        if !db::try_mark_download_started(&conn, concert_id)? {
            tracing::info!("download already running for concert {}", concert_id);
            return Ok(StartOutcome::AlreadyRunning);
        }
    }

    let (job, title) = {
        let conn = db.lock().unwrap();
        let concert = db::get_concert(&conn, concert_id)?;
        let title = concert.title.clone();
        let job = download_job_from_concert(&concert, &config.working_dir)?;
        (job, title)
    };

    tracing::info!("download started for concert {} ({})", concert_id, title);
    let handle = tokio::task::spawn(run_download(db.clone(), registry.clone(), config, job));
    registry.insert(key, handle);

    Ok(StartOutcome::Spawned)
}

async fn run_download(
    db: Arc<Mutex<Connection>>,
    registry: Arc<JobRegistry>,
    config: JobConfig,
    job: DownloadJob,
) {
    let concert_id = job.concert_id;
    let key = JobKey {
        concert_id,
        kind: JobKind::Download,
    };
    let cmd = (config.download_cmd)(&job);

    let log_dir = config.log_dir();
    let temp_file =
        match std::fs::create_dir_all(&log_dir).and_then(|_| NamedTempFile::new_in(&log_dir)) {
            Ok(f) => Some(f),
            Err(e) => {
                tracing::warn!("failed to create temp log file: {}", e);
                None
            }
        };
    let temp_path = temp_file.as_ref().map(|f| f.path().to_path_buf());

    match run_with_logging(cmd, "download", concert_id, temp_path.as_deref()).await {
        Ok((status, _)) if status.success() => {
            tracing::info!("download completed for concert {}", concert_id);
            drop(temp_file);
            let ext = crate::jobs::find_downloaded_file(&config.working_dir, &job.album)
                .and_then(|p| {
                    p.extension()
                        .and_then(|e| e.to_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| "mp4".to_string());
            {
                let conn = db.lock().unwrap();
                let _ = db::mark_download_succeeded(&conn, concert_id, &ext);
            }
            crate::jobs::spawn_dependents(db, registry, config, &key);
        }
        Ok((status, stderr_tail)) => {
            let error = format!("exit {:?}: {}", status.code(), stderr_tail.trim());
            tracing::warn!("download failed for concert {}: {}", concert_id, error);
            registry.drop_dependency_edges(&key);
            let conn = db.lock().unwrap();
            let _ = db::mark_download_failed(&conn, concert_id, &error);
            persist_job_log(&conn, concert_id, "download", &error, temp_file, &log_dir);
        }
        Err(e) => {
            let hint = if e.kind() == std::io::ErrorKind::NotFound {
                ". Is yt-dlp installed? See: https://github.com/yt-dlp/yt-dlp#installation"
            } else {
                ""
            };
            let error = format!("spawn error: {}{}", e, hint);
            tracing::warn!("download failed for concert {}: {}", concert_id, error);
            registry.drop_dependency_edges(&key);
            let conn = db.lock().unwrap();
            let _ = db::mark_download_failed(&conn, concert_id, &error);
            persist_job_log(&conn, concert_id, "download", &error, temp_file, &log_dir);
        }
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
        JobConfig {
            working_dir: PathBuf::from("/tmp"),
            download_cmd: Arc::new(|_job: &DownloadJob| Command::new("true")),
            split_cmd: Arc::new(|_| unreachable!()),
            open_cmd: Arc::new(|_| Command::new("true")),
        }
    }

    fn config_failure() -> JobConfig {
        JobConfig {
            working_dir: PathBuf::from("/tmp"),
            download_cmd: Arc::new(|_| {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", "echo boom >&2; exit 7"]);
                cmd
            }),
            split_cmd: Arc::new(|_| unreachable!()),
            open_cmd: Arc::new(|_| Command::new("true")),
        }
    }

    fn seeded_db_with_set_list(set_list: Vec<String>) -> Arc<Mutex<Connection>> {
        let conn = db::open_in_memory().unwrap();
        db::upsert_listing(
            &conn,
            &db::NewListing {
                source_url: "https://npr.org/test/dl".to_string(),
                title: "Test Concert".to_string(),
                concert_date: None,
                teaser: None,
            },
        )
        .unwrap();
        db::update_metadata(
            &conn,
            1,
            &db::MetadataUpdate {
                artist: "Test Artist".to_string(),
                album: "Test Album".to_string(),
                description: None,
                set_list,
                musicians: vec![],
            },
        )
        .unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn seeded_db() -> Arc<Mutex<Connection>> {
        seeded_db_with_set_list(vec![])
    }

    /// Poll `check` every 50ms until it returns true or ~5s elapse.
    async fn wait_for(db: &Arc<Mutex<Connection>>, check: impl Fn(&crate::model::Concert) -> bool) {
        for _ in 0..100 {
            {
                let conn = db.lock().unwrap();
                if let Ok(c) = db::get_concert(&conn, 1) {
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
        let db = seeded_db();
        let registry = Arc::new(JobRegistry::new());
        start_download(db.clone(), registry, config_success(), 1)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let conn = db.lock().unwrap();
        let concert = db::get_concert(&conn, 1).unwrap();
        assert!(concert.downloaded_at.is_some());
        assert!(concert.download_errors.is_empty());
    }

    #[tokio::test]
    async fn failed_download_records_error() {
        let db = seeded_db();
        let registry = Arc::new(JobRegistry::new());
        start_download(db.clone(), registry, config_failure(), 1)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let conn = db.lock().unwrap();
        let concert = db::get_concert(&conn, 1).unwrap();
        assert!(concert.downloaded_at.is_none());
        assert!(!concert.download_errors.is_empty());
    }

    #[tokio::test]
    async fn download_success_starts_dependent_split() {
        let tmp = tempfile::tempdir().unwrap();
        let db = seeded_db_with_set_list(vec!["Song A".to_string(), "Song B".to_string()]);
        // The no-op download command creates no file, so place the "downloaded"
        // source file where find_downloaded_file expects it.
        let cd = crate::model::concert_dir(tmp.path(), "Test Album");
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(cd.join("Test Album.mp4"), b"video").unwrap();

        let config = JobConfig {
            working_dir: tmp.path().to_path_buf(),
            download_cmd: Arc::new(|_| Command::new("true")),
            // Real command (no mock): the "splitter" creates the per-song
            // files the rescan expects.
            split_cmd: Arc::new(|job: &crate::jobs::SplitJob| {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(format!(
                    "touch '{0}/Song A.m4a' '{0}/Song B.m4a'",
                    job.output_dir.display()
                ));
                cmd
            }),
            open_cmd: Arc::new(|_| Command::new("true")),
        };

        let registry = Arc::new(JobRegistry::new());
        let download_key = JobKey {
            concert_id: 1,
            kind: JobKind::Download,
        };
        let split_key = JobKey {
            concert_id: 1,
            kind: JobKind::Split,
        };
        registry.add_dependent(download_key, split_key);

        start_download(db.clone(), registry.clone(), config, 1)
            .await
            .unwrap();
        wait_for(&db, |c| c.split_at.is_some()).await;

        let conn = db.lock().unwrap();
        let c = db::get_concert(&conn, 1).unwrap();
        assert!(c.downloaded_at.is_some(), "download should have succeeded");
        assert!(c.split_at.is_some(), "dependent split should have run");
        assert_eq!(c.tracks_present, vec![true, true]);
        assert!(c.split_errors.is_empty());
    }

    #[tokio::test]
    async fn download_failure_drops_dependent_split() {
        let db = seeded_db_with_set_list(vec!["Song A".to_string()]);
        let registry = Arc::new(JobRegistry::new());
        let download_key = JobKey {
            concert_id: 1,
            kind: JobKind::Download,
        };
        let split_key = JobKey {
            concert_id: 1,
            kind: JobKind::Split,
        };
        registry.add_dependent(download_key.clone(), split_key.clone());

        start_download(db.clone(), registry.clone(), config_failure(), 1)
            .await
            .unwrap();
        wait_for(&db, |c| !c.download_errors.is_empty()).await;

        let conn = db.lock().unwrap();
        let c = db::get_concert(&conn, 1).unwrap();
        assert!(!c.download_errors.is_empty());
        assert!(c.split_started_at.is_none(), "split must never start");
        assert!(c.split_at.is_none());
        assert!(
            !registry.has_dependent(&download_key, &split_key),
            "queued split should be dropped on download failure"
        );
    }

    #[tokio::test]
    async fn duplicate_start_returns_already_running() {
        let db = seeded_db();
        let registry = Arc::new(JobRegistry::new());
        let config = JobConfig {
            working_dir: PathBuf::from("/tmp"),
            download_cmd: Arc::new(|_| {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", "sleep 10"]);
                cmd
            }),
            split_cmd: Arc::new(|_| unreachable!()),
            open_cmd: Arc::new(|_| Command::new("true")),
        };
        let r1 = start_download(db.clone(), registry.clone(), config.clone(), 1)
            .await
            .unwrap();
        assert!(matches!(r1, StartOutcome::Spawned));
        let r2 = start_download(db.clone(), registry.clone(), config, 1)
            .await
            .unwrap();
        assert!(matches!(r2, StartOutcome::AlreadyRunning));
    }
}
