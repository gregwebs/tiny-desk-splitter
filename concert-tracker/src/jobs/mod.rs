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
    pub input_file: PathBuf,
    pub working_dir: PathBuf,
    pub _temp_file: tempfile::NamedTempFile,
}

#[derive(Clone)]
pub struct JobConfig {
    pub working_dir: PathBuf,
    pub download_cmd: Arc<dyn Fn(&DownloadJob) -> Command + Send + Sync>,
    pub split_cmd: Arc<dyn Fn(&SplitJob) -> Command + Send + Sync>,
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

impl JobConfig {
    pub fn production(working_dir: PathBuf, splitter_bin: PathBuf) -> Self {
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
                let mut cmd = Command::new(&splitter_bin);
                cmd.arg(&job.json_path)
                    .arg("--input-file")
                    .arg(&job.input_file)
                    .arg("--output-dir")
                    .arg(&job.working_dir);
                cmd
            }),
        }
    }
}

/// Find the downloaded media file for an album in `working_dir`.
///
/// yt-dlp writes the file as `{sanitize_album(album)}.{ext}` where `ext` is
/// picked at runtime (typically `mp4`). We don't know the extension up front,
/// so we list the directory and return the first entry whose file stem matches
/// the sanitized album. `.json` is excluded as a safety belt against scraped
/// metadata sidecars.
pub fn find_downloaded_file(working_dir: &Path, album: &str) -> Option<PathBuf> {
    let expected_stem = sanitize_album(album);
    let entries = std::fs::read_dir(working_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem != expected_stem {
            continue;
        }
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if ext.eq_ignore_ascii_case("json") {
            continue;
        }
        return Some(path);
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn find_downloaded_file_returns_match_for_known_extension() {
        let dir = tempfile::tempdir().unwrap();
        File::create(dir.path().join("Foo Album.mp4")).unwrap();
        let found = find_downloaded_file(dir.path(), "Foo Album").unwrap();
        assert_eq!(found, dir.path().join("Foo Album.mp4"));
    }

    #[test]
    fn find_downloaded_file_ignores_json_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        File::create(dir.path().join("Foo Album.json")).unwrap();
        File::create(dir.path().join("Foo Album.mp4")).unwrap();
        let found = find_downloaded_file(dir.path(), "Foo Album").unwrap();
        assert_eq!(found, dir.path().join("Foo Album.mp4"));
    }

    #[test]
    fn find_downloaded_file_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_downloaded_file(dir.path(), "Foo Album").is_none());
    }

    #[test]
    fn find_downloaded_file_handles_colons_in_album() {
        let dir = tempfile::tempdir().unwrap();
        File::create(dir.path().join("A B.mp4")).unwrap();
        let found = find_downloaded_file(dir.path(), "A: B").unwrap();
        assert_eq!(found, dir.path().join("A B.mp4"));
    }

    #[test]
    fn find_downloaded_file_returns_none_when_only_json_exists() {
        let dir = tempfile::tempdir().unwrap();
        File::create(dir.path().join("Foo Album.json")).unwrap();
        assert!(find_downloaded_file(dir.path(), "Foo Album").is_none());
    }
}
