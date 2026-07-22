//! Publication seam for complete Concert Split output.

use crate::io;
use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct PublishedSplitExists;

impl std::fmt::Display for PublishedSplitExists {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a Published Concert Split already exists")
    }
}

impl std::error::Error for PublishedSplitExists {}

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
    PartialManifestInstall,
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
pub const PARTIAL_MANIFEST_NAME: &str = ".concert-split-partial.json";
pub const BACKUP_DIR_NAME: &str = ".concert-split-backup";
const BACKUP_CANDIDATE_NAME: &str = ".concert-split-backup-next";
const BACKUP_OLD_NAME: &str = ".concert-split-backup-old";
const LOCK_NAME: &str = ".concert-split-publication.lock";
const MANIFEST_TEMP_NAME: &str = ".concert-split-published.json.next";
const PARTIAL_MANIFEST_TEMP_NAME: &str = ".concert-split-partial.json.next";
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

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PartialTrackOutput {
    pub title: String,
    pub start_time: f64,
    pub end_time: f64,
    pub files: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct PartialPublicationRequest {
    pub canonical_dir: PathBuf,
    pub staging_dir: PathBuf,
    pub expected_tracks: Vec<PartialExpectedTrack>,
    pub completed_tracks: Vec<PartialTrackOutput>,
}

#[derive(Clone, Debug)]
pub struct PartialExpectedTrack {
    pub title: String,
    pub files: Vec<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PartialManifest {
    tracks: Vec<PartialTrackOutput>,
}

fn read_partial_manifest(canonical_dir: &Path) -> Result<Option<PartialManifest>> {
    let path = canonical_dir.join(PARTIAL_MANIFEST_NAME);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()))
        }
    };
    let manifest: PartialManifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("could not parse {}", path.display()))?;
    validate_partial_manifest(canonical_dir, &manifest, None)?;
    Ok(Some(manifest))
}

fn validate_partial_manifest(
    canonical_dir: &Path,
    manifest: &PartialManifest,
    expected_tracks: Option<&BTreeMap<String, BTreeSet<PathBuf>>>,
) -> Result<()> {
    anyhow::ensure!(
        !manifest.tracks.is_empty(),
        "partial manifest has no tracks"
    );
    let mut titles = BTreeSet::new();
    let mut files = BTreeSet::new();
    for track in &manifest.tracks {
        anyhow::ensure!(!track.title.is_empty(), "partial track title is empty");
        anyhow::ensure!(
            titles.insert(track.title.clone()),
            "duplicate partial track title"
        );
        if let Some(expected) = expected_tracks {
            anyhow::ensure!(
                expected.contains_key(&track.title),
                "unknown partial track title"
            );
        }
        anyhow::ensure!(
            track.start_time.is_finite()
                && track.end_time.is_finite()
                && track.start_time < track.end_time,
            "partial track timing is invalid"
        );
        anyhow::ensure!(!track.files.is_empty(), "partial track has no files");
        let expected_stem = io::sanitize_filename(&track.title);
        anyhow::ensure!(
            !expected_stem.is_empty(),
            "partial track title has no filename"
        );
        for relative in &track.files {
            anyhow::ensure!(
                relative.file_stem().and_then(|stem| stem.to_str()) == Some(expected_stem.as_str()),
                "partial track filename does not match its title"
            );
        }
        if let Some(expected) = expected_tracks {
            let track_files: BTreeSet<PathBuf> = track.files.iter().cloned().collect();
            anyhow::ensure!(
                expected.get(&track.title) == Some(&track_files),
                "partial track files do not match its title"
            );
        }
        for relative in &track.files {
            validate_relative_filename(relative)?;
            anyhow::ensure!(files.insert(relative.clone()), "duplicate partial filename");
            let metadata = fs::metadata(canonical_dir.join(relative))?;
            anyhow::ensure!(
                metadata.is_file() && metadata.len() > 0,
                "partial canonical file is missing or empty"
            );
        }
    }
    Ok(())
}

fn partial_files(manifest: &PartialManifest) -> BTreeSet<PathBuf> {
    manifest
        .tracks
        .iter()
        .flat_map(|track| track.files.iter().cloned())
        .collect()
}

fn snapshot_partial(canonical_dir: &Path, manifest: &PartialManifest) -> Result<tempfile::TempDir> {
    let parent = canonical_dir.parent().unwrap_or(canonical_dir);
    let snapshot = tempfile::Builder::new()
        .prefix(".concert-split-partial-rollback-")
        .tempdir_in(parent)?;
    for relative in partial_files(manifest) {
        copy_synced(
            &canonical_dir.join(&relative),
            &snapshot.path().join(relative),
        )?;
    }
    copy_synced(
        &canonical_dir.join(PARTIAL_MANIFEST_NAME),
        &snapshot.path().join(PARTIAL_MANIFEST_NAME),
    )?;
    Ok(snapshot)
}

fn restore_partial(
    canonical_dir: &Path,
    snapshot: &tempfile::TempDir,
    manifest: &PartialManifest,
    attempt_files: &BTreeSet<PathBuf>,
) -> Result<()> {
    let prior_files = partial_files(manifest);
    for relative in attempt_files.difference(&prior_files) {
        let path = canonical_dir.join(relative);
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    for relative in prior_files {
        copy_synced(
            &snapshot.path().join(&relative),
            &canonical_dir.join(relative),
        )?;
    }
    copy_synced(
        &snapshot.path().join(PARTIAL_MANIFEST_NAME),
        &canonical_dir.join(PARTIAL_MANIFEST_NAME),
    )?;
    Ok(())
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

    let partial_prior = read_partial_manifest(&request.canonical_dir)?;
    anyhow::ensure!(
        !(partial_prior.is_some() && request.canonical_dir.join(MANIFEST_NAME).exists()),
        "Published and Recoverable Partial Split manifests both exist"
    );
    let prior = if partial_prior.is_some() {
        PublishedManifest {
            files: BTreeSet::new(),
        }
    } else {
        read_manifest(&request.canonical_dir, &replacement)?
    };
    let partial_snapshot = partial_prior
        .as_ref()
        .map(|partial| snapshot_partial(&request.canonical_dir, partial))
        .transpose()?;
    let backup = request.canonical_dir.join(BACKUP_DIR_NAME);
    let backup_candidate = request.canonical_dir.join(BACKUP_CANDIDATE_NAME);
    let old_backup = request.canonical_dir.join(BACKUP_OLD_NAME);
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
        let partial_owned = partial_prior
            .as_ref()
            .map(partial_files)
            .unwrap_or_default();
        let obsolete: BTreeSet<PathBuf> = prior
            .files
            .union(&partial_owned)
            .filter(|relative| !replacement.contains(*relative))
            .cloned()
            .collect();
        for relative in obsolete {
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
        if partial_prior.is_some() {
            fs::remove_file(request.canonical_dir.join(PARTIAL_MANIFEST_NAME))?;
        }
        #[cfg(test)]
        maybe_fail(FailurePoint::ManifestInstall)?;
        fs::rename(manifest_temp, request.canonical_dir.join(MANIFEST_NAME))?;
        Ok(())
    })();

    if let Err(publication_error) = result {
        let manifest_temp = request.canonical_dir.join(MANIFEST_TEMP_NAME);
        let cleanup_error = if manifest_temp.exists() {
            fs::remove_file(manifest_temp).err()
        } else {
            None
        };
        if let Err(rollback_error) =
            restore_prior(&request.canonical_dir, &backup, &prior, &replacement)
        {
            if let Some(snapshot) = partial_snapshot {
                let preserved = snapshot.keep();
                return Err(rollback_error).with_context(|| {
                    format!(
                        "publication failed ({publication_error:#}); rollback failed; partial recovery snapshot preserved at {}",
                        preserved.display()
                    )
                });
            }
            return Err(rollback_error).with_context(|| {
                format!("publication failed ({publication_error:#}) and rollback also failed")
            });
        }
        if old_backup.exists() {
            if backup.exists() {
                fs::remove_dir_all(&backup)?;
            }
            fs::rename(&old_backup, &backup)?;
        }
        if let (Some(partial), Some(snapshot)) = (&partial_prior, &partial_snapshot) {
            if let Err(rollback_error) =
                restore_partial(&request.canonical_dir, snapshot, partial, &replacement)
            {
                let preserved = partial_snapshot.unwrap().keep();
                return Err(rollback_error).with_context(|| {
                    format!(
                        "publication failed ({publication_error:#}); partial rollback failed; recovery snapshot preserved at {}",
                        preserved.display()
                    )
                });
            }
        }
        if let Some(cleanup_error) = cleanup_error {
            return Err(cleanup_error).with_context(|| {
                format!("publication failed ({publication_error:#}) and temporary cleanup failed")
            });
        }
        return Err(publication_error);
    }
    if old_backup.exists() {
        if let Err(error) = fs::remove_dir_all(&old_backup) {
            log::warn!(
                "Warning: Published Concert Split committed but old backup cleanup failed at {}: {error}",
                old_backup.display()
            );
        }
    }
    Ok(())
}

pub fn publish_partial(request: &PartialPublicationRequest) -> Result<Vec<PartialTrackOutput>> {
    anyhow::ensure!(
        !request.completed_tracks.is_empty(),
        "Recoverable Partial Split has no completed tracks"
    );
    let mut expected = BTreeSet::new();
    let mut expected_by_title = BTreeMap::new();
    for track in &request.expected_tracks {
        let mut files = BTreeSet::new();
        for relative in &track.files {
            validate_relative_filename(relative)?;
            anyhow::ensure!(
                files.insert(relative.clone()),
                "duplicate expected filename"
            );
            anyhow::ensure!(
                expected.insert(relative.clone()),
                "duplicate expected filename"
            );
        }
        anyhow::ensure!(!files.is_empty(), "expected track has no files");
        anyhow::ensure!(
            expected_by_title
                .insert(track.title.clone(), files)
                .is_none(),
            "duplicate expected track title"
        );
    }
    let expected_titles: BTreeSet<String> = expected_by_title.keys().cloned().collect();
    anyhow::ensure!(
        expected_titles.len() == request.expected_tracks.len(),
        "duplicate expected track title"
    );
    let mut titles = BTreeSet::new();
    let mut completed_files = BTreeSet::new();
    for track in &request.completed_tracks {
        anyhow::ensure!(!track.title.is_empty(), "partial track title is empty");
        anyhow::ensure!(
            expected_titles.contains(&track.title),
            "unknown partial track title"
        );
        anyhow::ensure!(
            titles.insert(track.title.clone()),
            "duplicate partial track title"
        );
        anyhow::ensure!(
            track.start_time.is_finite()
                && track.end_time.is_finite()
                && track.start_time < track.end_time,
            "partial track timing is invalid"
        );
        anyhow::ensure!(!track.files.is_empty(), "partial track has no files");
        let track_files: BTreeSet<PathBuf> = track.files.iter().cloned().collect();
        anyhow::ensure!(
            expected_by_title.get(&track.title) == Some(&track_files),
            "partial track files do not match its title"
        );
        for relative in &track.files {
            validate_relative_filename(relative)?;
            anyhow::ensure!(expected.contains(relative), "partial file is not expected");
            anyhow::ensure!(
                completed_files.insert(relative.clone()),
                "duplicate partial filename"
            );
            let metadata = fs::metadata(request.staging_dir.join(relative))
                .with_context(|| format!("missing staged partial {}", relative.display()))?;
            anyhow::ensure!(
                metadata.is_file() && metadata.len() > 0,
                "staged partial is missing or empty: {}",
                relative.display()
            );
        }
    }

    fs::create_dir_all(&request.canonical_dir)?;
    let lock = lock_file(&request.canonical_dir)?;
    acquire_with_timeout(|| FileExt::try_lock_exclusive(&lock), "exclusive")?;
    let published_exists = request.canonical_dir.join(MANIFEST_NAME).exists();
    let partial_exists = request.canonical_dir.join(PARTIAL_MANIFEST_NAME).exists();
    anyhow::ensure!(
        !(published_exists && partial_exists),
        "Published and Recoverable Partial Split manifests both exist"
    );
    if published_exists {
        return Err(PublishedSplitExists.into());
    }
    let prior_partial = read_partial_manifest(&request.canonical_dir)?;
    if let Some(prior) = &prior_partial {
        validate_partial_manifest(&request.canonical_dir, prior, Some(&expected_by_title))?;
    } else {
        anyhow::ensure!(
            !expected
                .iter()
                .any(|relative| request.canonical_dir.join(relative).exists()),
            "legacy canonical split output already exists"
        );
    }
    let mut snapshot = prior_partial
        .as_ref()
        .map(|prior| snapshot_partial(&request.canonical_dir, prior))
        .transpose()?;
    let mut merged: BTreeMap<String, PartialTrackOutput> = prior_partial
        .as_ref()
        .into_iter()
        .flat_map(|manifest| manifest.tracks.iter().cloned())
        .map(|track| (track.title.clone(), track))
        .collect();
    for track in &request.completed_tracks {
        merged.insert(track.title.clone(), track.clone());
    }
    let merged_tracks: Vec<PartialTrackOutput> = request
        .expected_tracks
        .iter()
        .filter_map(|track| merged.remove(&track.title))
        .collect();
    let merged_files: BTreeSet<PathBuf> = merged_tracks
        .iter()
        .flat_map(|track| track.files.iter().cloned())
        .collect();
    let obsolete: BTreeSet<PathBuf> = prior_partial
        .as_ref()
        .map(partial_files)
        .unwrap_or_default()
        .difference(&merged_files)
        .cloned()
        .collect();

    let mut installed = Vec::new();
    let result = (|| -> Result<()> {
        for relative in &completed_files {
            let temporary = request.canonical_dir.join(format!(
                ".concert-split-partial-copy-{}",
                relative.to_string_lossy()
            ));
            copy_synced(&request.staging_dir.join(relative), &temporary)?;
            fs::rename(&temporary, request.canonical_dir.join(relative))?;
            installed.push(relative.clone());
        }
        for relative in &obsolete {
            let path = request.canonical_dir.join(relative);
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
        let manifest = PartialManifest {
            tracks: merged_tracks.clone(),
        };
        let temporary = request.canonical_dir.join(PARTIAL_MANIFEST_TEMP_NAME);
        let mut file = File::create(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
        file.sync_all()?;
        #[cfg(test)]
        maybe_fail(FailurePoint::PartialManifestInstall)?;
        fs::rename(temporary, request.canonical_dir.join(PARTIAL_MANIFEST_NAME))?;
        Ok(())
    })();
    if let Err(error) = result {
        let manifest_temporary = request.canonical_dir.join(PARTIAL_MANIFEST_TEMP_NAME);
        let cleanup_error = if manifest_temporary.exists() {
            fs::remove_file(&manifest_temporary).err()
        } else {
            None
        };
        let attempt_files: BTreeSet<PathBuf> = installed.into_iter().collect();
        if let (Some(prior), Some(snapshot_ref)) = (&prior_partial, &snapshot) {
            if let Err(rollback_error) =
                restore_partial(&request.canonical_dir, snapshot_ref, prior, &attempt_files)
            {
                let preserved = snapshot.take().unwrap().keep();
                return Err(rollback_error).with_context(|| {
                    format!(
                        "partial publication failed ({error:#}); rollback failed; recovery snapshot preserved at {}",
                        preserved.display()
                    )
                });
            }
        } else {
            for relative in attempt_files {
                let path = request.canonical_dir.join(relative);
                if path.exists() {
                    fs::remove_file(path)?;
                }
            }
        }
        if let Some(cleanup_error) = cleanup_error {
            return Err(cleanup_error).with_context(|| {
                format!("partial publication failed ({error:#}) and temporary cleanup failed")
            });
        }
        return Err(error);
    }
    Ok(merged_tracks)
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

    fn expected(tracks: &[(&str, &[&str])]) -> Vec<PartialExpectedTrack> {
        tracks
            .iter()
            .map(|(title, files)| PartialExpectedTrack {
                title: (*title).to_string(),
                files: files.iter().map(PathBuf::from).collect(),
            })
            .collect()
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
    fn failed_resplit_restores_the_previous_retained_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("concert");
        for (directory, bytes) in [("first", b"first" as &[u8]), ("second", b"second")] {
            let staging = tmp.path().join(directory);
            write(&staging.join("Song.m4a"), bytes);
            publish(&PublicationRequest {
                canonical_dir: canonical.clone(),
                staging_dir: staging,
                replacement_files: vec![PathBuf::from("Song.m4a")],
            })
            .unwrap();
        }
        let backup_before = fs::read(canonical.join(BACKUP_DIR_NAME).join("Song.m4a")).unwrap();
        let failed = tmp.path().join("failed");
        write(&failed.join("Song.m4a"), b"third");
        fail_at(FailurePoint::ManifestInstall);

        assert!(publish(&PublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: failed,
            replacement_files: vec![PathBuf::from("Song.m4a")],
        })
        .is_err());

        assert_eq!(fs::read(canonical.join("Song.m4a")).unwrap(), b"second");
        assert_eq!(
            fs::read(canonical.join(BACKUP_DIR_NAME).join("Song.m4a")).unwrap(),
            backup_before
        );
        assert!(!canonical.join(BACKUP_OLD_NAME).exists());
        assert!(!canonical.join(MANIFEST_TEMP_NAME).exists());
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

    #[test]
    fn first_partial_publication_copies_only_completed_songs() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("concert");
        let staging = tmp.path().join("staging");
        write(&staging.join("First.m4a"), b"first");

        let published = publish_partial(&PartialPublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: staging,
            expected_tracks: expected(&[("First", &["First.m4a"]), ("Second", &["Second.m4a"])]),
            completed_tracks: vec![PartialTrackOutput {
                title: "First".to_string(),
                start_time: 0.0,
                end_time: 10.0,
                files: vec![PathBuf::from("First.m4a")],
            }],
        })
        .unwrap();

        assert_eq!(published.len(), 1);
        assert_eq!(published[0].title, "First");
        assert_eq!(fs::read(canonical.join("First.m4a")).unwrap(), b"first");
        assert!(!canonical.join("Second.m4a").exists());
        assert!(canonical.join(PARTIAL_MANIFEST_NAME).is_file());
        assert!(!canonical.join(MANIFEST_NAME).exists());
        assert!(!canonical.join(BACKUP_DIR_NAME).exists());
    }

    #[test]
    fn partial_publication_never_replaces_a_published_split() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("concert");
        let first = tmp.path().join("first");
        write(&first.join("First.m4a"), b"published");
        publish(&PublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: first,
            replacement_files: vec![PathBuf::from("First.m4a")],
        })
        .unwrap();
        let manifest_before = fs::read(canonical.join(MANIFEST_NAME)).unwrap();

        let failed = tmp.path().join("failed");
        write(&failed.join("First.m4a"), b"partial replacement");
        let error = publish_partial(&PartialPublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: failed,
            expected_tracks: expected(&[("First", &["First.m4a"])]),
            completed_tracks: vec![PartialTrackOutput {
                title: "First".to_string(),
                start_time: 0.0,
                end_time: 5.0,
                files: vec![PathBuf::from("First.m4a")],
            }],
        })
        .unwrap_err();

        assert!(error.to_string().contains("Published Concert Split"));
        assert_eq!(fs::read(canonical.join("First.m4a")).unwrap(), b"published");
        assert_eq!(
            fs::read(canonical.join(MANIFEST_NAME)).unwrap(),
            manifest_before
        );
        assert!(!canonical.join(PARTIAL_MANIFEST_NAME).exists());
    }

    #[test]
    fn partial_retry_merges_valid_prior_tracks() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("concert");
        let first = tmp.path().join("first");
        write(&first.join("First.m4a"), b"first");
        publish_partial(&PartialPublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: first,
            expected_tracks: expected(&[("First", &["First.m4a"]), ("Second", &["Second.m4a"])]),
            completed_tracks: vec![PartialTrackOutput {
                title: "First".to_string(),
                start_time: 0.0,
                end_time: 10.0,
                files: vec![PathBuf::from("First.m4a")],
            }],
        })
        .unwrap();
        let second = tmp.path().join("second");
        write(&second.join("Second.m4a"), b"second");

        let merged = publish_partial(&PartialPublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: second,
            expected_tracks: expected(&[("First", &["First.m4a"]), ("Second", &["Second.m4a"])]),
            completed_tracks: vec![PartialTrackOutput {
                title: "Second".to_string(),
                start_time: 10.0,
                end_time: 20.0,
                files: vec![PathBuf::from("Second.m4a")],
            }],
        })
        .unwrap();

        assert_eq!(
            merged
                .iter()
                .map(|track| track.title.as_str())
                .collect::<Vec<_>>(),
            vec!["First", "Second"]
        );
        assert_eq!(fs::read(canonical.join("First.m4a")).unwrap(), b"first");
        assert_eq!(fs::read(canonical.join("Second.m4a")).unwrap(), b"second");
    }

    #[test]
    fn first_partial_manifest_failure_removes_all_attempt_files() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("concert");
        let staging = tmp.path().join("staging");
        write(&staging.join("First.m4a"), b"first");
        fail_at(FailurePoint::PartialManifestInstall);

        assert!(publish_partial(&PartialPublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: staging,
            expected_tracks: expected(&[("First", &["First.m4a"])]),
            completed_tracks: vec![PartialTrackOutput {
                title: "First".to_string(),
                start_time: 0.0,
                end_time: 10.0,
                files: vec![PathBuf::from("First.m4a")],
            }],
        })
        .is_err());

        assert!(!canonical.join("First.m4a").exists());
        assert!(!canonical.join(PARTIAL_MANIFEST_NAME).exists());
        assert!(!canonical.join(PARTIAL_MANIFEST_TEMP_NAME).exists());
    }

    #[test]
    fn first_partial_copy_failure_leaves_no_canonical_track() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("concert");
        let staging = tmp.path().join("staging");
        write(&staging.join("First.m4a"), b"first");
        fail_copy_after(0);

        assert!(publish_partial(&PartialPublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: staging,
            expected_tracks: expected(&[("First", &["First.m4a"])]),
            completed_tracks: vec![PartialTrackOutput {
                title: "First".to_string(),
                start_time: 0.0,
                end_time: 10.0,
                files: vec![PathBuf::from("First.m4a")],
            }],
        })
        .is_err());

        assert!(!canonical.join("First.m4a").exists());
        assert!(!canonical.join(PARTIAL_MANIFEST_NAME).exists());
    }

    #[test]
    fn partial_title_cannot_claim_another_tracks_file() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("concert");
        let staging = tmp.path().join("staging");
        write(&staging.join("Second.m4a"), b"second");

        let error = publish_partial(&PartialPublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: staging,
            expected_tracks: expected(&[("First", &["First.m4a"]), ("Second", &["Second.m4a"])]),
            completed_tracks: vec![PartialTrackOutput {
                title: "First".to_string(),
                start_time: 0.0,
                end_time: 10.0,
                files: vec![PathBuf::from("Second.m4a")],
            }],
        })
        .unwrap_err();

        assert!(error.to_string().contains("do not match"));
        assert!(!canonical.exists());
    }

    #[test]
    fn partial_retry_manifest_failure_restores_prior_bytes_and_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("concert");
        let first = tmp.path().join("first");
        write(&first.join("First.m4a"), b"known-good");
        publish_partial(&PartialPublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: first,
            expected_tracks: expected(&[("First", &["First.m4a"]), ("Second", &["Second.m4a"])]),
            completed_tracks: vec![PartialTrackOutput {
                title: "First".to_string(),
                start_time: 0.0,
                end_time: 10.0,
                files: vec![PathBuf::from("First.m4a")],
            }],
        })
        .unwrap();
        let manifest_before = fs::read(canonical.join(PARTIAL_MANIFEST_NAME)).unwrap();
        let retry = tmp.path().join("retry");
        write(&retry.join("First.m4a"), b"replacement");
        write(&retry.join("Second.m4a"), b"new");
        fail_at(FailurePoint::PartialManifestInstall);

        assert!(publish_partial(&PartialPublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: retry,
            expected_tracks: expected(&[("First", &["First.m4a"]), ("Second", &["Second.m4a"])]),
            completed_tracks: vec![
                PartialTrackOutput {
                    title: "First".to_string(),
                    start_time: 1.0,
                    end_time: 11.0,
                    files: vec![PathBuf::from("First.m4a")],
                },
                PartialTrackOutput {
                    title: "Second".to_string(),
                    start_time: 11.0,
                    end_time: 20.0,
                    files: vec![PathBuf::from("Second.m4a")],
                },
            ],
        })
        .is_err());

        assert_eq!(
            fs::read(canonical.join("First.m4a")).unwrap(),
            b"known-good"
        );
        assert!(!canonical.join("Second.m4a").exists());
        assert_eq!(
            fs::read(canonical.join(PARTIAL_MANIFEST_NAME)).unwrap(),
            manifest_before
        );
    }

    #[test]
    fn complete_publication_supersedes_partial_without_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("concert");
        let partial = tmp.path().join("partial");
        write(&partial.join("First.m4a"), b"partial");
        publish_partial(&PartialPublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: partial,
            expected_tracks: expected(&[("First", &["First.m4a"]), ("Second", &["Second.m4a"])]),
            completed_tracks: vec![PartialTrackOutput {
                title: "First".to_string(),
                start_time: 0.0,
                end_time: 10.0,
                files: vec![PathBuf::from("First.m4a")],
            }],
        })
        .unwrap();
        let complete = tmp.path().join("complete");
        write(&complete.join("First.m4a"), b"complete-first");
        write(&complete.join("Second.m4a"), b"complete-second");

        publish(&PublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: complete,
            replacement_files: vec![PathBuf::from("First.m4a"), PathBuf::from("Second.m4a")],
        })
        .unwrap();

        assert_eq!(
            fs::read(canonical.join("First.m4a")).unwrap(),
            b"complete-first"
        );
        assert!(canonical.join(MANIFEST_NAME).is_file());
        assert!(!canonical.join(PARTIAL_MANIFEST_NAME).exists());
        assert!(!canonical.join(BACKUP_DIR_NAME).exists());
    }

    #[test]
    fn complete_manifest_failure_restores_prior_partial() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("concert");
        let partial = tmp.path().join("partial");
        write(&partial.join("First.m4a"), b"partial");
        publish_partial(&PartialPublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: partial,
            expected_tracks: expected(&[("First", &["First.m4a"])]),
            completed_tracks: vec![PartialTrackOutput {
                title: "First".to_string(),
                start_time: 0.0,
                end_time: 10.0,
                files: vec![PathBuf::from("First.m4a")],
            }],
        })
        .unwrap();
        let complete = tmp.path().join("complete");
        write(&complete.join("First.m4a"), b"complete");
        fail_at(FailurePoint::ManifestInstall);

        assert!(publish(&PublicationRequest {
            canonical_dir: canonical.clone(),
            staging_dir: complete,
            replacement_files: vec![PathBuf::from("First.m4a")],
        })
        .is_err());
        assert_eq!(fs::read(canonical.join("First.m4a")).unwrap(), b"partial");
        assert!(canonical.join(PARTIAL_MANIFEST_NAME).is_file());
        assert!(!canonical.join(MANIFEST_NAME).exists());
    }
}
