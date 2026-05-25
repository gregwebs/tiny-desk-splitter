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
    let handle = tokio::task::spawn(run_download(db.clone(), config, job));
    registry.insert(key, handle);

    Ok(StartOutcome::Spawned)
}

async fn run_download(db: Arc<Mutex<Connection>>, config: JobConfig, job: DownloadJob) {
    let concert_id = job.concert_id;
    let cmd = (config.download_cmd)(&job);

    let log_dir = config.log_dir();
    let temp_file = match std::fs::create_dir_all(&log_dir)
        .and_then(|_| NamedTempFile::new_in(&log_dir).map_err(Into::into))
    {
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
            let conn = db.lock().unwrap();
            let _ = db::mark_download_succeeded(&conn, concert_id);
        }
        Ok((status, stderr_tail)) => {
            let error = format!("exit {:?}: {}", status.code(), stderr_tail.trim());
            tracing::warn!("download failed for concert {}: {}", concert_id, error);
            let conn = db.lock().unwrap();
            let _ = db::mark_download_failed(&conn, concert_id, &error);
            persist_job_log(&conn, concert_id, "download", &error, temp_file, &log_dir);
        }
        Err(e) => {
            let error = format!("spawn error: {}", e);
            tracing::warn!("download failed for concert {}: {}", concert_id, error);
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
        }
    }

    fn seeded_db() -> Arc<Mutex<Connection>> {
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
                set_list: vec![],
                musicians: vec![],
            },
        )
        .unwrap();
        Arc::new(Mutex::new(conn))
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
