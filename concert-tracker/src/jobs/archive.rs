use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::db;
use crate::jobs::run::{self, Admission, JobCancellation, JobRequest};
use crate::jobs::{JobKey, JobKind, JobRegistry, JobRunFuture, JobStepFailure, JobStepOutcome};
use crate::model::{concert_dir, sanitize_album};

#[derive(Clone)]
pub struct ArchiveJob {
    pub concert_id: i64,
    pub source_dir: PathBuf,
    pub dest_dir: PathBuf,
}

pub enum StartOutcome {
    Spawned,
    AlreadyRunning,
    NothingToArchive,
}

#[derive(Debug)]
struct ArchiveValidationError;

impl std::fmt::Display for ArchiveValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Concert has neither a download nor a split to archive")
    }
}

impl std::error::Error for ArchiveValidationError {}

/// Start an archive job for the given concert. Goes through the Job Run engine
/// (`jobs::run`): race-safe admission, exactly one terminal outcome, and
/// Failed Job history on any unsuccessful outcome including cancellation. See
/// `docs/jobs.md`.
pub async fn start_archive(
    db: Arc<Mutex<Connection>>,
    registry: Arc<JobRegistry>,
    working_dir: &Path,
    archive_location: &str,
    concert_id: i64,
) -> Result<StartOutcome> {
    let request = ArchiveRequest::new(
        concert_id,
        working_dir.to_path_buf(),
        archive_location.to_string(),
    );
    match run::submit(db, registry, request).await {
        Ok(Admission::Accepted) => Ok(StartOutcome::Spawned),
        Ok(Admission::AlreadyRunning) => Ok(StartOutcome::AlreadyRunning),
        Err(error) if error.downcast_ref::<ArchiveValidationError>().is_some() => {
            Ok(StartOutcome::NothingToArchive)
        }
        Err(error) => Err(error),
    }
}

/// The archive [`JobRequest`]. `Setup` is the identity of `Input`
/// (`ArchiveJob`) — archive has no separate post-acceptance preparation step,
/// matching download.
///
/// `pub(crate)` so `crate::lifecycle::cancel_job` can build one to route a
/// user-initiated cancellation through [`run::cancel`].
pub(crate) struct ArchiveRequest {
    concert_id: i64,
    working_dir: PathBuf,
    archive_location: String,
}

impl ArchiveRequest {
    pub(crate) fn new(concert_id: i64, working_dir: PathBuf, archive_location: String) -> Self {
        ArchiveRequest {
            concert_id,
            working_dir,
            archive_location,
        }
    }
}

pub(crate) struct ArchiveCancellation {
    concert_id: i64,
}

impl ArchiveCancellation {
    pub(crate) fn new(concert_id: i64) -> Self {
        Self { concert_id }
    }
}

impl JobCancellation for ArchiveCancellation {
    fn key(&self) -> JobKey {
        JobKey {
            concert_id: self.concert_id,
            kind: JobKind::Archive,
        }
    }

    fn job_name(&self) -> &'static str {
        "archive"
    }

    fn record_failure(&self, conn: &Connection, error: &str) -> Result<()> {
        db::lifecycle::mark_archive_failed(conn, self.concert_id, error)
    }

    fn has_stale_in_progress(&self, conn: &Connection) -> Result<bool> {
        Ok(conn.query_row(
            "SELECT archive_started_at IS NOT NULL FROM concerts WHERE id = ?1",
            [self.concert_id],
            |row| row.get(0),
        )?)
    }
}

impl JobCancellation for ArchiveRequest {
    fn key(&self) -> JobKey {
        JobKey {
            concert_id: self.concert_id,
            kind: JobKind::Archive,
        }
    }

    fn job_name(&self) -> &'static str {
        "archive"
    }

    fn record_failure(&self, conn: &Connection, error: &str) -> Result<()> {
        db::lifecycle::mark_archive_failed(conn, self.concert_id, error)
    }

    fn has_stale_in_progress(&self, conn: &Connection) -> Result<bool> {
        Ok(conn.query_row(
            "SELECT archive_started_at IS NOT NULL FROM concerts WHERE id = ?1",
            [self.concert_id],
            |row| row.get(0),
        )?)
    }
}

impl JobRequest for ArchiveRequest {
    type Input = ArchiveJob;
    type Setup = ArchiveJob;
    type Facts = ();

    fn validate(&self, conn: &Connection) -> Result<ArchiveJob> {
        let concert = db::concerts::get_concert(conn, self.concert_id)?;
        if concert.downloaded_at.is_none() && concert.split_at.is_none() {
            return Err(ArchiveValidationError.into());
        }
        let album = concert
            .album
            .ok_or_else(|| anyhow::anyhow!("concert {} has no album", self.concert_id))?;
        let source_dir = concert_dir(&self.working_dir, &album);
        let dest_dir = Path::new(&self.archive_location).join(sanitize_album(&album));
        Ok(ArchiveJob {
            concert_id: self.concert_id,
            source_dir,
            dest_dir,
        })
    }

    fn try_mark_started(&self, conn: &Connection) -> Result<bool> {
        db::lifecycle::try_mark_archive_started(conn, self.concert_id)
    }

    fn setup(&self, input: ArchiveJob) -> Result<ArchiveJob> {
        Ok(input)
    }

    fn execute<'a>(
        &'a self,
        setup: &'a ArchiveJob,
        _log_file: Option<&'a Path>,
    ) -> JobRunFuture<'a, JobStepOutcome> {
        // do_archive is real (blocking) filesystem work: rename-or-copy plus a
        // symlink. Run it on a blocking thread, same as the pre-#127 code, and
        // keep that behavior — and its safety checks — owned entirely here.
        let job = setup.clone();
        Box::pin(async move {
            match tokio::task::spawn_blocking(move || do_archive(&job)).await {
                Ok(Ok(())) => JobStepOutcome::Succeeded,
                Ok(Err(e)) => JobStepOutcome::Failed(JobStepFailure::ordinary(format!("{:#}", e))),
                Err(e) => JobStepOutcome::Failed(JobStepFailure::ordinary(format!(
                    "task panicked: {}",
                    e
                ))),
            }
        })
    }

    fn gather_success_facts(&self, _setup: &ArchiveJob) -> Result<()> {
        Ok(())
    }

    fn commit_success(&self, conn: &Connection, _facts: ()) -> Result<()> {
        db::lifecycle::mark_archive_succeeded(conn, self.concert_id)
    }
}

fn do_archive(job: &ArchiveJob) -> anyhow::Result<()> {
    if !job.source_dir.exists() {
        anyhow::bail!(
            "source directory does not exist: {}",
            job.source_dir.display()
        );
    }

    if let Some(parent) = job.dest_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }

    tracing::debug!(
        "attempting rename {} -> {}",
        job.source_dir.display(),
        job.dest_dir.display()
    );

    match std::fs::rename(&job.source_dir, &job.dest_dir) {
        Ok(()) => {
            tracing::debug!("rename succeeded (same filesystem)");
        }
        Err(e) if is_cross_device(&e) => {
            tracing::debug!("cross-device move, falling back to copy+delete");
            copy_dir_recursive(&job.source_dir, &job.dest_dir)?;
            std::fs::remove_dir_all(&job.source_dir)?;
        }
        Err(e) => return Err(e.into()),
    }

    #[cfg(unix)]
    {
        tracing::debug!(
            "creating symlink {} -> {}",
            job.source_dir.display(),
            job.dest_dir.display()
        );
        std::os::unix::fs::symlink(&job.dest_dir, &job.source_dir)?;
    }

    Ok(())
}

/// Reverse `do_archive`. The symlink at `source_dir` is the authoritative
/// record of where the files went — read it (don't recompute from current
/// settings), then move the dest back over the symlink. Recomputing was
/// brittle: settings.archive_location or sanitize_album can have drifted
/// since archiving (observed in the wild: archive at
/// `/nas/.../Bloc Party - Tiny Desk Concert` vs. recomputed
/// `/nas/.../Bloc Party Tiny Desk Concert`).
///
/// The rename happy path covers same-filesystem moves; the EXDEV branch
/// mirrors `do_archive`'s and is exercised manually rather than in unit
/// tests.
pub fn do_unarchive(source_dir: &Path) -> anyhow::Result<()> {
    let source_meta = std::fs::symlink_metadata(source_dir).ok();
    let dest_dir = match source_meta {
        Some(meta) if meta.file_type().is_symlink() => std::fs::read_link(source_dir)
            .with_context(|| {
                format!("failed to read archive symlink at {}", source_dir.display())
            })?,
        Some(_) => anyhow::bail!(
            "source path is a real directory, refusing to clobber: {}",
            source_dir.display()
        ),
        None => anyhow::bail!(
            "no archive symlink at {}, cannot determine archive location",
            source_dir.display()
        ),
    };

    if !dest_dir.exists() {
        anyhow::bail!("archive directory does not exist: {}", dest_dir.display());
    }

    tracing::debug!("removing archive symlink {}", source_dir.display());
    std::fs::remove_file(source_dir)?;

    if let Some(parent) = source_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }

    tracing::debug!(
        "attempting rename {} -> {}",
        dest_dir.display(),
        source_dir.display()
    );

    match std::fs::rename(&dest_dir, source_dir) {
        Ok(()) => {
            tracing::debug!("rename succeeded (same filesystem)");
        }
        Err(e) if is_cross_device(&e) => {
            tracing::debug!("cross-device move, falling back to copy+delete");
            copy_dir_recursive(&dest_dir, source_dir)?;
            std::fs::remove_dir_all(&dest_dir)?;
        }
        Err(e) => return Err(e.into()),
    }

    Ok(())
}

fn is_cross_device(e: &std::io::Error) -> bool {
    // EXDEV is 18 on macOS and Linux
    e.raw_os_error() == Some(18)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dest_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            std::fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::JobRegistry;

    #[test]
    fn do_archive_moves_and_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("concerts").join("Test Album");
        let dest = tmp.path().join("archive").join("Test Album");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("test.mp4"), b"data").unwrap();

        let job = ArchiveJob {
            concert_id: 1,
            source_dir: source.clone(),
            dest_dir: dest.clone(),
        };

        do_archive(&job).unwrap();

        assert!(dest.join("test.mp4").exists());
        assert!(source.is_symlink());
        assert_eq!(std::fs::read_link(&source).unwrap(), dest);
        assert_eq!(
            std::fs::read_to_string(source.join("test.mp4")).unwrap(),
            "data"
        );
    }

    #[test]
    fn do_archive_fails_if_source_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let job = ArchiveJob {
            concert_id: 1,
            source_dir: tmp.path().join("nope"),
            dest_dir: tmp.path().join("archive"),
        };
        assert!(do_archive(&job).is_err());
    }

    #[test]
    fn do_unarchive_restores_files_and_removes_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("concerts").join("Test Album");
        let dest = tmp.path().join("archive").join("Test Album");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("test.mp4"), b"data").unwrap();

        do_archive(&ArchiveJob {
            concert_id: 1,
            source_dir: source.clone(),
            dest_dir: dest.clone(),
        })
        .unwrap();

        do_unarchive(&source).unwrap();

        assert!(source.is_dir());
        assert!(!source.is_symlink());
        assert!(!dest.exists());
        assert_eq!(
            std::fs::read_to_string(source.join("test.mp4")).unwrap(),
            "data"
        );
    }

    #[test]
    fn do_unarchive_follows_symlink_with_drifted_name() {
        // Simulates the wild case: sanitize_album drift means the recomputed
        // dest path would not match, but the symlink records the real one.
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp
            .path()
            .join("concerts")
            .join("Bloc Party Tiny Desk Concert");
        let real_dest = tmp
            .path()
            .join("archive")
            .join("Bloc Party - Tiny Desk Concert");
        std::fs::create_dir_all(&real_dest).unwrap();
        std::fs::write(real_dest.join("test.mp4"), b"data").unwrap();
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real_dest, &source).unwrap();

        do_unarchive(&source).unwrap();

        assert!(source.is_dir());
        assert!(!source.is_symlink());
        assert!(!real_dest.exists());
        assert_eq!(
            std::fs::read_to_string(source.join("test.mp4")).unwrap(),
            "data"
        );
    }

    #[test]
    fn do_unarchive_fails_if_dest_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("concerts").join("Test Album");
        let dest = tmp.path().join("archive").join("Test Album");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&dest, &source).unwrap();

        assert!(do_unarchive(&source).is_err());
    }

    #[test]
    fn do_unarchive_fails_if_source_is_real_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("concerts").join("Test Album");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("user-data.txt"), b"keep me").unwrap();

        let err = do_unarchive(&source).unwrap_err().to_string();
        assert!(
            err.contains("real directory"),
            "expected clobber-refusal error, got: {err}"
        );
        assert!(
            source.join("user-data.txt").exists(),
            "source must not be touched"
        );
    }

    #[test]
    fn do_unarchive_fails_if_source_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("concerts").join("Test Album");
        let err = do_unarchive(&source).unwrap_err().to_string();
        assert!(
            err.contains("no archive symlink"),
            "expected missing-symlink error, got: {err}"
        );
    }

    #[test]
    fn copy_dir_recursive_copies_nested() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.txt"), b"hello").unwrap();
        std::fs::write(src.join("sub").join("b.txt"), b"world").unwrap();

        copy_dir_recursive(&src, &dst).unwrap();

        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "hello");
        assert_eq!(
            std::fs::read_to_string(dst.join("sub").join("b.txt")).unwrap(),
            "world"
        );
    }

    // ── Job Run engine migration (#127) ─────────────────────────────────────

    fn seeded_db(album: &str, downloaded: bool, split: bool) -> (Arc<Mutex<Connection>>, i64) {
        let conn = db::connection::open_in_memory().unwrap();
        let id = db::seeds::SeedContext::new(&conn)
            .seed_scraped_concert(db::seeds::SeedScrapedConcert {
                source_url: Some(format!("https://npr.org/c/{}", album)),
                title: Some(album.to_string()),
                concert_date: None,
                artist: Some("Test Artist".to_string()),
                album: Some(album.to_string()),
                set_list: Some(vec!["Song".to_string()]),
            })
            .unwrap()
            .id;
        if downloaded {
            db::lifecycle::try_mark_download_started(&conn, id).unwrap();
            db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();
        }
        if split {
            db::lifecycle::try_mark_split_started(&conn, id).unwrap();
            db::lifecycle::mark_split_succeeded(&conn, id).unwrap();
        }
        (Arc::new(Mutex::new(conn)), id)
    }

    async fn wait_until_finished(registry: &JobRegistry, concert_id: i64) {
        let key = JobKey {
            concert_id,
            kind: JobKind::Archive,
        };
        for _ in 0..100 {
            if !registry.is_running(&key) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("archive job did not finish");
    }

    fn failed_job_count(conn: &Connection, id: i64) -> usize {
        db::failed_jobs::list_failed_jobs(conn, 100)
            .unwrap()
            .into_iter()
            .filter(|j| j.concert_id == id)
            .count()
    }

    #[tokio::test]
    async fn nothing_to_archive_is_rejected_without_archive_history() {
        let tmp = tempfile::tempdir().unwrap();
        let (db, id) = seeded_db("Nothing To Archive", false, false);
        let registry = Arc::new(JobRegistry::new());

        let outcome = start_archive(db.clone(), registry.clone(), tmp.path(), "/archive", id)
            .await
            .unwrap();

        assert!(matches!(outcome, StartOutcome::NothingToArchive));
        let conn = db.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, id).unwrap();
        assert!(concert.archive_started_at.is_none());
        assert!(concert.archive_errors.is_empty());
        assert_eq!(failed_job_count(&conn, id), 0);
        assert!(!crate::events::list_for_concert(&conn, id)
            .iter()
            .any(|event| matches!(event.event.as_str(), "archive_started" | "archive_error")));
    }

    #[tokio::test]
    async fn concurrent_start_archive_accepts_exactly_one() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Concurrent Archive";
        let cd = concert_dir(tmp.path(), album);
        std::fs::create_dir_all(&cd).unwrap();
        let (db, id) = seeded_db(album, true, false);
        let registry = Arc::new(JobRegistry::new());
        let archive_dir = tmp.path().join("archive");

        let first = start_archive(
            db.clone(),
            registry.clone(),
            tmp.path(),
            archive_dir.to_str().unwrap(),
            id,
        )
        .await
        .unwrap();
        let second = start_archive(
            db.clone(),
            registry.clone(),
            tmp.path(),
            archive_dir.to_str().unwrap(),
            id,
        )
        .await
        .unwrap();

        assert!(matches!(first, StartOutcome::Spawned));
        assert!(matches!(second, StartOutcome::AlreadyRunning));
        wait_until_finished(&registry, id).await;
        let conn = db.lock().unwrap();
        let started_events = crate::events::list_for_concert(&conn, id)
            .into_iter()
            .filter(|e| e.event == "archive_started")
            .count();
        assert_eq!(started_events, 1, "exactly one started transition/event");
    }

    #[tokio::test]
    async fn successful_archive_sets_archived_at_and_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Successful Archive";
        let cd = concert_dir(tmp.path(), album);
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(cd.join("track.m4a"), b"audio").unwrap();
        let (db, id) = seeded_db(album, true, true);
        let registry = Arc::new(JobRegistry::new());
        let archive_dir = tmp.path().join("archive");

        let outcome = start_archive(
            db.clone(),
            registry.clone(),
            tmp.path(),
            archive_dir.to_str().unwrap(),
            id,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, StartOutcome::Spawned));
        wait_until_finished(&registry, id).await;

        let conn = db.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, id).unwrap();
        assert!(concert.archived_at.is_some());
        assert!(concert.archive_started_at.is_none());
        assert!(concert.archive_errors.is_empty());
        assert_eq!(failed_job_count(&conn, id), 0);
        assert!(cd.is_symlink());
        assert!(
            archive_dir
                .join(sanitize_album(album))
                .join("track.m4a")
                .exists(),
            "archived file must exist at the destination"
        );
        assert_eq!(
            crate::events::list_for_concert(&conn, id)
                .last()
                .unwrap()
                .event,
            "archived"
        );
    }

    #[tokio::test]
    async fn execution_failure_produces_failed_terminal_and_failed_job() {
        // Album directory never created on disk — do_archive's source-missing
        // check fails inside execute, after acceptance.
        let (db, id) = seeded_db("Missing Source Dir", true, false);
        let registry = Arc::new(JobRegistry::new());

        let outcome = start_archive(
            db.clone(),
            registry.clone(),
            Path::new("/nonexistent-working-dir-for-test"),
            "/archive-dest-for-test",
            id,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, StartOutcome::Spawned));
        wait_until_finished(&registry, id).await;

        let conn = db.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, id).unwrap();
        assert!(concert.archived_at.is_none());
        assert!(concert.archive_started_at.is_none());
        assert!(!concert.archive_errors.is_empty());
        assert!(concert
            .archive_errors
            .last()
            .unwrap()
            .error
            .contains("source directory"));
        assert_eq!(failed_job_count(&conn, id), 1);
        assert_eq!(
            db::failed_jobs::list_failed_jobs(&conn, 10).unwrap()[0].name,
            "archive"
        );
    }

    #[tokio::test]
    async fn success_persistence_failure_produces_failed_terminal_not_success() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Persistence Failure";
        let cd = concert_dir(tmp.path(), album);
        std::fs::create_dir_all(&cd).unwrap();
        let (db, id) = seeded_db(album, true, false);
        {
            let conn = db.lock().unwrap();
            conn.execute_batch(
                "CREATE TRIGGER reject_archived_event BEFORE INSERT ON events
                 WHEN NEW.event = 'archived'
                 BEGIN SELECT RAISE(ABORT, 'rejected terminal event'); END;",
            )
            .unwrap();
        }
        let registry = Arc::new(JobRegistry::new());
        let archive_dir = tmp.path().join("archive");

        let outcome = start_archive(
            db.clone(),
            registry.clone(),
            tmp.path(),
            archive_dir.to_str().unwrap(),
            id,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, StartOutcome::Spawned));
        wait_until_finished(&registry, id).await;

        let conn = db.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, id).unwrap();
        assert!(
            concert.archived_at.is_none(),
            "success must not be visible when persistence failed"
        );
        assert_eq!(failed_job_count(&conn, id), 1);
    }
}
