//! Publication seam for complete Concert Split output.

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(test)]
thread_local! {
    static FAIL_COPY_AT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static FAIL_POINT: std::cell::Cell<Option<FailurePoint>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq)]
enum FailurePoint {
    BackupInstall,
    RemoveObsolete,
    ManifestInstall,
    Rollback,
}

#[cfg(test)]
fn maybe_fail(point: FailurePoint) -> Result<()> {
    FAIL_POINT.with(|configured| {
        if configured.get() == Some(point) {
            configured.set(None);
            anyhow::bail!("injected publication operation failure");
        }
        Ok(())
    })
}

pub const MANIFEST_NAME: &str = ".concert-split-published.json";
pub const BACKUP_DIR_NAME: &str = ".concert-split-backup";
const BACKUP_CANDIDATE_NAME: &str = ".concert-split-backup-next";
const BACKUP_OLD_NAME: &str = ".concert-split-backup-old";
const LOCK_NAME: &str = ".concert-split-publication.lock";
const MANIFEST_TEMP_NAME: &str = ".concert-split-published.json.next";
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PublishedManifest {
    files: BTreeSet<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct PublicationRequest {
    pub canonical_dir: PathBuf,
    pub staging_dir: PathBuf,
    pub replacement_files: Vec<PathBuf>,
}

/// Shared guard held while an application consumer opens canonical split media.
pub struct SharedPublicationLock(File);

impl SharedPublicationLock {
    pub fn acquire(canonical_dir: &Path) -> Result<Self> {
        let file = lock_file(canonical_dir)?;
        acquire_with_timeout(|| FileExt::try_lock_shared(&file), "shared")?;
        Ok(Self(file))
    }
}

impl Drop for SharedPublicationLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

fn lock_file(canonical_dir: &Path) -> Result<File> {
    fs::create_dir_all(canonical_dir)?;
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(canonical_dir.join(LOCK_NAME))
        .context("could not open Concert Split publication lock")
}

fn read_manifest(
    canonical_dir: &Path,
    replacement: &BTreeSet<PathBuf>,
) -> Result<PublishedManifest> {
    let path = canonical_dir.join(MANIFEST_NAME);
    match fs::read(&path) {
        Ok(bytes) => {
            let manifest: PublishedManifest = serde_json::from_slice(&bytes)
                .with_context(|| format!("could not parse {}", path.display()))?;
            for relative in &manifest.files {
                validate_relative_filename(relative)?;
            }
            Ok(manifest)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let interlude = regex::Regex::new(r"^interlude_\d{2}\.(mp4|m4a)$")?;
            let mut files: BTreeSet<PathBuf> = replacement
                .iter()
                .filter(|relative| canonical_dir.join(relative).is_file())
                .cloned()
                .collect();
            if canonical_dir.join("timestamps.json").is_file() {
                files.insert(PathBuf::from("timestamps.json"));
            }
            for entry in fs::read_dir(canonical_dir)? {
                let entry = entry?;
                let name = entry.file_name();
                if interlude.is_match(&name.to_string_lossy()) && entry.path().is_file() {
                    files.insert(PathBuf::from(name));
                }
            }
            Ok(PublishedManifest { files })
        }
        Err(error) => Err(error).with_context(|| format!("could not read {}", path.display())),
    }
}

fn validate_replacements(request: &PublicationRequest) -> Result<BTreeSet<PathBuf>> {
    let mut files = BTreeSet::new();
    for relative in &request.replacement_files {
        validate_relative_filename(relative)?;
        anyhow::ensure!(files.insert(relative.clone()), "duplicate output filename");
        let metadata = fs::metadata(request.staging_dir.join(relative))
            .with_context(|| format!("missing staged output {}", relative.display()))?;
        anyhow::ensure!(metadata.is_file(), "staged output is not a file");
        anyhow::ensure!(
            metadata.len() > 0,
            "staged output is empty: {}",
            relative.display()
        );
    }
    Ok(files)
}

fn validate_relative_filename(path: &Path) -> Result<()> {
    anyhow::ensure!(
        path.components().count() == 1 && path.file_name().is_some(),
        "Concert Split output must be a filename: {}",
        path.display()
    );
    Ok(())
}

fn acquire_with_timeout(
    mut try_lock: impl FnMut() -> std::io::Result<()>,
    mode: &'static str,
) -> Result<()> {
    let started = Instant::now();
    loop {
        match try_lock() {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if started.elapsed() >= LOCK_TIMEOUT {
                    anyhow::bail!("timed out acquiring {mode} Concert Split publication lock");
                }
                thread::sleep(LOCK_POLL_INTERVAL);
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn copy_synced(source: &Path, destination: &Path) -> Result<()> {
    #[cfg(test)]
    FAIL_COPY_AT.with(|remaining| {
        if let Some(value) = remaining.get() {
            if value == 0 {
                remaining.set(None);
                anyhow::bail!("injected publication copy failure");
            }
            remaining.set(Some(value - 1));
        }
        Ok::<(), anyhow::Error>(())
    })?;
    fs::copy(source, destination).with_context(|| {
        format!(
            "could not copy {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    File::open(destination)?.sync_all()?;
    Ok(())
}

fn restore_prior(
    canonical: &Path,
    backup: &Path,
    prior: &PublishedManifest,
    replacement: &BTreeSet<PathBuf>,
) -> Result<()> {
    #[cfg(test)]
    maybe_fail(FailurePoint::Rollback)?;
    for relative in replacement.difference(&prior.files) {
        let path = canonical.join(relative);
        if path.exists() {
            fs::remove_file(&path)?;
        }
    }
    for relative in &prior.files {
        copy_synced(&backup.join(relative), &canonical.join(relative))?;
    }
    Ok(())
}

pub fn publish(request: &PublicationRequest) -> Result<()> {
    let replacement = validate_replacements(request)?;
    fs::create_dir_all(&request.canonical_dir)?;
    let lock = lock_file(&request.canonical_dir)?;
    acquire_with_timeout(|| FileExt::try_lock_exclusive(&lock), "exclusive")?;

    let prior = read_manifest(&request.canonical_dir, &replacement)?;
    let backup = request.canonical_dir.join(BACKUP_DIR_NAME);
    let backup_candidate = request.canonical_dir.join(BACKUP_CANDIDATE_NAME);
    if backup_candidate.exists() {
        fs::remove_dir_all(&backup_candidate)?;
    }
    if !prior.files.is_empty() {
        fs::create_dir(&backup_candidate)?;
        for relative in &prior.files {
            let metadata = fs::metadata(request.canonical_dir.join(relative))?;
            anyhow::ensure!(
                metadata.is_file() && metadata.len() > 0,
                "prior Published Concert Split file is missing or empty: {}",
                relative.display()
            );
            copy_synced(
                &request.canonical_dir.join(relative),
                &backup_candidate.join(relative),
            )?;
        }
        let old_backup = request.canonical_dir.join(BACKUP_OLD_NAME);
        if old_backup.exists() {
            fs::remove_dir_all(&old_backup)?;
        }
        if backup.exists() {
            fs::rename(&backup, &old_backup)?;
        }
        #[cfg(test)]
        maybe_fail(FailurePoint::BackupInstall)?;
        if let Err(error) = fs::rename(&backup_candidate, &backup) {
            if old_backup.exists() {
                fs::rename(&old_backup, &backup)?;
            }
            return Err(error).context("could not install Concert Split backup");
        }
        if old_backup.exists() {
            fs::remove_dir_all(old_backup)?;
        }
    }

    let result = (|| -> Result<()> {
        for relative in &replacement {
            let temporary = request.canonical_dir.join(format!(
                ".concert-split-copy-{}",
                relative.to_string_lossy()
            ));
            copy_synced(&request.staging_dir.join(relative), &temporary)?;
            fs::rename(&temporary, request.canonical_dir.join(relative))?;
        }
        for relative in prior.files.difference(&replacement) {
            #[cfg(test)]
            maybe_fail(FailurePoint::RemoveObsolete)?;
            let path = request.canonical_dir.join(relative);
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
        let manifest = PublishedManifest {
            files: replacement.clone(),
        };
        let manifest_temp = request.canonical_dir.join(MANIFEST_TEMP_NAME);
        let mut file = File::create(&manifest_temp)?;
        file.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
        file.sync_all()?;
        #[cfg(test)]
        maybe_fail(FailurePoint::ManifestInstall)?;
        fs::rename(manifest_temp, request.canonical_dir.join(MANIFEST_NAME))?;
        Ok(())
    })();

    if let Err(publication_error) = result {
        restore_prior(&request.canonical_dir, &backup, &prior, &replacement).with_context(
            || format!("publication failed ({publication_error:#}) and rollback also failed"),
        )?;
        return Err(publication_error);
    }
    Ok(())
}

pub fn with_shared_lock<T>(
    canonical_dir: &Path,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let _lock = SharedPublicationLock::acquire(canonical_dir)?;
    operation()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }

    fn fail_copy_after(successful_copies: usize) {
        FAIL_COPY_AT.with(|remaining| remaining.set(Some(successful_copies)));
    }

    fn fail_at(point: FailurePoint) {
        FAIL_POINT.with(|configured| configured.set(Some(point)));
    }

    #[test]
    fn first_publication_copies_complete_set_and_writes_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("concert");
        let staging = tmp.path().join("staging");
        write(&staging.join("First.m4a"), b"first");
        write(&staging.join("Second.m4a"), b"second");

        publish(&PublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: staging,
            replacement_files: vec![PathBuf::from("First.m4a"), PathBuf::from("Second.m4a")],
        })
        .unwrap();

        assert_eq!(fs::read(canonical.join("First.m4a")).unwrap(), b"first");
        assert_eq!(fs::read(canonical.join("Second.m4a")).unwrap(), b"second");
        let manifest = fs::read_to_string(canonical.join(MANIFEST_NAME)).unwrap();
        assert!(manifest.contains("First.m4a"));
        assert!(manifest.contains("Second.m4a"));
        assert!(!canonical.join(BACKUP_DIR_NAME).exists());
    }

    #[test]
    fn replacement_retains_one_backup_and_removes_obsolete_owned_file() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("concert");
        let first = tmp.path().join("first");
        write(&first.join("Old.m4a"), b"old");
        write(&first.join("Gone.m4a"), b"gone");
        publish(&PublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: first,
            replacement_files: vec![PathBuf::from("Old.m4a"), PathBuf::from("Gone.m4a")],
        })
        .unwrap();
        write(&canonical.join("preview.jpg"), b"preview");

        let second = tmp.path().join("second");
        write(&second.join("Old.m4a"), b"new");
        publish(&PublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: second,
            replacement_files: vec![PathBuf::from("Old.m4a")],
        })
        .unwrap();

        assert_eq!(fs::read(canonical.join("Old.m4a")).unwrap(), b"new");
        assert!(!canonical.join("Gone.m4a").exists());
        assert_eq!(fs::read(canonical.join("preview.jpg")).unwrap(), b"preview");
        assert_eq!(
            fs::read(canonical.join(BACKUP_DIR_NAME).join("Old.m4a")).unwrap(),
            b"old"
        );
        assert_eq!(
            fs::read(canonical.join(BACKUP_DIR_NAME).join("Gone.m4a")).unwrap(),
            b"gone"
        );
    }

    #[test]
    fn empty_replacement_is_rejected_before_canonical_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("concert");
        let staging = tmp.path().join("staging");
        write(&canonical.join("Song.m4a"), b"known-good");
        write(&staging.join("Song.m4a"), b"");

        let error = publish(&PublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: staging,
            replacement_files: vec![PathBuf::from("Song.m4a")],
        })
        .unwrap_err();

        assert!(error.to_string().contains("empty"));
        assert_eq!(fs::read(canonical.join("Song.m4a")).unwrap(), b"known-good");
    }

    #[test]
    fn first_manifest_adopts_only_exact_legacy_outputs() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("concert");
        let staging = tmp.path().join("staging");
        write(&canonical.join("Song.m4a"), b"legacy");
        write(&canonical.join("unrelated.m4a"), b"unrelated");
        write(&canonical.join("interlude_01.m4a"), b"gap");
        write(&staging.join("Song.m4a"), b"replacement");

        publish(&PublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: staging,
            replacement_files: vec![PathBuf::from("Song.m4a")],
        })
        .unwrap();

        assert_eq!(
            fs::read(canonical.join("Song.m4a")).unwrap(),
            b"replacement"
        );
        assert!(!canonical.join("interlude_01.m4a").exists());
        assert_eq!(
            fs::read(canonical.join("unrelated.m4a")).unwrap(),
            b"unrelated"
        );
        assert_eq!(
            fs::read(canonical.join(BACKUP_DIR_NAME).join("Song.m4a")).unwrap(),
            b"legacy"
        );
    }

    #[test]
    fn replacement_copy_failure_restores_previous_published_split() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("concert");
        let first = tmp.path().join("first");
        write(&first.join("Song.m4a"), b"known-good");
        publish(&PublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: first,
            replacement_files: vec![PathBuf::from("Song.m4a")],
        })
        .unwrap();
        let second = tmp.path().join("second");
        write(&second.join("Song.m4a"), b"replacement");

        // One backup copy succeeds, then the canonical replacement copy fails.
        fail_copy_after(1);
        let error = publish(&PublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: second,
            replacement_files: vec![PathBuf::from("Song.m4a")],
        })
        .unwrap_err();

        assert!(error.to_string().contains("injected"));
        assert_eq!(fs::read(canonical.join("Song.m4a")).unwrap(), b"known-good");
    }

    #[test]
    fn shared_reader_waits_for_exclusive_publisher() {
        use std::sync::mpsc;

        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("concert");
        let publisher = lock_file(&canonical).unwrap();
        FileExt::lock_exclusive(&publisher).unwrap();
        let (sent, received) = mpsc::channel();
        let reader_dir = canonical.clone();
        let reader = std::thread::spawn(move || {
            let _guard = SharedPublicationLock::acquire(&reader_dir).unwrap();
            sent.send(()).unwrap();
        });
        assert!(received.recv_timeout(Duration::from_millis(100)).is_err());
        FileExt::unlock(&publisher).unwrap();
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        reader.join().unwrap();
    }

    #[test]
    fn manifest_install_failure_rolls_back_media_and_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("concert");
        let first = tmp.path().join("first");
        write(&first.join("Song.m4a"), b"old");
        publish(&PublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: first,
            replacement_files: vec![PathBuf::from("Song.m4a")],
        })
        .unwrap();
        let prior_manifest = fs::read(canonical.join(MANIFEST_NAME)).unwrap();
        let second = tmp.path().join("second");
        write(&second.join("Song.m4a"), b"new");
        fail_at(FailurePoint::ManifestInstall);
        assert!(publish(&PublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: second,
            replacement_files: vec![PathBuf::from("Song.m4a")]
        })
        .is_err());
        assert_eq!(fs::read(canonical.join("Song.m4a")).unwrap(), b"old");
        assert_eq!(
            fs::read(canonical.join(MANIFEST_NAME)).unwrap(),
            prior_manifest
        );
    }

    #[test]
    fn obsolete_removal_failure_restores_previous_set() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("concert");
        let first = tmp.path().join("first");
        write(&first.join("Keep.m4a"), b"old");
        write(&first.join("Obsolete.m4a"), b"obsolete");
        publish(&PublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: first,
            replacement_files: vec![PathBuf::from("Keep.m4a"), PathBuf::from("Obsolete.m4a")],
        })
        .unwrap();
        let second = tmp.path().join("second");
        write(&second.join("Keep.m4a"), b"new");
        fail_at(FailurePoint::RemoveObsolete);
        assert!(publish(&PublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: second,
            replacement_files: vec![PathBuf::from("Keep.m4a")]
        })
        .is_err());
        assert_eq!(fs::read(canonical.join("Keep.m4a")).unwrap(), b"old");
        assert_eq!(
            fs::read(canonical.join("Obsolete.m4a")).unwrap(),
            b"obsolete"
        );
    }
}
