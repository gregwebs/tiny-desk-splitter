pub mod download;
pub mod split;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::process::Command;
use tokio::task::JoinHandle;

use crate::model::sanitize_album;
use crate::model::Concert;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JobKind {
    Download,
    Split,
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
    pub working_dir: PathBuf,
    pub _temp_file: tempfile::NamedTempFile,
}

#[derive(Clone)]
pub struct JobConfig {
    pub working_dir: PathBuf,
    pub download_cmd: Arc<dyn Fn(&DownloadJob) -> Command + Send + Sync>,
    pub split_cmd: Arc<dyn Fn(&SplitJob) -> Command + Send + Sync>,
}

impl JobConfig {
    pub fn production(working_dir: PathBuf) -> Self {
        let wd = working_dir.clone();
        JobConfig {
            working_dir: working_dir.clone(),
            download_cmd: Arc::new(move |job: &DownloadJob| {
                let out = wd
                    .join(format!("{}.%(ext)s", sanitize_album(&job.album)))
                    .to_string_lossy()
                    .to_string();
                let mut cmd = Command::new("yt-dlp");
                cmd.arg("-o").arg(out).arg(&job.source_url);
                cmd
            }),
            split_cmd: Arc::new(move |job: &SplitJob| {
                let mut cmd = Command::new("live-set-splitter");
                cmd.arg(&job.json_path).arg(&job.working_dir);
                cmd
            }),
        }
    }
}

pub fn download_job_from_concert(concert: &Concert, working_dir: &Path) -> anyhow::Result<DownloadJob> {
    let album = concert
        .album
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Concert {} has no album", concert.id))?;
    Ok(DownloadJob {
        concert_id: concert.id,
        source_url: concert.source_url.clone(),
        album: album.clone(),
        working_dir: working_dir.to_path_buf(),
    })
}
