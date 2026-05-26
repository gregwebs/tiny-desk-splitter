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

pub enum StartOutcome {
    Spawned,
    AlreadyRunning,
    NotDownloaded,
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
    let album = concert.album.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "Concert {} has no album, cannot locate input file",
            concert_id
        )
    })?;
    let input_file = find_downloaded_file(&config.working_dir, album).ok_or_else(|| {
        anyhow::anyhow!(
            "Downloaded file for concert {} (album {:?}) not found in {}",
            concert_id,
            album,
            config.working_dir.display()
        )
    })?;
    let temp_file = write_splitter_input(&concert)?;
    let json_path = temp_file.path().to_path_buf();
    let output_dir = concert_dir(&config.working_dir, album);
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
