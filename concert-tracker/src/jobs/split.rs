use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use std::io::Write;
use std::sync::{Arc, Mutex};
use tempfile::NamedTempFile;

use crate::db;
use crate::jobs::{
    find_downloaded_file, persist_job_log, run_with_logging, JobConfig, JobKey, JobKind,
    JobRegistry, SplitJob,
};
use crate::model::{concert_dir, Concert, Musician};

#[derive(Debug)]
pub enum StartOutcome {
    Spawned,
    AlreadyRunning,
    NotDownloaded,
    /// Source file was missing but split tracks already exist on disk; the
    /// concert's split state was reconciled from the filesystem instead of
    /// running the splitter. Used to recover imported concerts whose
    /// `split_at` was never recorded.
    AlreadySplit,
}

#[derive(Serialize)]
struct SplitterInput {
    artist: String,
    source: String,
    show: String,
    date: Option<String>,
    album: String,
    description: Option<String>,
    set_list: Vec<SplitterSong>,
    musicians: Vec<SplitterMusician>,
}

#[derive(Serialize)]
struct SplitterSong {
    title: String,
}

#[derive(Serialize)]
struct SplitterMusician {
    name: String,
    instruments: Vec<String>,
}

fn write_splitter_input(concert: &Concert) -> Result<NamedTempFile> {
    let set_list: Vec<SplitterSong> = concert
        .set_list
        .iter()
        .map(|title| SplitterSong {
            title: title.clone(),
        })
        .collect();
    let musicians: Vec<SplitterMusician> = concert
        .musicians
        .iter()
        .map(|m: &Musician| SplitterMusician {
            name: m.name.clone(),
            instruments: m.instruments.clone(),
        })
        .collect();

    let input = SplitterInput {
        artist: concert.artist.clone().unwrap_or_default(),
        source: concert.source_url.clone(),
        show: "Tiny Desk Concerts".to_string(),
        date: concert.concert_date.clone(),
        album: concert.album.clone().unwrap_or_default(),
        description: concert.description.clone(),
        set_list,
        musicians,
    };

    let mut file = NamedTempFile::new()?;
    let json = serde_json::to_string(&input)?;
    file.write_all(json.as_bytes())?;
    Ok(file)
}

/// Start a split job for the given concert. Requires downloaded_at IS NOT NULL.
pub async fn start_split(
    db: Arc<Mutex<Connection>>,
    registry: Arc<JobRegistry>,
    config: JobConfig,
    concert_id: i64,
) -> Result<StartOutcome> {
    let key = JobKey {
        concert_id,
        kind: JobKind::Split,
    };
    if registry.is_running(&key) {
        tracing::info!("split already running for concert {}", concert_id);
        return Ok(StartOutcome::AlreadyRunning);
    }

    let (ok, concert) = {
        let conn = db.lock().unwrap();
        let ok = db::try_mark_split_started(&conn, concert_id)?;
        let concert = db::get_concert(&conn, concert_id)?;
        (ok, concert)
    };

    if !ok {
        if concert.downloaded_at.is_none() {
            tracing::info!("split rejected: concert {} not yet downloaded", concert_id);
            return Ok(StartOutcome::NotDownloaded);
        }
        tracing::info!("split already running for concert {}", concert_id);
        return Ok(StartOutcome::AlreadyRunning);
    }

    let title = concert.title.clone();

    enum SetupResult {
        Ready(NamedTempFile, std::path::PathBuf, std::path::PathBuf),
        AlreadySplit,
    }

    let setup = (|| -> Result<SetupResult> {
        let album = concert.album.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "Concert {} has no album, cannot locate input file",
                concert_id
            )
        })?;
        if let Some(input_file) = find_downloaded_file(&config.working_dir, album) {
            let temp_file = write_splitter_input(&concert)?;
            let output_dir = concert_dir(&config.working_dir, album);
            return Ok(SetupResult::Ready(temp_file, input_file, output_dir));
        }
        // Source file missing. If the concert dir already contains split
        // tracks (e.g. imported from an archive that no longer has the
        // original full-concert file), reconcile split state from disk
        // instead of failing. Older imports left split_at NULL even when
        // tracks were present, which surfaced the Split button in the UI
        // and made clicks fail with a "not found" error.
        let cd = concert_dir(&config.working_dir, album);
        if !concert.set_list.is_empty() && crate::scan::has_split_tracks(&cd, album) {
            let present: Vec<bool> = concert
                .set_list
                .iter()
                .map(|title| {
                    crate::model::find_track_file(&config.working_dir, album, title).is_some()
                })
                .collect();
            let conn = db.lock().unwrap();
            db::set_tracks_present(&conn, concert_id, &present)?;
            db::mark_split_succeeded(&conn, concert_id)?;
            return Ok(SetupResult::AlreadySplit);
        }
        Err(anyhow::anyhow!(
            "Downloaded file for concert {} (album {:?}) not found in {}",
            concert_id,
            album,
            config.working_dir.display()
        ))
    })();

    let (temp_file, input_file, output_dir) = match setup {
        Ok(SetupResult::Ready(t, i, o)) => (t, i, o),
        Ok(SetupResult::AlreadySplit) => {
            tracing::info!(
                "split auto-recovered for concert {} ({}) — tracks already present on disk",
                concert_id,
                title
            );
            return Ok(StartOutcome::AlreadySplit);
        }
        Err(e) => {
            // Setup failed after we marked the split as started — clear the flag
            // so the user can retry, and surface the error.
            let conn = db.lock().unwrap();
            let _ = db::mark_split_failed(&conn, concert_id, &e.to_string());
            return Err(e);
        }
    };
    let json_path = temp_file.path().to_path_buf();
    let job = SplitJob {
        concert_id: concert.id,
        json_path,
        input_file,
        output_dir,
        _temp_file: temp_file,
    };

    tracing::info!("split started for concert {} ({})", concert_id, title);
    let handle = tokio::task::spawn(run_split(db.clone(), config, job));
    registry.insert(key, handle);

    Ok(StartOutcome::Spawned)
}

async fn run_split(db: Arc<Mutex<Connection>>, config: JobConfig, job: SplitJob) {
    let concert_id = job.concert_id;
    let cmd = (config.split_cmd)(&job);

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

    match run_with_logging(cmd, "split", concert_id, temp_path.as_deref()).await {
        Ok((status, _)) if status.success() => {
            tracing::info!("split completed for concert {}", concert_id);
            drop(temp_file);
            let conn = db.lock().unwrap();
            let _ = db::mark_split_succeeded(&conn, concert_id);
            let concert = db::get_concert(&conn, concert_id);
            if let Ok(c) = concert {
                if let Some(album) = c.album.as_deref() {
                    let present: Vec<bool> = c
                        .set_list
                        .iter()
                        .map(|title| {
                            crate::model::find_track_file(&config.working_dir, album, title)
                                .is_some()
                        })
                        .collect();
                    let _ = db::set_tracks_present(&conn, concert_id, &present);
                }
            }
        }
        Ok((status, stderr_tail)) => {
            let error = format!("exit {:?}: {}", status.code(), stderr_tail.trim());
            tracing::warn!("split failed for concert {}: {}", concert_id, error);
            let conn = db.lock().unwrap();
            let _ = db::mark_split_failed(&conn, concert_id, &error);
            persist_job_log(&conn, concert_id, "split", &error, temp_file, &log_dir);
        }
        Err(e) => {
            let hint = if e.kind() == std::io::ErrorKind::NotFound {
                ". Is live-set-splitter built? Run: cargo build --bin live-set-splitter"
            } else {
                ""
            };
            let error = format!("spawn error: {}{}", e, hint);
            tracing::warn!("split failed for concert {}: {}", concert_id, error);
            let conn = db.lock().unwrap();
            let _ = db::mark_split_failed(&conn, concert_id, &error);
            persist_job_log(&conn, concert_id, "split", &error, temp_file, &log_dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{DownloadJob, JobConfig, JobRegistry};
    use std::fs;
    use std::path::PathBuf;
    use tokio::process::Command;

    fn config_for(working_dir: PathBuf) -> JobConfig {
        JobConfig {
            working_dir,
            download_cmd: Arc::new(|_: &DownloadJob| Command::new("true")),
            // Long sleep so a real spawn would be visibly running. Tests that
            // exercise the auto-recover path expect the splitter to never run;
            // tests that exercise the spawn path check the registry, not exit.
            split_cmd: Arc::new(|_| {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", "sleep 10"]);
                cmd
            }),
        }
    }

    fn seeded_db(album: &str, set_list: Vec<String>) -> Arc<Mutex<Connection>> {
        let conn = db::open_in_memory().unwrap();
        db::upsert_listing(
            &conn,
            &db::NewListing {
                source_url: format!("https://npr.org/c/{}", album),
                title: album.to_string(),
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
                album: album.to_string(),
                description: None,
                set_list,
                musicians: vec![],
            },
        )
        .unwrap();
        db::set_downloaded_at_if_missing(&conn, 1, "2024-01-01T00:00:00Z").unwrap();
        Arc::new(Mutex::new(conn))
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

        let db = seeded_db(album, set_list.clone());
        let registry = Arc::new(JobRegistry::new());
        let outcome = start_split(
            db.clone(),
            registry.clone(),
            config_for(tmp.path().to_path_buf()),
            1,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, StartOutcome::AlreadySplit));
        let conn = db.lock().unwrap();
        let c = db::get_concert(&conn, 1).unwrap();
        assert!(c.split_at.is_some(), "split_at should be set");
        assert_eq!(c.tracks_present, vec![true; 4]);
        assert!(c.split_errors.is_empty(), "no error should be recorded");
        // Auto-recovery must not spawn a splitter job.
        assert!(!registry.is_running(&JobKey {
            concert_id: 1,
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

        let db = seeded_db(album, set_list);
        let registry = Arc::new(JobRegistry::new());
        let outcome = start_split(
            db.clone(),
            registry,
            config_for(tmp.path().to_path_buf()),
            1,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, StartOutcome::AlreadySplit));
        let conn = db.lock().unwrap();
        let c = db::get_concert(&conn, 1).unwrap();
        assert!(c.split_at.is_some());
        assert_eq!(c.tracks_present, vec![true, false, true]);
    }

    #[tokio::test]
    async fn errors_when_no_source_and_no_tracks() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Empty Album";
        let set_list = vec!["Song".to_string()];
        // Concert dir does not exist at all — no source, no tracks.

        let db = seeded_db(album, set_list);
        let registry = Arc::new(JobRegistry::new());
        let err = start_split(
            db.clone(),
            registry,
            config_for(tmp.path().to_path_buf()),
            1,
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("Downloaded file"));
        let conn = db.lock().unwrap();
        let c = db::get_concert(&conn, 1).unwrap();
        assert!(c.split_at.is_none());
        assert!(!c.split_errors.is_empty(), "split error should be recorded");
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

        let db = seeded_db(album, set_list);
        let registry = Arc::new(JobRegistry::new());
        let outcome = start_split(
            db.clone(),
            registry.clone(),
            config_for(tmp.path().to_path_buf()),
            1,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, StartOutcome::Spawned));
        // The splitter command we configured `sleep 10`, so the job is running.
        assert!(registry.is_running(&JobKey {
            concert_id: 1,
            kind: JobKind::Split,
        }));
        // Stop the background sleeper so the test exits promptly.
        registry.cancel(&JobKey {
            concert_id: 1,
            kind: JobKind::Split,
        });
    }
}
