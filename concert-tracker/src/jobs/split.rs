use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use std::io::Write;
use std::sync::{Arc, Mutex};
use tempfile::NamedTempFile;

use crate::db;
use crate::jobs::{JobConfig, JobKey, JobKind, JobRegistry, SplitJob};
use crate::model::{Concert, Musician};

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
            return Ok(StartOutcome::NotDownloaded);
        }
        return Ok(StartOutcome::AlreadyRunning);
    }

    let temp_file = write_splitter_input(&concert)?;
    let json_path = temp_file.path().to_path_buf();
    let job = SplitJob {
        concert_id: concert.id,
        json_path,
        working_dir: config.working_dir.clone(),
        _temp_file: temp_file,
    };

    let handle = tokio::task::spawn(run_split(db.clone(), config, job));
    registry.insert(key, handle);

    Ok(StartOutcome::Spawned)
}

async fn run_split(db: Arc<Mutex<Connection>>, config: JobConfig, job: SplitJob) {
    let concert_id = job.concert_id;
    let mut cmd = (config.split_cmd)(&job);

    match cmd.output().await {
        Ok(output) if output.status.success() => {
            let conn = db.lock().unwrap();
            let _ = db::mark_split_succeeded(&conn, concert_id);
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let error = format!("exit {:?}: {}", output.status.code(), stderr.trim());
            let conn = db.lock().unwrap();
            let _ = db::mark_split_failed(&conn, concert_id, &error);
        }
        Err(e) => {
            let error = format!("spawn error: {}", e);
            let conn = db.lock().unwrap();
            let _ = db::mark_split_failed(&conn, concert_id, &error);
        }
    }
}
