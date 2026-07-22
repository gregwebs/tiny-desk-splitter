//! Orchestrates the automated download → split chain behind track playback.
//!
//! `prepare` is the single entry point used when the user plays a track that
//! does not exist on disk yet. It is idempotent: repeat calls while a job is
//! running are no-ops (guarded by `try_mark_*_started` and the registry), and
//! it always converges on every set_list track file existing.

use anyhow::Result;
use rusqlite::Connection;
use std::sync::{Arc, Mutex};

use crate::concert_media::find_downloaded_file;
use crate::db;
use crate::jobs::{download, split, JobConfig, JobKey, JobKind, JobRegistry, SplitMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrepareOutcome {
    /// Every set_list track file already exists; nothing was started.
    Ready,
    /// A split job is running (or just finished via auto-recovery).
    Splitting,
    /// A download job is running with a split queued behind it.
    Downloading,
}

/// The concert has no album/set list, so there is nothing to split. The
/// caller should have the user scrape metadata first; HTTP handlers map this
/// to 422 (user-correctable) rather than 500.
#[derive(Debug)]
pub struct NoSetList {
    pub concert_id: i64,
}

impl std::fmt::Display for NoSetList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Concert {} has no set list; scrape metadata first",
            self.concert_id
        )
    }
}

impl std::error::Error for NoSetList {}

/// Ensure every set_list track file will (eventually) exist:
/// - all track files on disk            → `Ready` (no job started)
/// - source file on disk, tracks missing → start a split (re-split is allowed
///   when `split_at` is already set; it restores all deleted tracks)
/// - source file missing                → queue Split as a dependent of
///   Download, then start the download
///
/// Branches on actual source-file presence rather than `downloaded_at`: the
/// file may have been deleted out-of-band, and the split job cannot run
/// without it.
pub async fn prepare(
    db: Arc<Mutex<Connection>>,
    registry: Arc<JobRegistry>,
    config: JobConfig,
    concert_id: i64,
) -> Result<PrepareOutcome> {
    let concert = {
        let conn = db.lock().unwrap();
        db::concerts::get_concert(&conn, concert_id)?
    };
    if concert.album.is_none() || concert.set_list.is_empty() {
        return Err(NoSetList { concert_id }.into());
    }
    let album = concert.album.as_deref().expect("checked above");

    let all_present = crate::concert_media::ConcertMediaInventory::for_concert(
        &config.working_dir,
        &concert,
        None,
    )
    .all_tracks_present_on_disk();
    if all_present {
        tracing::debug!("prepare: concert {} already has all tracks", concert_id);
        return Ok(PrepareOutcome::Ready);
    }

    let download_key = JobKey {
        concert_id,
        kind: JobKind::Download,
    };
    let split_key = JobKey {
        concert_id,
        kind: JobKind::Split,
    };

    // Only treat the source file as usable when no registry-tracked download
    // is running: yt-dlp creates (and incrementally writes) the destination
    // file before it finishes, so file-presence alone could start a split on
    // a partial download. This guards downloads started by this process; a
    // yt-dlp run started out-of-band (CLI, another process) is invisible here
    // and its partial file would still be picked up — accepted limitation.
    let download_running = registry.is_running(&download_key);

    if !download_running && find_downloaded_file(&config.working_dir, album).is_some() {
        // Source file exists. `try_mark_split_started` requires downloaded_at,
        // which can be NULL when the file arrived outside the app (manual copy,
        // restored backup) — reconcile so the split can run.
        {
            let conn = db.lock().unwrap();
            db::lifecycle::set_downloaded_at_if_missing(
                &conn,
                concert_id,
                &db::time::now_string(),
            )?;
        }
        tracing::info!("prepare: starting split for concert {}", concert_id);
        split::start_split(db, registry, config, concert_id, SplitMode::Analyze).await?;
        return Ok(PrepareOutcome::Splitting);
    }

    registry.add_dependent(download_key.clone(), split_key.clone());
    // Race guard: the download may have completed between the checks above and
    // queueing the dependent, in which case its spawn_dependents already ran
    // and missed our entry. Dispatch the split ourselves — but only when the
    // download has truly finished (not running + file present); take_dependents
    // is an atomic removal, so even if the finishing download raced us here the
    // split is started exactly once. If an edge is ever left queued under a
    // download that never completes, it is harmless: prepare is re-entrant and
    // the next play click re-converges (add_dependent deduplicates).
    if !registry.is_running(&download_key)
        && find_downloaded_file(&config.working_dir, album).is_some()
    {
        tracing::info!(
            "prepare: download finished while queueing; dispatching split for concert {}",
            concert_id
        );
        crate::jobs::spawn_dependents(db, registry, config, &download_key);
        return Ok(PrepareOutcome::Splitting);
    }

    tracing::info!(
        "prepare: starting download with queued split for concert {}",
        concert_id
    );
    match download::start_download(db, registry.clone(), config, concert_id).await {
        Ok(_) => Ok(PrepareOutcome::Downloading),
        Err(e) => {
            // The download never started; remove the queued split so no stale
            // dependency remains.
            registry.drop_dependency_edges(&split_key);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{DownloadJob, JobRegistry, SplitJob};
    use crate::model::concert_dir;
    use std::fs;
    use std::path::PathBuf;
    use tokio::process::Command;

    const ALBUM: &str = "Prepare Album";

    fn seeded_db(set_list: Vec<String>) -> (Arc<Mutex<Connection>>, i64) {
        let conn = db::connection::open_in_memory().unwrap();
        let id = db::seeds::SeedContext::new(&conn)
            .seed_scraped_concert(db::seeds::SeedScrapedConcert {
                source_url: Some("https://npr.org/test/prepare".to_string()),
                title: Some("Prepare Concert".to_string()),
                concert_date: None,
                artist: Some("Test Artist".to_string()),
                album: Some(ALBUM.to_string()),
                set_list: Some(set_list),
            })
            .unwrap()
            .id;
        (Arc::new(Mutex::new(conn)), id)
    }

    /// Config whose download "fetches" the source file and whose splitter
    /// creates the per-song files — real commands, no mocks.
    fn config_for(working_dir: PathBuf, songs: &[&str]) -> JobConfig {
        let cd = concert_dir(&working_dir, ALBUM);
        let source = cd.join(format!("{}.mp4", ALBUM));
        let song_files: Vec<String> = songs
            .iter()
            .map(|s| format!("'{}.m4a'", cd.join(s).display()))
            .collect();
        let touch_songs = format!("touch {}", song_files.join(" "));
        let timestamps = songs
            .iter()
            .enumerate()
            .map(|(index, title)| {
                serde_json::json!({
                    "title": title,
                    "start_time": index as f64 * 10.0,
                    "end_time": (index + 1) as f64 * 10.0,
                    "duration": 10.0,
                })
            })
            .collect::<Vec<_>>();
        let timestamps_json = serde_json::json!({
            "artist": "Test Artist",
            "source": "",
            "show": "",
            "album": ALBUM,
            "set_list": [],
            "musicians": [],
            "timestamps": timestamps,
        })
        .to_string();
        let complete_split = format!(
            "{}; printf '%s' '{}' > '{}/timestamps.json'",
            touch_songs,
            timestamps_json,
            cd.display()
        );
        let fetch_source = format!(
            "mkdir -p '{}' && touch '{}'",
            cd.display(),
            source.display()
        );
        JobConfig::from_commands(
            working_dir,
            Arc::new(move |_: &DownloadJob| {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(fetch_source.clone());
                cmd
            }),
            Arc::new(move |_: &SplitJob| {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(complete_split.clone());
                cmd
            }),
            Arc::new(|_| Command::new("true")),
        )
    }

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
    async fn ready_when_all_tracks_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let songs = ["Alpha", "Beta"];
        let cd = concert_dir(tmp.path(), ALBUM);
        fs::create_dir_all(&cd).unwrap();
        for s in songs {
            fs::write(cd.join(format!("{}.m4a", s)), b"audio").unwrap();
        }
        let (db, id) = seeded_db(songs.iter().map(|s| s.to_string()).collect());
        let registry = Arc::new(JobRegistry::new());

        let outcome = prepare(
            db,
            registry.clone(),
            config_for(tmp.path().to_path_buf(), &songs),
            id,
        )
        .await
        .unwrap();

        assert_eq!(outcome, PrepareOutcome::Ready);
        assert!(!registry.is_running(&JobKey {
            concert_id: id,
            kind: JobKind::Split,
        }));
    }

    #[tokio::test]
    async fn splits_when_source_present_and_tracks_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let songs = ["Alpha", "Beta"];
        let cd = concert_dir(tmp.path(), ALBUM);
        fs::create_dir_all(&cd).unwrap();
        fs::write(cd.join(format!("{}.mp4", ALBUM)), b"video").unwrap();
        let (db, id) = seeded_db(songs.iter().map(|s| s.to_string()).collect());
        {
            let conn = db.lock().unwrap();
            db::lifecycle::set_downloaded_at_if_missing(&conn, id, "2024-01-01 00:00:00").unwrap();
        }
        let registry = Arc::new(JobRegistry::new());

        let outcome = prepare(
            db.clone(),
            registry,
            config_for(tmp.path().to_path_buf(), &songs),
            id,
        )
        .await
        .unwrap();

        assert_eq!(outcome, PrepareOutcome::Splitting);
        wait_for(&db, id, |c| c.split_at.is_some()).await;
        let conn = db.lock().unwrap();
        let c = db::concerts::get_concert(&conn, id).unwrap();
        assert_eq!(c.tracks_present, vec![true, true]);
    }

    #[tokio::test]
    async fn splits_when_source_present_but_downloaded_at_null() {
        // File arrived outside the app (manual copy / restored backup).
        let tmp = tempfile::tempdir().unwrap();
        let songs = ["Alpha"];
        let cd = concert_dir(tmp.path(), ALBUM);
        fs::create_dir_all(&cd).unwrap();
        fs::write(cd.join(format!("{}.mp4", ALBUM)), b"video").unwrap();
        let (db, id) = seeded_db(songs.iter().map(|s| s.to_string()).collect());
        let registry = Arc::new(JobRegistry::new());

        let outcome = prepare(
            db.clone(),
            registry,
            config_for(tmp.path().to_path_buf(), &songs),
            id,
        )
        .await
        .unwrap();

        assert_eq!(outcome, PrepareOutcome::Splitting);
        wait_for(&db, id, |c| c.split_at.is_some()).await;
        let conn = db.lock().unwrap();
        let c = db::concerts::get_concert(&conn, id).unwrap();
        assert!(c.downloaded_at.is_some(), "downloaded_at reconciled");
        assert_eq!(c.tracks_present, vec![true]);
    }

    #[tokio::test]
    async fn downloads_then_splits_when_source_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let songs = ["Alpha", "Beta"];
        let (db, id) = seeded_db(songs.iter().map(|s| s.to_string()).collect());
        let registry = Arc::new(JobRegistry::new());

        let outcome = prepare(
            db.clone(),
            registry.clone(),
            config_for(tmp.path().to_path_buf(), &songs),
            id,
        )
        .await
        .unwrap();

        assert_eq!(outcome, PrepareOutcome::Downloading);
        wait_for(&db, id, |c| c.split_at.is_some()).await;
        let conn = db.lock().unwrap();
        let c = db::concerts::get_concert(&conn, id).unwrap();
        assert!(c.downloaded_at.is_some());
        assert!(c.split_at.is_some());
        assert_eq!(c.tracks_present, vec![true, true]);
    }

    #[tokio::test]
    async fn resplits_after_track_deleted() {
        // Already split, one track file removed: prepare re-splits and the
        // rescan restores every track.
        let tmp = tempfile::tempdir().unwrap();
        let songs = ["Alpha", "Beta"];
        let cd = concert_dir(tmp.path(), ALBUM);
        fs::create_dir_all(&cd).unwrap();
        fs::write(cd.join(format!("{}.mp4", ALBUM)), b"video").unwrap();
        fs::write(cd.join("Beta.m4a"), b"audio").unwrap(); // Alpha deleted
        let (db, id) = seeded_db(songs.iter().map(|s| s.to_string()).collect());
        {
            let conn = db.lock().unwrap();
            db::lifecycle::set_downloaded_at_if_missing(&conn, id, "2024-01-01 00:00:00").unwrap();
            db::lifecycle::try_mark_split_started(&conn, id).unwrap();
            db::lifecycle::mark_split_succeeded(&conn, id).unwrap();
            db::split_timestamps::set_tracks_present(&conn, id, &[false, true]).unwrap();
        }
        let registry = Arc::new(JobRegistry::new());

        let outcome = prepare(
            db.clone(),
            registry,
            config_for(tmp.path().to_path_buf(), &songs),
            id,
        )
        .await
        .unwrap();

        assert_eq!(outcome, PrepareOutcome::Splitting);
        wait_for(&db, id, |c| c.tracks_present == vec![true, true]).await;
        let conn = db.lock().unwrap();
        let c = db::concerts::get_concert(&conn, id).unwrap();
        assert_eq!(c.tracks_present, vec![true, true], "deleted track restored");
    }

    #[tokio::test]
    async fn redownloads_when_source_deleted_even_if_downloaded_at_set() {
        // Source file deleted out-of-band while downloaded_at is still set:
        // prepare must chain download → split, not trust the stale column.
        let tmp = tempfile::tempdir().unwrap();
        let songs = ["Alpha"];
        let (db, id) = seeded_db(songs.iter().map(|s| s.to_string()).collect());
        {
            let conn = db.lock().unwrap();
            db::lifecycle::set_downloaded_at_if_missing(&conn, id, "2024-01-01 00:00:00").unwrap();
        }
        let registry = Arc::new(JobRegistry::new());

        let outcome = prepare(
            db.clone(),
            registry.clone(),
            config_for(tmp.path().to_path_buf(), &songs),
            id,
        )
        .await
        .unwrap();

        assert_eq!(outcome, PrepareOutcome::Downloading);
        wait_for(&db, id, |c| c.tracks_present == vec![true]).await;
        let conn = db.lock().unwrap();
        let c = db::concerts::get_concert(&conn, id).unwrap();
        assert_eq!(c.tracks_present, vec![true]);
    }

    #[tokio::test]
    async fn repeat_prepare_while_running_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let songs = ["Alpha"];
        let (db, id) = seeded_db(songs.iter().map(|s| s.to_string()).collect());
        let registry = Arc::new(JobRegistry::new());
        // Slow download so the second prepare arrives mid-job.
        let cd = concert_dir(tmp.path(), ALBUM);
        let source = cd.join(format!("{}.mp4", ALBUM));
        let fetch = format!(
            "sleep 0.3 && mkdir -p '{}' && touch '{}'",
            cd.display(),
            source.display()
        );
        let touch = format!(
            "touch '{}'; printf '%s' '{}' > '{}/timestamps.json'",
            cd.join("Alpha.m4a").display(),
            r#"{"artist":"A","source":"","show":"","album":"","set_list":[],"musicians":[],"timestamps":[{"title":"Alpha","start_time":0.0,"end_time":10.0,"duration":10.0}]}"#,
            cd.display()
        );
        let config = JobConfig::from_commands(
            tmp.path().to_path_buf(),
            Arc::new(move |_: &DownloadJob| {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(fetch.clone());
                cmd
            }),
            Arc::new(move |_: &SplitJob| {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(touch.clone());
                cmd
            }),
            Arc::new(|_| Command::new("true")),
        );

        let o1 = prepare(db.clone(), registry.clone(), config.clone(), id)
            .await
            .unwrap();
        let o2 = prepare(db.clone(), registry.clone(), config.clone(), id)
            .await
            .unwrap();
        assert_eq!(o1, PrepareOutcome::Downloading);
        assert_eq!(o2, PrepareOutcome::Downloading);

        wait_for(&db, id, |c| c.tracks_present == vec![true]).await;
        let conn = db.lock().unwrap();
        let c = db::concerts::get_concert(&conn, id).unwrap();
        assert_eq!(c.tracks_present, vec![true]);
        assert!(c.download_errors.is_empty());
        assert!(c.split_errors.is_empty());
    }

    #[tokio::test]
    async fn errors_without_set_list() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, id) = seeded_db(vec![]);
        let registry = Arc::new(JobRegistry::new());
        let result = prepare(db, registry, config_for(tmp.path().to_path_buf(), &[]), id).await;
        assert!(result.is_err());
    }
}
