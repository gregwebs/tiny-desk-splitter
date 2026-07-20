use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use rusqlite::Connection;

use crate::concert_media::{find_downloaded_file, source_redundant};
use crate::db;
use crate::events::{self, Event};
use crate::jobs::{JobConfig, JobKind, JobRegistry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteDownloadOutcome {
    Deleted { removed_file: Option<PathBuf> },
    MissingFileRequiresConfirmation,
    NotDownloaded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteRedundantSourceOutcome {
    Deleted { removed_file: Option<PathBuf> },
    NotRedundant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteSplitOutcome {
    Deleted,
    NoSplitState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteTrackOutcome {
    pub track_index: usize,
    pub track_title: String,
    pub removed_files: Vec<PathBuf>,
    pub split_cleared: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelJobOutcome {
    CancelledRunning,
    DroppedQueued,
    MarkedStaleFailed,
    NoSuchActiveJob,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InProgressFailureCount {
    pub downloads: usize,
    pub splits: usize,
    pub archives: usize,
}

impl InProgressFailureCount {
    pub fn as_tuple(self) -> (usize, usize, usize) {
        (self.downloads, self.splits, self.archives)
    }
}

pub fn delete_download(
    conn: &Connection,
    working_dir: &Path,
    id: i64,
    force: bool,
) -> Result<DeleteDownloadOutcome> {
    let concert = db::concerts::get_concert(conn, id)?;
    if concert.downloaded_at.is_none() {
        return Ok(DeleteDownloadOutcome::NotDownloaded);
    }

    let mut removed_file = None;
    if !force {
        let path = concert
            .album
            .as_deref()
            .and_then(|album| find_downloaded_file(working_dir, album));
        let Some(path) = path else {
            return Ok(DeleteDownloadOutcome::MissingFileRequiresConfirmation);
        };
        std::fs::remove_file(&path)
            .with_context(|| format!("Failed to remove {}", path.display()))?;
        removed_file = Some(path);
    }

    db::lifecycle::clear_download_state(conn, id)?;
    Ok(DeleteDownloadOutcome::Deleted { removed_file })
}

pub fn delete_redundant_source(
    conn: &Connection,
    working_dir: &Path,
    id: i64,
) -> Result<DeleteRedundantSourceOutcome> {
    let concert = db::concerts::get_concert(conn, id)?;
    let album = concert.album.as_deref().unwrap_or("");
    let stored_ts = db::split_timestamps::get_split_timestamps(conn, id)?.user;
    let is_redundant = source_redundant(
        working_dir,
        album,
        &concert.tracks_present,
        stored_ts.as_deref(),
        concert.media_duration,
    );
    if !is_redundant {
        return Ok(DeleteRedundantSourceOutcome::NotRedundant);
    }

    let source_path = concert
        .album
        .as_deref()
        .and_then(|album| find_downloaded_file(working_dir, album));
    if let Some(path) = source_path.as_ref() {
        std::fs::remove_file(path)
            .with_context(|| format!("Failed to remove {}", path.display()))?;
    }

    db::lifecycle::clear_download_state(conn, id)?;
    events::record_now(conn, id, Event::SourceRedundantDelete, None);
    Ok(DeleteRedundantSourceOutcome::Deleted {
        removed_file: source_path,
    })
}

pub fn delete_split(conn: &Connection, id: i64) -> Result<DeleteSplitOutcome> {
    let concert = db::concerts::get_concert(conn, id)?;
    let has_split_error = !concert.split_errors.is_empty();
    if concert.split_at.is_none() && !has_split_error {
        return Ok(DeleteSplitOutcome::NoSplitState);
    }

    db::lifecycle::clear_split_state(conn, id)?;
    Ok(DeleteSplitOutcome::Deleted)
}

pub fn delete_track(
    conn: &Connection,
    working_dir: &Path,
    id: i64,
    track_index: usize,
) -> Result<DeleteTrackOutcome> {
    let concert = db::concerts::get_concert(conn, id)?;
    let track_title = concert
        .set_list
        .get(track_index)
        .ok_or_else(|| anyhow!("track index {track_index} not found"))?
        .clone();
    let album = concert
        .album
        .as_deref()
        .ok_or_else(|| anyhow!("concert has no album"))?;
    let stem = crate::model::sanitize_filename(&track_title);
    let dir = crate::model::concert_dir(working_dir, album);

    let mut removed_files = Vec::new();
    for ext in &["mp4", "m4a"] {
        let path = dir.join(format!("{stem}.{ext}"));
        if !path.exists() {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {
                tracing::info!(
                    "delete_track: removed {} for concert {}",
                    path.display(),
                    id
                );
                removed_files.push(path);
            }
            Err(e) => {
                tracing::warn!("delete_track: failed to remove {}: {}", path.display(), e);
            }
        }
    }

    let json =
        serde_json::json!({"track_index": track_index, "track_title": &track_title}).to_string();
    events::record_now(conn, id, Event::TrackDelete, Some(&json));

    let mut tracks_present = concert.tracks_present.clone();
    if tracks_present.len() < concert.set_list.len() {
        tracks_present.resize(concert.set_list.len(), true);
    }
    tracks_present[track_index] = false;

    let split_cleared = tracks_present.iter().all(|present| !present);
    if split_cleared {
        db::lifecycle::clear_split_state(conn, id)?;
    } else {
        db::split_timestamps::set_tracks_present(conn, id, &tracks_present)?;
    }

    Ok(DeleteTrackOutcome {
        track_index,
        track_title,
        removed_files,
        split_cleared,
    })
}

/// Cancel the running/queued/stale job named by `(id, job_kind)`.
///
/// All three kinds route through the Job Run engine's
/// [`crate::jobs::run::cancel`]: gate-arbitrated exactly-one-terminal-outcome,
/// and a Failed Job row on a won cancellation (see `docs/jobs.md`).
pub fn cancel_job(
    conn: &Connection,
    registry: &Arc<JobRegistry>,
    _jobs: &JobConfig,
    id: i64,
    job_kind: JobKind,
) -> Result<CancelJobOutcome> {
    let outcome = match job_kind {
        JobKind::Download => crate::jobs::run::cancel(
            conn,
            registry,
            &crate::jobs::download::DownloadCancellation::new(id),
        )?,
        JobKind::Split => crate::jobs::run::cancel(
            conn,
            registry,
            &crate::jobs::split::SplitCancellation::new(id),
        )?,
        JobKind::Archive => crate::jobs::run::cancel(
            conn,
            registry,
            &crate::jobs::archive::ArchiveCancellation::new(id),
        )?,
    };
    Ok(match outcome {
        crate::jobs::run::CancelOutcome::CancelledRunning => CancelJobOutcome::CancelledRunning,
        crate::jobs::run::CancelOutcome::DroppedQueued => CancelJobOutcome::DroppedQueued,
        crate::jobs::run::CancelOutcome::MarkedStaleFailed => CancelJobOutcome::MarkedStaleFailed,
        crate::jobs::run::CancelOutcome::NoSuchActiveJob => CancelJobOutcome::NoSuchActiveJob,
    })
}

/// Convert every stale accepted Job Run (download, split, archive) into a
/// transactional Failed Job via [`crate::jobs::run::recover_failed`]. Used at
/// server startup (before the registry exists) and after graceful shutdown's
/// `JobRegistry::cancel_all` (once every slot/gate is already gone) — see
/// `recover_failed`'s doc comment for why no gate/reservation is needed here.
pub fn fail_in_progress_jobs(conn: &Connection, error: &str) -> Result<InProgressFailureCount> {
    let download_ids = ids_with_column(conn, "download_started_at")?;
    for id in &download_ids {
        crate::jobs::run::recover_failed(
            conn,
            &crate::jobs::download::DownloadCancellation::new(*id),
            error,
        )?;
    }

    let split_ids = ids_with_column(conn, "split_started_at")?;
    for id in &split_ids {
        crate::jobs::run::recover_failed(
            conn,
            &crate::jobs::split::SplitCancellation::new(*id),
            error,
        )?;
    }

    let archive_ids = ids_with_column(conn, "archive_started_at")?;
    for id in &archive_ids {
        crate::jobs::run::recover_failed(
            conn,
            &crate::jobs::archive::ArchiveCancellation::new(*id),
            error,
        )?;
    }

    Ok(InProgressFailureCount {
        downloads: download_ids.len(),
        splits: split_ids.len(),
        archives: archive_ids.len(),
    })
}

pub fn reset_in_progress(conn: &Connection) -> Result<usize> {
    let rows = conn
        .execute(
            "UPDATE concerts SET download_started_at = NULL, split_started_at = NULL, archive_started_at = NULL
             WHERE download_started_at IS NOT NULL OR split_started_at IS NOT NULL OR archive_started_at IS NOT NULL",
            [],
        )
        .context("Failed to reset in-progress")?;
    Ok(rows)
}

fn ids_with_column(conn: &Connection, column: &str) -> Result<Vec<i64>> {
    conn.prepare(&format!(
        "SELECT id FROM concerts WHERE {column} IS NOT NULL ORDER BY id"
    ))?
    .query_map([], |row| row.get::<_, i64>(0))?
    .collect::<rusqlite::Result<_>>()
    .with_context(|| format!("Failed to read concerts with {column}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tokio::sync::oneshot;

    use crate::jobs::run::CANCELLED_BY_USER;
    use crate::jobs::JobKey;
    use crate::model::{concert_dir, sanitize_filename};

    fn insert_concert(conn: &Connection, album: &str, tracks: &[&str]) -> i64 {
        let source_url = format!("https://example.test/{album}");
        db::seeds::SeedContext::new(conn)
            .seed_scraped_concert(db::seeds::SeedScrapedConcert {
                source_url: Some(source_url),
                title: Some(album.to_string()),
                concert_date: None,
                artist: Some("Artist".to_string()),
                album: Some(album.to_string()),
                set_list: Some(tracks.iter().map(|track| track.to_string()).collect()),
            })
            .unwrap()
            .id
    }

    fn downloaded_file(working_dir: &Path, album: &str) -> PathBuf {
        let dir = concert_dir(working_dir, album);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.mp4", sanitize_filename(album)));
        fs::write(&path, b"source").unwrap();
        path
    }

    fn track_file(working_dir: &Path, album: &str, title: &str, ext: &str) -> PathBuf {
        let dir = concert_dir(working_dir, album);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.{}", sanitize_filename(title), ext));
        fs::write(&path, b"track").unwrap();
        path
    }

    fn events_for(conn: &Connection, id: i64) -> Vec<String> {
        events::list_for_concert(conn, id)
            .into_iter()
            .map(|event| event.event)
            .collect()
    }

    #[test]
    fn delete_download_preserves_split_state() {
        let conn = db::connection::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let id = insert_concert(&conn, "Album", &["One", "Two"]);
        let source = downloaded_file(dir.path(), "Album");
        db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();
        db::lifecycle::mark_split_succeeded(&conn, id).unwrap();
        db::split_timestamps::set_tracks_present(&conn, id, &[true, true]).unwrap();

        let outcome = delete_download(&conn, dir.path(), id, false).unwrap();

        assert_eq!(
            outcome,
            DeleteDownloadOutcome::Deleted {
                removed_file: Some(source)
            }
        );
        let concert = db::concerts::get_concert(&conn, id).unwrap();
        assert!(concert.downloaded_at.is_none());
        assert!(concert.split_at.is_some());
        assert_eq!(concert.tracks_present, vec![true, true]);
        assert!(events_for(&conn, id).contains(&"download_delete".to_string()));
    }

    #[test]
    fn delete_redundant_source_requires_coverage_and_records_both_events() {
        let conn = db::connection::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let id = insert_concert(&conn, "Album", &["One"]);
        let source = downloaded_file(dir.path(), "Album");
        db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();
        db::split_timestamps::set_tracks_present(&conn, id, &[true]).unwrap();
        db::split_timestamps::set_media_duration(&conn, id, 10.0).unwrap();

        assert_eq!(
            delete_redundant_source(&conn, dir.path(), id).unwrap(),
            DeleteRedundantSourceOutcome::NotRedundant
        );

        let timestamps = serde_json::to_string(&vec![concert_types::SongTimestamp {
            title: "One".to_string(),
            start_time: 0.0,
            end_time: 10.0,
            duration: 10.0,
        }])
        .unwrap();
        conn.execute(
            "UPDATE concerts SET user_split_timestamps_json = ?1 WHERE id = ?2",
            rusqlite::params![timestamps, id],
        )
        .unwrap();

        assert_eq!(
            delete_redundant_source(&conn, dir.path(), id).unwrap(),
            DeleteRedundantSourceOutcome::Deleted {
                removed_file: Some(source)
            }
        );
        let event_names = events_for(&conn, id);
        assert!(event_names.contains(&"download_delete".to_string()));
        assert!(event_names.contains(&"source_redundant_delete".to_string()));
    }

    #[test]
    fn delete_redundant_source_is_not_redundant_once_source_is_already_gone() {
        let conn = db::connection::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let id = insert_concert(&conn, "Album", &["One"]);
        downloaded_file(dir.path(), "Album");
        db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();
        db::split_timestamps::set_tracks_present(&conn, id, &[true]).unwrap();
        db::split_timestamps::set_media_duration(&conn, id, 10.0).unwrap();
        let timestamps = serde_json::to_string(&vec![concert_types::SongTimestamp {
            title: "One".to_string(),
            start_time: 0.0,
            end_time: 10.0,
            duration: 10.0,
        }])
        .unwrap();
        conn.execute(
            "UPDATE concerts SET user_split_timestamps_json = ?1 WHERE id = ?2",
            rusqlite::params![timestamps, id],
        )
        .unwrap();

        assert!(matches!(
            delete_redundant_source(&conn, dir.path(), id).unwrap(),
            DeleteRedundantSourceOutcome::Deleted { .. }
        ));

        // Re-checking now that the source file is gone must fail closed
        // rather than report a second (fictitious) deletion.
        assert_eq!(
            delete_redundant_source(&conn, dir.path(), id).unwrap(),
            DeleteRedundantSourceOutcome::NotRedundant
        );
    }

    #[test]
    fn delete_split_clears_tracks_present_and_split_errors() {
        let conn = db::connection::open_in_memory().unwrap();
        let id = insert_concert(&conn, "Album", &["One"]);
        db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();
        db::lifecycle::mark_split_succeeded(&conn, id).unwrap();
        db::split_timestamps::set_tracks_present(&conn, id, &[true]).unwrap();
        db::lifecycle::mark_split_failed(&conn, id, "bad split").unwrap();

        assert_eq!(
            delete_split(&conn, id).unwrap(),
            DeleteSplitOutcome::Deleted
        );

        let concert = db::concerts::get_concert(&conn, id).unwrap();
        assert!(concert.split_at.is_none());
        assert!(concert.split_started_at.is_none());
        assert!(concert.tracks_present.is_empty());
        assert!(concert.split_errors.is_empty());
    }

    #[test]
    fn delete_track_clears_split_only_when_last_present_track_is_removed() {
        let conn = db::connection::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let id = insert_concert(&conn, "Album", &["One", "Two"]);
        track_file(dir.path(), "Album", "One", "mp4");
        track_file(dir.path(), "Album", "Two", "m4a");
        db::lifecycle::mark_split_succeeded(&conn, id).unwrap();
        db::split_timestamps::set_tracks_present(&conn, id, &[true, true]).unwrap();

        let first = delete_track(&conn, dir.path(), id, 0).unwrap();
        assert!(!first.split_cleared);
        let concert = db::concerts::get_concert(&conn, id).unwrap();
        assert!(concert.split_at.is_some());
        assert_eq!(concert.tracks_present, vec![false, true]);

        let second = delete_track(&conn, dir.path(), id, 1).unwrap();
        assert!(second.split_cleared);
        let concert = db::concerts::get_concert(&conn, id).unwrap();
        assert!(concert.split_at.is_none());
        assert!(concert.tracks_present.is_empty());
    }

    #[tokio::test]
    async fn cancel_distinguishes_running_queued_stale_and_absent_jobs() {
        let conn = db::connection::open_in_memory().unwrap();
        let registry = Arc::new(JobRegistry::new());
        let jobs = JobConfig::test(std::path::PathBuf::from("/tmp"));
        let running_id = insert_concert(&conn, "Running", &["One"]);
        let queued_id = insert_concert(&conn, "Queued", &["One"]);
        let stale_id = insert_concert(&conn, "Stale", &["One"]);
        let absent_id = insert_concert(&conn, "Absent", &["One"]);

        db::lifecycle::try_mark_download_started(&conn, running_id).unwrap();
        let (_tx, rx) = oneshot::channel::<()>();
        registry.insert(
            JobKey {
                concert_id: running_id,
                kind: JobKind::Download,
            },
            tokio::spawn(async move {
                let _ = rx.await;
            }),
        );

        registry.add_dependent(
            JobKey {
                concert_id: running_id,
                kind: JobKind::Download,
            },
            JobKey {
                concert_id: queued_id,
                kind: JobKind::Split,
            },
        );

        db::lifecycle::mark_download_succeeded(&conn, stale_id, "mp4").unwrap();
        db::lifecycle::try_mark_split_started(&conn, stale_id).unwrap();

        assert_eq!(
            cancel_job(&conn, &registry, &jobs, queued_id, JobKind::Split).unwrap(),
            CancelJobOutcome::DroppedQueued
        );
        assert_eq!(
            cancel_job(&conn, &registry, &jobs, running_id, JobKind::Download).unwrap(),
            CancelJobOutcome::CancelledRunning
        );
        assert_eq!(
            cancel_job(&conn, &registry, &jobs, stale_id, JobKind::Split).unwrap(),
            CancelJobOutcome::MarkedStaleFailed
        );
        assert_eq!(
            cancel_job(&conn, &registry, &jobs, absent_id, JobKind::Archive).unwrap(),
            CancelJobOutcome::NoSuchActiveJob
        );

        assert!(db::concerts::get_concert(&conn, running_id)
            .unwrap()
            .download_started_at
            .is_none());
        assert!(db::concerts::get_concert(&conn, stale_id)
            .unwrap()
            .split_started_at
            .is_none());
    }

    #[tokio::test]
    async fn cancelling_a_running_download_creates_failed_job_history() {
        // #125 acceptance criterion: user cancellation of an accepted Job
        // Run produces exactly one terminal outcome AND a Failed Job — the
        // legacy Split/Archive path only cleared lifecycle columns.
        let conn = db::connection::open_in_memory().unwrap();
        let registry = Arc::new(JobRegistry::new());
        let jobs = JobConfig::test(std::path::PathBuf::from("/tmp"));
        let id = insert_concert(&conn, "Cancel Me", &["One"]);
        db::lifecycle::try_mark_download_started(&conn, id).unwrap();
        let (_tx, rx) = oneshot::channel::<()>();
        registry.insert(
            JobKey {
                concert_id: id,
                kind: JobKind::Download,
            },
            tokio::spawn(async move {
                let _ = rx.await;
            }),
        );

        assert_eq!(
            cancel_job(&conn, &registry, &jobs, id, JobKind::Download).unwrap(),
            CancelJobOutcome::CancelledRunning
        );

        let concert = db::concerts::get_concert(&conn, id).unwrap();
        assert!(concert.download_started_at.is_none());
        assert_eq!(
            concert.download_errors.last().unwrap().error,
            CANCELLED_BY_USER
        );
        let failed = db::failed_jobs::list_failed_jobs(&conn, 10).unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].concert_id, id);
        assert_eq!(failed[0].failure_message, CANCELLED_BY_USER);
    }

    #[tokio::test]
    async fn cancelling_a_running_split_creates_failed_job_history() {
        let conn = db::connection::open_in_memory().unwrap();
        let registry = Arc::new(JobRegistry::new());
        let jobs = JobConfig::test(std::path::PathBuf::from("/tmp"));
        let id = insert_concert(&conn, "Cancel Split", &["One"]);
        db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();
        db::lifecycle::try_mark_split_started(&conn, id).unwrap();
        let (_tx, rx) = oneshot::channel::<()>();
        registry.insert(
            JobKey {
                concert_id: id,
                kind: JobKind::Split,
            },
            tokio::spawn(async move {
                let _ = rx.await;
            }),
        );

        assert_eq!(
            cancel_job(&conn, &registry, &jobs, id, JobKind::Split).unwrap(),
            CancelJobOutcome::CancelledRunning
        );

        let concert = db::concerts::get_concert(&conn, id).unwrap();
        assert!(concert.split_started_at.is_none());
        assert_eq!(
            concert.split_errors.last().unwrap().error,
            CANCELLED_BY_USER
        );
        let failed = db::failed_jobs::list_failed_jobs(&conn, 10).unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].name, "split");
        assert_eq!(failed[0].failure_message, CANCELLED_BY_USER);
    }

    #[tokio::test]
    async fn cancelling_a_running_archive_creates_failed_job_history() {
        // #127 acceptance criterion: archive cancellation now goes through the
        // same engine as download/split — one terminal outcome AND a Failed
        // Job (the pre-#127 legacy path only cleared lifecycle columns).
        let conn = db::connection::open_in_memory().unwrap();
        let registry = Arc::new(JobRegistry::new());
        let jobs = JobConfig::test(std::path::PathBuf::from("/tmp"));
        let id = insert_concert(&conn, "Cancel Archive", &["One"]);
        db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();
        db::lifecycle::try_mark_archive_started(&conn, id).unwrap();
        let (_tx, rx) = oneshot::channel::<()>();
        registry.insert(
            JobKey {
                concert_id: id,
                kind: JobKind::Archive,
            },
            tokio::spawn(async move {
                let _ = rx.await;
            }),
        );

        assert_eq!(
            cancel_job(&conn, &registry, &jobs, id, JobKind::Archive).unwrap(),
            CancelJobOutcome::CancelledRunning
        );

        let concert = db::concerts::get_concert(&conn, id).unwrap();
        assert!(concert.archive_started_at.is_none());
        assert!(concert.archived_at.is_none());
        assert_eq!(
            concert.archive_errors.last().unwrap().error,
            CANCELLED_BY_USER
        );
        let failed = db::failed_jobs::list_failed_jobs(&conn, 10).unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].name, "archive");
        assert_eq!(failed[0].failure_message, CANCELLED_BY_USER);
    }

    #[tokio::test]
    async fn split_cancellation_reports_terminal_persistence_failure() {
        let conn = db::connection::open_in_memory().unwrap();
        let registry = Arc::new(JobRegistry::new());
        let jobs = JobConfig::test(std::path::PathBuf::from("/tmp"));
        let id = insert_concert(&conn, "Cancel Persistence Failure", &["One"]);
        db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();
        db::lifecycle::try_mark_split_started(&conn, id).unwrap();
        let (_tx, rx) = oneshot::channel::<()>();
        let key = JobKey {
            concert_id: id,
            kind: JobKind::Split,
        };
        registry.insert(
            key.clone(),
            tokio::spawn(async move {
                let _ = rx.await;
            }),
        );
        conn.execute_batch(
            "CREATE TRIGGER reject_split_error_event
             BEFORE INSERT ON events
             WHEN NEW.event = 'split_error'
             BEGIN SELECT RAISE(ABORT, 'reject split error event'); END;",
        )
        .unwrap();

        let error = cancel_job(&conn, &registry, &jobs, id, JobKind::Split).unwrap_err();

        assert!(error
            .to_string()
            .contains("Failed to commit cancelled terminal"));
        assert!(
            !registry.is_running(&key),
            "failed persistence must not leak the slot"
        );
        assert!(db::failed_jobs::list_failed_jobs(&conn, 10)
            .unwrap()
            .is_empty());
        assert!(db::concerts::get_concert(&conn, id)
            .unwrap()
            .split_started_at
            .is_some());
    }

    #[test]
    fn restart_recovery_marks_stale_download_split_and_archive_jobs_failed() {
        let conn = db::connection::open_in_memory().unwrap();
        let download_id = insert_concert(&conn, "Download", &["One"]);
        let split_id = insert_concert(&conn, "Split", &["One"]);
        let archive_id = insert_concert(&conn, "Archive", &["One"]);

        db::lifecycle::try_mark_download_started(&conn, download_id).unwrap();
        db::lifecycle::mark_download_succeeded(&conn, split_id, "mp4").unwrap();
        db::lifecycle::try_mark_split_started(&conn, split_id).unwrap();
        db::lifecycle::try_mark_archive_started(&conn, archive_id).unwrap();

        let counts = fail_in_progress_jobs(&conn, "server restarted").unwrap();

        assert_eq!(
            counts,
            InProgressFailureCount {
                downloads: 1,
                splits: 1,
                archives: 1
            }
        );
        assert_eq!(
            db::concerts::get_concert(&conn, download_id)
                .unwrap()
                .download_errors[0]
                .error,
            "server restarted"
        );
        assert_eq!(
            db::concerts::get_concert(&conn, split_id)
                .unwrap()
                .split_errors[0]
                .error,
            "server restarted"
        );
        assert_eq!(
            db::concerts::get_concert(&conn, archive_id)
                .unwrap()
                .archive_errors[0]
                .error,
            "server restarted"
        );

        // #127: recovery must also produce transactional Failed Job history,
        // not just the lifecycle error column — one row per kind.
        let failed = db::failed_jobs::list_failed_jobs(&conn, 10).unwrap();
        assert_eq!(failed.len(), 3);
        for id in [download_id, split_id, archive_id] {
            let matching: Vec<_> = failed.iter().filter(|j| j.concert_id == id).collect();
            assert_eq!(matching.len(), 1, "exactly one Failed Job for concert {id}");
            assert_eq!(matching[0].failure_message, "server restarted");
        }
        assert_eq!(
            failed
                .iter()
                .find(|j| j.concert_id == download_id)
                .unwrap()
                .name,
            "download"
        );
        assert_eq!(
            failed
                .iter()
                .find(|j| j.concert_id == split_id)
                .unwrap()
                .name,
            "split"
        );
        assert_eq!(
            failed
                .iter()
                .find(|j| j.concert_id == archive_id)
                .unwrap()
                .name,
            "archive"
        );
    }

    #[test]
    fn recovery_is_transactional_per_kind() {
        // An event-insert failure for one stale kind rolls back that kind's
        // lifecycle/Failed-Job writes without touching the others.
        let conn = db::connection::open_in_memory().unwrap();
        let download_id = insert_concert(&conn, "Download Tx", &["One"]);
        let split_id = insert_concert(&conn, "Split Tx", &["One"]);

        db::lifecycle::try_mark_download_started(&conn, download_id).unwrap();
        db::lifecycle::mark_download_succeeded(&conn, split_id, "mp4").unwrap();
        db::lifecycle::try_mark_split_started(&conn, split_id).unwrap();

        conn.execute_batch(
            "CREATE TRIGGER reject_split_error_event_recovery
             BEFORE INSERT ON events
             WHEN NEW.event = 'split_error'
             BEGIN SELECT RAISE(ABORT, 'reject split error event'); END;",
        )
        .unwrap();

        let error = fail_in_progress_jobs(&conn, "server restarted").unwrap_err();
        assert!(
            error.to_string().contains("split_error"),
            "expected the rejected split_error event in the error chain, got: {error:#}"
        );

        // The split row's terminal write must have rolled back entirely...
        let split_concert = db::concerts::get_concert(&conn, split_id).unwrap();
        assert!(
            split_concert.split_started_at.is_some(),
            "split_started_at must survive the rolled-back transaction"
        );
        assert!(split_concert.split_errors.is_empty());
        assert!(db::failed_jobs::list_failed_jobs(&conn, 10)
            .unwrap()
            .iter()
            .all(|j| j.concert_id != split_id));

        // ...but this test only exercises the split failure path directly:
        // recovery processes download before split, so the download row (which
        // has no trigger) committed successfully before the split failure was
        // hit.
        let download_concert = db::concerts::get_concert(&conn, download_id).unwrap();
        assert!(download_concert.download_started_at.is_none());
        assert_eq!(download_concert.download_errors.len(), 1);
        assert_eq!(
            db::failed_jobs::list_failed_jobs(&conn, 10)
                .unwrap()
                .iter()
                .filter(|j| j.concert_id == download_id)
                .count(),
            1
        );
    }

    #[test]
    fn recovery_does_not_re_fail_an_already_committed_success() {
        // *_started_at is the sole recovery coordination signal (see
        // `run::recover_failed`'s doc comment): a row whose *_started_at is
        // already cleared because the run committed success must not be
        // touched, even if recovery runs concurrently with that commit.
        let conn = db::connection::open_in_memory().unwrap();
        let id = insert_concert(&conn, "Already Archived", &["One"]);
        db::lifecycle::try_mark_download_started(&conn, id).unwrap();
        db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();
        db::lifecycle::try_mark_archive_started(&conn, id).unwrap();
        db::lifecycle::mark_archive_succeeded(&conn, id).unwrap();

        let counts = fail_in_progress_jobs(&conn, "server restarted").unwrap();

        assert_eq!(
            counts,
            InProgressFailureCount {
                downloads: 0,
                splits: 0,
                archives: 0
            }
        );
        let concert = db::concerts::get_concert(&conn, id).unwrap();
        assert!(concert.archived_at.is_some(), "success must be preserved");
        assert!(concert.archive_errors.is_empty());
        assert!(db::failed_jobs::list_failed_jobs(&conn, 10)
            .unwrap()
            .is_empty());
    }
}
