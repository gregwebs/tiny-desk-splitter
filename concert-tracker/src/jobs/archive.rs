use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::db;
use crate::jobs::{JobKey, JobKind, JobRegistry};
use crate::model::{concert_dir, sanitize_album};

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

pub async fn start_archive(
    db: Arc<Mutex<Connection>>,
    registry: Arc<JobRegistry>,
    working_dir: &Path,
    archive_location: &str,
    concert_id: i64,
) -> Result<StartOutcome> {
    let key = JobKey {
        concert_id,
        kind: JobKind::Archive,
    };
    if registry.is_running(&key) {
        return Ok(StartOutcome::AlreadyRunning);
    }

    let (album, title) = {
        let conn = db.lock().unwrap();
        let concert = db::get_concert(&conn, concert_id)?;
        if concert.downloaded_at.is_none() && concert.split_at.is_none() {
            return Ok(StartOutcome::NothingToArchive);
        }
        let album = concert
            .album
            .ok_or_else(|| anyhow::anyhow!("concert {} has no album", concert_id))?;
        (album, concert.title)
    };

    {
        let conn = db.lock().unwrap();
        if !db::try_mark_archive_started(&conn, concert_id)? {
            tracing::info!("archive already running for concert {}", concert_id);
            return Ok(StartOutcome::AlreadyRunning);
        }
    }

    let source_dir = concert_dir(working_dir, &album);
    let dest_dir = Path::new(archive_location).join(sanitize_album(&album));

    tracing::info!(
        "archive started for concert {} ({}) -> {}",
        concert_id,
        title,
        dest_dir.display()
    );

    let job = ArchiveJob {
        concert_id,
        source_dir,
        dest_dir,
    };

    let handle = tokio::task::spawn(run_archive(db.clone(), job));
    registry.insert(key, handle);

    Ok(StartOutcome::Spawned)
}

async fn run_archive(db: Arc<Mutex<Connection>>, job: ArchiveJob) {
    let concert_id = job.concert_id;
    match tokio::task::spawn_blocking(move || do_archive(&job)).await {
        Ok(Ok(())) => {
            tracing::info!("archive completed for concert {}", concert_id);
            let conn = db.lock().unwrap();
            let _ = db::mark_archive_succeeded(&conn, concert_id);
        }
        Ok(Err(e)) => {
            let error = format!("{:#}", e);
            tracing::warn!("archive failed for concert {}: {}", concert_id, error);
            let conn = db.lock().unwrap();
            let _ = db::mark_archive_failed(&conn, concert_id, &error);
            let _ = db::insert_failed_job(&conn, concert_id, "archive", &error);
        }
        Err(e) => {
            let error = format!("task panicked: {}", e);
            tracing::warn!("archive failed for concert {}: {}", concert_id, error);
            let conn = db.lock().unwrap();
            let _ = db::mark_archive_failed(&conn, concert_id, &error);
            let _ = db::insert_failed_job(&conn, concert_id, "archive", &error);
        }
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
        anyhow::bail!(
            "archive directory does not exist: {}",
            dest_dir.display()
        );
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
        let source = tmp.path().join("concerts").join("Bloc Party Tiny Desk Concert");
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
}
