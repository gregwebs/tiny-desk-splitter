//! Concert Media Inventory: filesystem-backed facts about a concert's media
//! files.
//!
//! This module owns downloaded-source lookup, split-track lookup, interlude
//! lookup, all-tracks-present checks, reconstruction-playback item
//! construction, and source-redundancy (destructive-deletion gating). It is a
//! v1 refactor extraction from `model.rs` — see
//! `docs/change/2026-07-09-concert-media-inventory.md` for the module
//! boundary and migration notes.
//!
//! `playback.rs` keeps playback plan selection, playback-facing response
//! structs/errors, and next/previous playable-track policy. `model.rs` keeps
//! shared domain data (`Concert`, `TrackInfo`, `PlaybackItem`, status enums)
//! and the generic path helpers (`concert_dir`, `sanitize_album`,
//! `sanitize_filename`, `is_browser_playable`) used by unrelated concerns
//! (scraping, archiving, downloading).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::model::{
    concert_dir, is_browser_playable, is_track_available, sanitize_filename, PlaybackItem,
    PlaybackItemKind, TrackDetailItem, TrackInfo,
};

pub fn is_video_extension(ext: &str) -> bool {
    matches!(ext.to_lowercase().as_str(), "mp4" | "webm")
}

const TRACK_MEDIA_EXTENSIONS: &[&str] = &[
    "mp4", "m4a", "webm", "mp3", "ogg", "opus", "wav", "flac", "mkv",
];

pub fn find_track_file(working_dir: &Path, album: &str, title: &str) -> Option<String> {
    let stem = sanitize_filename(title);
    let dir = concert_dir(working_dir, album);
    for ext in TRACK_MEDIA_EXTENSIONS {
        let filename = format!("{stem}.{ext}");
        if dir.join(&filename).exists() {
            return Some(filename);
        }
    }
    None
}

/// Why an application-side removal of canonical media could not run.
#[derive(Debug)]
pub enum MediaRemovalError {
    /// A Concert Split publication holds the concert directory, so removing
    /// canonical files now would delete them out from under the publisher.
    /// Conservative by design: the caller reports the conflict rather than
    /// waiting for, or racing, a publication.
    PublicationInProgress,
    Failed(anyhow::Error),
}

impl std::fmt::Display for MediaRemovalError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PublicationInProgress => formatter.write_str(
                "a Concert Split publication is in progress, so canonical media cannot be changed",
            ),
            Self::Failed(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for MediaRemovalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PublicationInProgress => None,
            Self::Failed(error) => Some(error.as_ref()),
        }
    }
}

impl From<anyhow::Error> for MediaRemovalError {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed(error)
    }
}

impl From<std::io::Error> for MediaRemovalError {
    fn from(error: std::io::Error) -> Self {
        Self::Failed(error.into())
    }
}

impl MediaRemovalError {
    /// Attach what the removal was for without turning "a publication is in
    /// progress" into an operational failure: that outcome keeps its own
    /// meaning, because the remedy is to retry, not to investigate a fault.
    pub fn add_context(self, context: impl std::fmt::Display) -> Self {
        match self {
            Self::PublicationInProgress => Self::PublicationInProgress,
            Self::Failed(error) => Self::Failed(error.context(context.to_string())),
        }
    }
}

/// Remove every file that canonical discovery would find for a track: the
/// filename stem with any of the known media extensions. Missing files are
/// normal (the desired state may already hold); only a real removal failure is
/// an error. Returns the files actually removed.
pub fn remove_track_files(
    working_dir: &Path,
    album: &str,
    title: &str,
) -> Result<Vec<PathBuf>, MediaRemovalError> {
    remove_canonical_media(
        working_dir,
        album,
        &sanitize_filename(title),
        TRACK_MEDIA_EXTENSIONS,
    )
}

/// Remove every file that canonical discovery would find for an interlude.
/// See [`remove_track_files`].
pub fn remove_interlude_files(
    working_dir: &Path,
    album: &str,
    index: usize,
) -> Result<Vec<PathBuf>, MediaRemovalError> {
    remove_canonical_media(
        working_dir,
        album,
        &concert_types::interlude_filename_stem(index),
        INTERLUDE_EXTENSIONS,
    )
}

/// The one place application code removes Published Concert Split media.
///
/// Removing canonical files is a mutation of state the publication journal and
/// manifest own, so it happens under the publication lock — and it leaves both
/// manifests naming only files that still exist, because a later publication
/// backs up, merges, and restores exactly what they name.
///
/// The guard is shared rather than exclusive because readers hold it too: a
/// stream in progress must not block a deletion. Deletions are therefore not
/// serialized against each other by the lock, which is safe as long as every
/// caller holds its database lock across the removal and the manifest rewrite —
/// `Concerts` does, and it is the only way canonical media is deleted.
fn remove_canonical_media(
    working_dir: &Path,
    album: &str,
    stem: &str,
    extensions: &[&str],
) -> Result<Vec<PathBuf>, MediaRemovalError> {
    let directory = concert_dir(working_dir, album);
    if !directory.try_exists()? {
        // Nothing to remove, and no reason to create a concert directory for a
        // deletion that has nothing to delete.
        return Ok(Vec::new());
    }
    let Some(guard) =
        live_set_splitter::publication::SharedPublicationLock::try_acquire(&directory)?
    else {
        return Err(MediaRemovalError::PublicationInProgress);
    };
    let removal = remove_media_files(&directory, stem, extensions);
    let removed_names = removal
        .removed
        .iter()
        .filter_map(|path| path.file_name().map(PathBuf::from))
        .collect();
    // Correct the manifests even when a removal failed: a file that is gone must
    // stop being claimed, whether or not the rest of the removal worked. The
    // correction is a reconciliation, not just a subtraction, so a retry of a
    // deletion whose manifest rewrite failed repairs it rather than reporting a
    // no-op success over a manifest that still claims the file.
    live_set_splitter::publication::forget_removed_split_files(&guard, &removed_names)?;
    match removal.failure {
        Some(error) => Err(MediaRemovalError::Failed(error)),
        None => Ok(removal.removed),
    }
}

/// What a removal attempt actually achieved. A failure is reported alongside the
/// files it did remove, so the manifest can still be told the truth.
struct MediaRemoval {
    removed: Vec<PathBuf>,
    failure: Option<anyhow::Error>,
}

fn remove_media_files(directory: &Path, stem: &str, extensions: &[&str]) -> MediaRemoval {
    let mut removal = MediaRemoval {
        removed: Vec::new(),
        failure: None,
    };
    for extension in extensions {
        let path = directory.join(format!("{stem}.{extension}"));
        match std::fs::remove_file(&path) {
            Ok(()) => removal.removed.push(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                removal.failure = Some(
                    anyhow::Error::new(error)
                        .context(format!("failed to remove {}", path.display())),
                );
                break;
            }
        }
    }
    removal
}

/// Extensions probed when looking for or deleting interlude files (priority order).
pub const INTERLUDE_EXTENSIONS: &[&str] = &["mp4", "m4a"];

/// Return the filename (stem + extension) of the interlude file for `index` if
/// it exists on disk, or `None`. Probes `.mp4` then `.m4a`. Uses
/// [`concert_types::interlude_filename_stem`] so the name always matches what
/// the splitter writes.
pub fn find_interlude_track_file(working_dir: &Path, album: &str, index: usize) -> Option<String> {
    let stem = concert_types::interlude_filename_stem(index);
    let dir = concert_dir(working_dir, album);
    for ext in INTERLUDE_EXTENSIONS {
        let filename = format!("{stem}.{ext}");
        if dir.join(&filename).exists() {
            return Some(filename);
        }
    }
    None
}

/// Return true if an interlude file for `index` exists on disk (either `.mp4`
/// or `.m4a`). Uses [`concert_types::interlude_filename_stem`] so the name
/// always matches what the splitter writes.
pub fn find_interlude_file(working_dir: &Path, album: &str, index: usize) -> bool {
    find_interlude_track_file(working_dir, album, index).is_some()
}

/// Extensions probed when looking for the downloaded source file.
const DOWNLOADED_MEDIA_EXTENSIONS: &[&str] = &[
    "mp4", "m4a", "webm", "mkv", "mp3", "ogg", "opus", "wav", "flac",
];

/// Find the downloaded media file for an album inside its concert dir
/// (`{working_dir}/concerts/{sanitize_album(album)}/`).
///
/// yt-dlp writes the file as `{sanitize_album(album)}.{ext}` where `ext` is
/// picked at runtime (typically `mp4`). We don't know the extension up front,
/// so we list the directory and return the first entry whose file stem matches
/// the sanitized album and has a known media extension.
pub fn find_downloaded_file(working_dir: &Path, album: &str) -> Option<PathBuf> {
    let expected_stem = crate::model::sanitize_album(album);
    let cd = concert_dir(working_dir, album);
    let entries = std::fs::read_dir(&cd).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem != expected_stem {
            continue;
        }
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if !DOWNLOADED_MEDIA_EXTENSIONS
            .iter()
            .any(|e| e.eq_ignore_ascii_case(ext))
        {
            continue;
        }
        return Some(path);
    }
    None
}

/// Fallible source lookup for callers that must distinguish absent media from
/// an observation failure. A missing concert directory is a known absence;
/// failures reading the directory, an entry, or its file type are propagated.
pub fn try_find_downloaded_file(
    working_dir: &Path,
    album: &str,
) -> std::io::Result<Option<PathBuf>> {
    let expected_stem = crate::model::sanitize_album(album);
    let cd = concert_dir(working_dir, album);
    let entries = match std::fs::read_dir(&cd) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem != expected_stem {
            continue;
        }
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if DOWNLOADED_MEDIA_EXTENSIONS
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(ext))
        {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// Determine whether the original source file is **fully redundant** — every
/// second of `[0, media_duration]` is covered by a present song track or an
/// interlude file on disk, so the source can be safely deleted.
///
/// Returns `false` (fails closed) when:
/// - the source file itself is already gone (nothing left to call redundant),
/// - `media_duration` is absent (not yet persisted),
/// - `user_split_timestamps` are absent (no user split has been done),
/// - any song track is missing (`tracks_present` has a `false`),
/// - any required interlude file is not on disk.
///
/// `working_dir` and `album` are needed to probe the source and interlude
/// files on disk.
pub fn source_redundant(
    working_dir: &Path,
    album: &str,
    tracks_present: &[bool],
    user_split_timestamps: Option<&[concert_types::SongTimestamp]>,
    media_duration: Option<f64>,
) -> bool {
    if find_downloaded_file(working_dir, album).is_none() {
        return false;
    }
    let Some(duration) = media_duration else {
        return false;
    };
    let Some(songs) = user_split_timestamps else {
        return false;
    };
    if tracks_present.iter().any(|&p| !p) {
        return false;
    }
    let interludes = concert_types::derive_interludes(songs, duration);
    interludes
        .iter()
        .all(|il| find_interlude_file(working_dir, album, il.index))
}

// ── Reconstruction playback ──────────────────────────────────────────────────

/// Build the time-ordered playback sequence for whole-concert reconstruction
/// (source file absent, tracks + interlude files present).
///
/// When `user_timestamps` is `None`, falls back to songs-only order (no
/// interludes) — mirrors the case before the user has ever adjusted timestamps.
///
/// Each item returned is browser-playable and its file is confirmed to exist on
/// disk. Applies the *deleted-song rule*: if a song is absent (`!tracks_present[i]`
/// or no file on disk), the interlude immediately **before** it is also dropped
/// (the interlude after a deleted song is kept). A tail interlude (no following
/// song) is always kept when its file exists.
///
/// Returns an empty `Vec` when nothing is playable.
pub fn build_reconstruction(
    working_dir: &Path,
    album: &str,
    set_list: &[String],
    tracks_present: &[bool],
    tracks_liked: &[bool],
    user_timestamps: Option<&[concert_types::SongTimestamp]>,
    media_duration: Option<f64>,
) -> Vec<PlaybackItem> {
    build_reconstruction_with_lookup(
        set_list,
        tracks_present,
        tracks_liked,
        user_timestamps,
        media_duration,
        |title| find_track_file(working_dir, album, title),
        |index| find_interlude_track_file(working_dir, album, index),
    )
}

fn build_reconstruction_with_lookup(
    set_list: &[String],
    tracks_present: &[bool],
    tracks_liked: &[bool],
    user_timestamps: Option<&[concert_types::SongTimestamp]>,
    media_duration: Option<f64>,
    find_track: impl Fn(&str) -> Option<String>,
    find_interlude: impl Fn(usize) -> Option<String>,
) -> Vec<PlaybackItem> {
    // No-user-ts fallback: songs only, no interludes.
    let Some(songs) = user_timestamps else {
        return songs_only_with_lookup(set_list, tracks_present, tracks_liked, &find_track);
    };

    let duration = match media_duration {
        Some(d) if d > 0.0 => d,
        _ => {
            // Can't derive interludes without a known duration; fall back to songs only.
            return songs_only_with_lookup(set_list, tracks_present, tracks_liked, &find_track);
        }
    };

    let interludes = concert_types::derive_interludes(songs, duration);

    // Build a unified list of (start_time, slot) merged in time order.
    // Songs and interludes are already non-overlapping by construction.
    enum Slot {
        Song(usize),      // song index in set_list
        Interlude(usize), // 1-based interlude index
    }
    let mut slots: Vec<(f64, Slot)> = Vec::with_capacity(set_list.len() + interludes.len());
    for (i, ts) in songs.iter().enumerate() {
        slots.push((ts.start_time, Slot::Song(i)));
    }
    for il in &interludes {
        slots.push((il.start_time, Slot::Interlude(il.index)));
    }
    slots.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    // First pass: determine which songs are "kept" (present on disk + browser-playable).
    let song_kept: Vec<bool> = set_list
        .iter()
        .enumerate()
        .map(|(i, title)| {
            let present = tracks_present.get(i).copied().unwrap_or(false);
            if !present {
                return false;
            }
            match find_track(title) {
                Some(f) => {
                    let ext = f.rsplit('.').next().unwrap_or("");
                    is_browser_playable(ext)
                }
                None => false,
            }
        })
        .collect();

    // Second pass: build the filtered playback list.
    let mut result = Vec::new();
    for (slot_pos, (_, slot)) in slots.iter().enumerate() {
        match slot {
            Slot::Song(i) => {
                if !song_kept.get(*i).copied().unwrap_or(false) {
                    continue;
                }
                let title = &set_list[*i];
                let Some(filename) = find_track(title) else {
                    continue;
                };
                let is_video = {
                    let ext = filename.rsplit('.').next().unwrap_or("");
                    is_video_extension(ext)
                };
                let liked = tracks_liked.get(*i).copied().unwrap_or(false);
                result.push(PlaybackItem {
                    kind: PlaybackItemKind::Song {
                        track_index: *i,
                        liked,
                    },
                    title: title.clone(),
                    filename,
                    is_video,
                });
            }
            Slot::Interlude(idx) => {
                // Deleted-song rule: find the next Song slot after this interlude.
                let next_song_deleted = slots[slot_pos + 1..]
                    .iter()
                    .find_map(|(_, s)| match s {
                        Slot::Song(i) => Some(i),
                        Slot::Interlude(_) => None,
                    })
                    .map(|i| !song_kept.get(*i).copied().unwrap_or(false))
                    .unwrap_or(false); // tail interlude: no next song → keep
                if next_song_deleted {
                    continue;
                }
                let Some(filename) = find_interlude(*idx) else {
                    continue;
                };
                let is_video = {
                    let ext = filename.rsplit('.').next().unwrap_or("");
                    if !is_browser_playable(ext) {
                        continue;
                    }
                    is_video_extension(ext)
                };
                result.push(PlaybackItem {
                    kind: PlaybackItemKind::Interlude { index: *idx },
                    title: "interlude".to_string(),
                    filename,
                    is_video,
                });
            }
        }
    }
    result
}

fn songs_only_with_lookup(
    set_list: &[String],
    tracks_present: &[bool],
    tracks_liked: &[bool],
    find_track: impl Fn(&str) -> Option<String>,
) -> Vec<PlaybackItem> {
    set_list
        .iter()
        .enumerate()
        .filter_map(|(i, title)| {
            let present = tracks_present.get(i).copied().unwrap_or(false);
            if !present {
                return None;
            }
            let filename = find_track(title)?;
            let is_video = {
                let ext = filename.rsplit('.').next().unwrap_or("");
                if !is_browser_playable(ext) {
                    return None;
                }
                is_video_extension(ext)
            };
            let liked = tracks_liked.get(i).copied().unwrap_or(false);
            Some(PlaybackItem {
                kind: PlaybackItemKind::Song {
                    track_index: i,
                    liked,
                },
                title: title.clone(),
                filename,
                is_video,
            })
        })
        .collect()
}

// ── Track listing ────────────────────────────────────────────────────────────

fn track_file_extension(dir: &Path, title: &str) -> Option<&'static str> {
    let stem = sanitize_filename(title);
    for ext in TRACK_MEDIA_EXTENSIONS {
        if dir.join(format!("{stem}.{ext}")).exists() {
            return Some(ext);
        }
    }
    None
}

fn captured_track_file(files: &BTreeSet<String>, title: &str) -> Option<String> {
    let stem = sanitize_filename(title);
    TRACK_MEDIA_EXTENSIONS
        .iter()
        .map(|extension| format!("{stem}.{extension}"))
        .find(|filename| files.contains(filename))
}

fn captured_interlude_file(files: &BTreeSet<String>, index: usize) -> Option<String> {
    let stem = concert_types::interlude_filename_stem(index);
    INTERLUDE_EXTENSIONS
        .iter()
        .map(|extension| format!("{stem}.{extension}"))
        .find(|filename| files.contains(filename))
}

fn captured_source_present(files: &BTreeSet<String>, album: &str) -> bool {
    let stem = crate::model::sanitize_album(album);
    files.iter().any(|filename| {
        Path::new(filename)
            .file_stem()
            .and_then(|value| value.to_str())
            .is_some_and(|candidate| candidate == stem)
            && Path::new(filename)
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|extension| {
                    DOWNLOADED_MEDIA_EXTENSIONS
                        .iter()
                        .any(|candidate| candidate.eq_ignore_ascii_case(extension))
                })
    })
}

pub fn list_tracks(working_dir: &Path, album: &str, set_list: &[String]) -> Vec<TrackInfo> {
    let dir = concert_dir(working_dir, album);
    set_list
        .iter()
        .enumerate()
        .filter_map(|(index, title)| {
            track_file_extension(&dir, title).map(|ext| TrackInfo {
                index,
                title: title.clone(),
                available: true,
                is_video: is_video_extension(ext),
                liked: false,
            })
        })
        .collect()
}

pub fn list_all_tracks(working_dir: &Path, album: &str, set_list: &[String]) -> Vec<TrackInfo> {
    let dir = concert_dir(working_dir, album);
    set_list
        .iter()
        .enumerate()
        .map(|(index, title)| {
            let ext = track_file_extension(&dir, title);
            TrackInfo {
                index,
                title: title.clone(),
                available: ext.is_some(),
                is_video: ext.is_some_and(is_video_extension),
                liked: false,
            }
        })
        .collect()
}

/// Like `model::list_all_tracks_from_db` but also probes the filesystem for
/// `is_video` (DB-cached `tracks_present` has no file-type info). Used by
/// `GET /concerts/:id/track-details` which the player widget calls to render
/// the whole-album sidebar track list.
pub fn list_all_track_details(
    working_dir: &Path,
    album: &str,
    set_list: &[String],
    tracks_present: &[bool],
    tracks_liked: &[bool],
) -> Vec<TrackDetailItem> {
    let dir = concert_dir(working_dir, album);
    set_list
        .iter()
        .enumerate()
        .map(|(index, title)| {
            let available = is_track_available(tracks_present, index);
            let is_video =
                available && track_file_extension(&dir, title).is_some_and(is_video_extension);
            let liked = tracks_liked.get(index).copied().unwrap_or(false);
            TrackDetailItem {
                index,
                title: title.clone(),
                available,
                is_video,
                liked,
            }
        })
        .collect()
}

// ── all-tracks-present ───────────────────────────────────────────────────────

/// Filesystem presence for every title in `set_list`, in order. This is the
/// single scan loop shared by prepare/scan/split call sites that previously
/// duplicated `find_track_file(...).is_some()` inline.
pub fn tracks_present_on_disk(working_dir: &Path, album: &str, set_list: &[String]) -> Vec<bool> {
    set_list
        .iter()
        .map(|title| find_track_file(working_dir, album, title).is_some())
        .collect()
}

/// Whether every title in `set_list` currently has a file on disk.
pub fn all_tracks_present_on_disk(working_dir: &Path, album: &str, set_list: &[String]) -> bool {
    set_list
        .iter()
        .all(|title| find_track_file(working_dir, album, title).is_some())
}

// ── ConcertMediaInventory ────────────────────────────────────────────────────

/// Primary test seam for this module: a borrowed snapshot of the facts needed
/// to answer filesystem-backed media questions for one concert. Built once per
/// request via [`ConcertMediaInventory::for_concert`] and queried through its
/// methods, instead of every call site re-deriving `album`/`working_dir`
/// combinations inline.
///
/// `concert_id`, `artist`, and `downloaded_at` are carried through for
/// tracing/diagnostics only. Playback policy that distinguishes "marked
/// downloaded but the file is missing" from "never downloaded" stays in
/// `playback.rs` — this struct exposes filesystem facts, not that policy, so
/// `downloaded_at` is deliberately unused by [`Self::can_play_concert`].
pub struct ConcertMediaInventory<'a> {
    working_dir: &'a Path,
    concert_id: i64,
    album: Option<&'a str>,
    artist: Option<&'a str>,
    downloaded_at: Option<&'a str>,
    split_succeeded: bool,
    set_list: &'a [String],
    tracks_present: &'a [bool],
    tracks_liked: &'a [bool],
    user_split_timestamps: Option<&'a [concert_types::SongTimestamp]>,
    media_duration: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct PublishedMediaSnapshot {
    pub tracks_present_on_disk: Vec<bool>,
    pub reconstruction_items: Vec<PlaybackItem>,
    pub source_redundant: bool,
}

impl<'a> ConcertMediaInventory<'a> {
    fn with_published_split<T>(&self, fallback: T, operation: impl FnOnce() -> T) -> T {
        let Some(album) = self.album else {
            return fallback;
        };
        let directory = concert_dir(self.working_dir, album);
        match live_set_splitter::publication::with_shared_lock(&directory, || Ok(operation())) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(concert_id = self.concert_id, %error, "could not read Published Concert Split");
                fallback
            }
        }
    }

    /// Build an inventory from an existing `Concert`. `user_split_timestamps`
    /// is passed separately because it is not stored on `Concert` itself (it
    /// lives in the separate split-timestamps table/column).
    pub fn for_concert(
        working_dir: &'a Path,
        concert: &'a crate::model::Concert,
        user_split_timestamps: Option<&'a [concert_types::SongTimestamp]>,
    ) -> Self {
        Self {
            working_dir,
            concert_id: concert.id,
            album: concert.album.as_deref(),
            artist: concert.artist.as_deref(),
            downloaded_at: concert.downloaded_at.as_deref(),
            split_succeeded: concert.split_at.is_some(),
            set_list: &concert.set_list,
            tracks_present: &concert.tracks_present,
            tracks_liked: &concert.tracks_liked,
            user_split_timestamps,
            media_duration: concert.media_duration,
        }
    }

    fn log_context(&self) -> (i64, Option<&str>, Option<&str>) {
        (self.concert_id, self.album, self.artist)
    }

    /// The downloaded source file on disk, if any.
    pub fn find_downloaded_file(&self) -> Option<PathBuf> {
        let album = self.album?;
        find_downloaded_file(self.working_dir, album)
    }

    /// Observe the downloaded source without converting filesystem failures
    /// into absence.
    pub fn try_find_downloaded_file(&self) -> std::io::Result<Option<PathBuf>> {
        let Some(album) = self.album else {
            return Ok(None);
        };
        try_find_downloaded_file(self.working_dir, album)
    }

    /// Observe all Published Concert Split facts under one shared publication
    /// lock. The preliminary full directory scan makes unreadable entries an
    /// error instead of letting the legacy convenience helpers silently treat
    /// them as absent.
    pub fn try_published_snapshot(&self) -> anyhow::Result<PublishedMediaSnapshot> {
        let Some(album) = self.album else {
            return Ok(PublishedMediaSnapshot {
                tracks_present_on_disk: vec![false; self.set_list.len()],
                reconstruction_items: Vec::new(),
                source_redundant: false,
            });
        };
        let directory = concert_dir(self.working_dir, album);
        match directory.try_exists() {
            Ok(false) => {
                return Ok(PublishedMediaSnapshot {
                    tracks_present_on_disk: vec![false; self.set_list.len()],
                    reconstruction_items: Vec::new(),
                    source_redundant: false,
                });
            }
            Ok(true) => {}
            Err(error) => return Err(error.into()),
        }
        live_set_splitter::publication::with_shared_lock(&directory, || {
            let mut files = BTreeSet::new();
            for entry in std::fs::read_dir(&directory)? {
                let entry = entry?;
                if entry.file_type()?.is_file() {
                    if let Some(filename) = entry.file_name().to_str() {
                        files.insert(filename.to_string());
                    }
                }
            }
            let find_track = |title: &str| captured_track_file(&files, title);
            let find_interlude = |index| captured_interlude_file(&files, index);
            let tracks_present_on_disk = self
                .set_list
                .iter()
                .map(|title| find_track(title).is_some())
                .collect();
            let reconstruction_items = if self.split_succeeded {
                build_reconstruction_with_lookup(
                    self.set_list,
                    self.tracks_present,
                    self.tracks_liked,
                    self.user_split_timestamps,
                    self.media_duration,
                    find_track,
                    find_interlude,
                )
            } else {
                Vec::new()
            };
            let source_redundant = self.split_succeeded
                && captured_source_present(&files, album)
                && self.media_duration.is_some()
                && self.user_split_timestamps.is_some()
                && self.tracks_present.iter().all(|present| *present)
                && concert_types::derive_interludes(
                    self.user_split_timestamps.unwrap_or_default(),
                    self.media_duration.unwrap_or_default(),
                )
                .iter()
                .all(|interlude| captured_interlude_file(&files, interlude.index).is_some());
            Ok(PublishedMediaSnapshot {
                tracks_present_on_disk,
                reconstruction_items,
                source_redundant,
            })
        })
    }

    /// The split-track file for `title`, if any.
    pub fn find_track_file(&self, title: &str) -> Option<String> {
        let album = self.album?;
        self.with_published_split(None, || find_track_file(self.working_dir, album, title))
    }

    /// The interlude file for `index`, if any.
    pub fn find_interlude_track_file(&self, index: usize) -> Option<String> {
        let album = self.album?;
        self.with_published_split(None, || {
            find_interlude_track_file(self.working_dir, album, index)
        })
    }

    /// Whether the interlude file for `index` exists on disk.
    pub fn find_interlude_file(&self, index: usize) -> bool {
        self.find_interlude_track_file(index).is_some()
    }

    /// Filesystem presence for every title in `set_list`, in order.
    pub fn tracks_present_on_disk(&self) -> Vec<bool> {
        let Some(album) = self.album else {
            return vec![false; self.set_list.len()];
        };
        self.with_published_split(vec![false; self.set_list.len()], || {
            tracks_present_on_disk(self.working_dir, album, self.set_list)
        })
    }

    /// Whether every title in `set_list` currently has a file on disk.
    pub fn all_tracks_present_on_disk(&self) -> bool {
        let Some(album) = self.album else {
            return self.set_list.is_empty();
        };
        self.with_published_split(false, || {
            all_tracks_present_on_disk(self.working_dir, album, self.set_list)
        })
    }

    /// Whether the source file is fully redundant (safe to delete) given
    /// `tracks_present`, user split timestamps, and `media_duration`. See
    /// [`source_redundant`] for the exact fail-closed rules.
    pub fn source_redundant(&self) -> bool {
        if !self.split_succeeded {
            return false;
        }
        let Some(album) = self.album else {
            return false;
        };
        self.with_published_split(false, || {
            source_redundant(
                self.working_dir,
                album,
                self.tracks_present,
                self.user_split_timestamps,
                self.media_duration,
            )
        })
    }

    /// The ordered reconstruction-playback sequence for whole-concert
    /// playback once the source file is gone. See [`build_reconstruction`].
    pub fn reconstruction_items(&self) -> Vec<PlaybackItem> {
        if !self.split_succeeded {
            return Vec::new();
        }
        let Some(album) = self.album else {
            return Vec::new();
        };
        self.with_published_split(Vec::new(), || {
            build_reconstruction(
                self.working_dir,
                album,
                self.set_list,
                self.tracks_present,
                self.tracks_liked,
                self.user_split_timestamps,
                self.media_duration,
            )
        })
    }

    /// Whether "Play concert" is meaningful: either the source file is present
    /// (whole-album mode) or reconstruction has at least one playable item.
    pub fn can_play_concert(&self) -> bool {
        let (concert_id, album, artist) = self.log_context();
        tracing::debug!(concert_id, album, artist, "can_play_concert");
        if self.find_downloaded_file().is_some() {
            return true;
        }
        if self.downloaded_at.is_some() {
            // DB says downloaded but the source file is gone — diagnostic only;
            // `playback::PlaybackLookupError::MarkedDownloadedButMissing` is
            // the policy surface for this state, not this method.
            tracing::debug!(concert_id, "marked downloaded but source file missing");
        }
        !self.reconstruction_items().is_empty()
    }

    /// Per-track availability/video/liked facts for the track-details sidebar.
    /// See [`list_all_track_details`].
    pub fn track_details(&self) -> Vec<TrackDetailItem> {
        let Some(album) = self.album else {
            return Vec::new();
        };
        self.with_published_split(Vec::new(), || {
            list_all_track_details(
                self.working_dir,
                album,
                self.set_list,
                self.tracks_present,
                self.tracks_liked,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_concert_dir(working_dir: &Path, album: &str) -> PathBuf {
        let cd = concert_dir(working_dir, album);
        std::fs::create_dir_all(&cd).unwrap();
        cd
    }

    /// Install a Published Concert Split the way the splitter does: files moved
    /// from a staging sibling into the canonical directory, with a manifest
    /// naming them. Publication validates its staging directory, so it has to be
    /// a `.concert-split-staging-*` sibling of the canonical directory.
    fn publish(canonical_dir: &Path, files: &[(&str, &[u8])]) {
        std::fs::create_dir_all(canonical_dir).unwrap();
        let parent = canonical_dir.parent().unwrap();
        let staging = tempfile::Builder::new()
            .prefix(".concert-split-staging-")
            .tempdir_in(parent)
            .unwrap();
        for (name, bytes) in files {
            std::fs::write(staging.path().join(name), bytes).unwrap();
        }
        live_set_splitter::publication::publish(
            &live_set_splitter::publication::PublicationRequest {
                canonical_dir: canonical_dir.to_path_buf(),
                staging_dir: staging.path().to_path_buf(),
                replacement_files: files.iter().map(|(name, _)| PathBuf::from(name)).collect(),
            },
        )
        .unwrap();
    }

    fn published_manifest(canonical_dir: &Path) -> String {
        std::fs::read_to_string(canonical_dir.join(live_set_splitter::publication::MANIFEST_NAME))
            .unwrap()
    }

    // ---------- find_downloaded_file ----------

    #[test]
    fn find_downloaded_file_returns_match_for_known_extension() {
        let dir = tempfile::tempdir().unwrap();
        let cd = make_concert_dir(dir.path(), "Foo Album");
        std::fs::File::create(cd.join("Foo Album.mp4")).unwrap();
        let found = find_downloaded_file(dir.path(), "Foo Album").unwrap();
        assert_eq!(found, cd.join("Foo Album.mp4"));
    }

    #[test]
    fn find_downloaded_file_ignores_json_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let cd = make_concert_dir(dir.path(), "Foo Album");
        std::fs::File::create(cd.join("Foo Album.json")).unwrap();
        std::fs::File::create(cd.join("Foo Album.mp4")).unwrap();
        let found = find_downloaded_file(dir.path(), "Foo Album").unwrap();
        assert_eq!(found, cd.join("Foo Album.mp4"));
    }

    #[test]
    fn find_downloaded_file_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_downloaded_file(dir.path(), "Foo Album").is_none());
    }

    #[test]
    fn find_downloaded_file_handles_colons_in_album() {
        let dir = tempfile::tempdir().unwrap();
        let cd = make_concert_dir(dir.path(), "A: B");
        std::fs::File::create(cd.join("A B.mp4")).unwrap();
        let found = find_downloaded_file(dir.path(), "A: B").unwrap();
        assert_eq!(found, cd.join("A B.mp4"));
    }

    #[test]
    fn find_downloaded_file_returns_none_when_only_json_exists() {
        let dir = tempfile::tempdir().unwrap();
        let cd = make_concert_dir(dir.path(), "Foo Album");
        std::fs::File::create(cd.join("Foo Album.json")).unwrap();
        assert!(find_downloaded_file(dir.path(), "Foo Album").is_none());
    }

    #[test]
    fn find_downloaded_file_skips_jpg_preview_image() {
        let dir = tempfile::tempdir().unwrap();
        let cd = make_concert_dir(dir.path(), "Foo Album");
        std::fs::File::create(cd.join("Foo Album.jpg")).unwrap();
        std::fs::File::create(cd.join("Foo Album.mp4")).unwrap();
        let found = find_downloaded_file(dir.path(), "Foo Album").unwrap();
        assert_eq!(found, cd.join("Foo Album.mp4"));
    }

    #[test]
    fn find_downloaded_file_returns_none_when_only_image_exists() {
        let dir = tempfile::tempdir().unwrap();
        let cd = make_concert_dir(dir.path(), "Foo Album");
        std::fs::File::create(cd.join("Foo Album.jpg")).unwrap();
        assert!(find_downloaded_file(dir.path(), "Foo Album").is_none());
    }

    #[test]
    fn find_downloaded_file_accepts_uppercase_extension() {
        let dir = tempfile::tempdir().unwrap();
        let cd = make_concert_dir(dir.path(), "Foo Album");
        std::fs::File::create(cd.join("Foo Album.MP4")).unwrap();
        let found = find_downloaded_file(dir.path(), "Foo Album").unwrap();
        assert_eq!(found, cd.join("Foo Album.MP4"));
    }

    #[test]
    fn find_downloaded_file_returns_none_for_missing_album() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_downloaded_file(dir.path(), "No Such Album").is_none());
    }

    // ---------- find_track_file ----------

    #[test]
    fn find_track_file_finds_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Test Album";
        let cd = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(cd.join("Song One.mp4"), b"data").unwrap();

        assert_eq!(
            find_track_file(dir.path(), album, "Song One"),
            Some("Song One.mp4".to_string())
        );
    }

    #[test]
    fn find_track_file_prefers_mp4_over_m4a() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Test Album";
        let cd = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(cd.join("Song.mp4"), b"data").unwrap();
        std::fs::write(cd.join("Song.m4a"), b"data").unwrap();

        assert_eq!(
            find_track_file(dir.path(), album, "Song"),
            Some("Song.mp4".to_string())
        );
    }

    #[test]
    fn find_track_file_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_track_file(dir.path(), "No Album", "No Song"), None);
    }

    #[test]
    fn find_track_file_ignores_non_media_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Test Album";
        let cd = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(cd.join("Song.json"), b"{}").unwrap();
        std::fs::write(cd.join("Song.jpg"), b"data").unwrap();
        assert_eq!(find_track_file(dir.path(), album, "Song"), None);
    }

    // ---------- list_tracks / list_all_tracks ----------

    #[test]
    fn list_tracks_returns_songs_with_files() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Test Album";
        let cd = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::File::create(cd.join("Song One.mp4")).unwrap();
        std::fs::File::create(cd.join("Song One.m4a")).unwrap();
        std::fs::File::create(cd.join("Song Three.m4a")).unwrap();

        let set_list = vec![
            "Song One".to_string(),
            "Song Two".to_string(),
            "Song Three".to_string(),
        ];
        let tracks = list_tracks(dir.path(), album, &set_list);
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].index, 0);
        assert_eq!(tracks[0].title, "Song One");
        assert!(tracks[0].available);
        assert_eq!(tracks[1].index, 2);
        assert_eq!(tracks[1].title, "Song Three");
        assert!(tracks[1].available);
    }

    #[test]
    fn list_tracks_returns_empty_when_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let set_list = vec!["Song A".to_string()];
        let tracks = list_tracks(dir.path(), "No Album", &set_list);
        assert!(tracks.is_empty());
    }

    #[test]
    fn list_tracks_defaults_liked_false() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Test Album";
        let cd = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::File::create(cd.join("Song One.m4a")).unwrap();

        let set_list = vec!["Song One".to_string()];
        let tracks = list_tracks(dir.path(), album, &set_list);
        assert_eq!(tracks.len(), 1);
        assert!(!tracks[0].liked);
    }

    #[test]
    fn list_all_tracks_includes_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Test Album";
        let cd = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::File::create(cd.join("Song One.m4a")).unwrap();

        let set_list = vec!["Song One".to_string(), "Song Two".to_string()];
        let tracks = list_all_tracks(dir.path(), album, &set_list);
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].title, "Song One");
        assert!(tracks[0].available);
        assert_eq!(tracks[1].title, "Song Two");
        assert!(!tracks[1].available);
    }

    #[test]
    fn list_all_tracks_defaults_liked_false() {
        let dir = tempfile::tempdir().unwrap();
        let set_list = vec!["Song A".to_string()];
        let tracks = list_all_tracks(dir.path(), "No Album", &set_list);
        assert_eq!(tracks.len(), 1);
        assert!(!tracks[0].liked);
    }

    // ---------- all-tracks-present ----------

    #[test]
    fn all_tracks_present_on_disk_true_when_all_files_exist() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Test Album";
        let cd = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(cd.join("Song One.m4a"), b"data").unwrap();
        std::fs::write(cd.join("Song Two.m4a"), b"data").unwrap();
        let set_list = vec!["Song One".to_string(), "Song Two".to_string()];
        assert!(all_tracks_present_on_disk(dir.path(), album, &set_list));
    }

    #[test]
    fn all_tracks_present_on_disk_false_when_one_missing() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Test Album";
        let cd = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(cd.join("Song One.m4a"), b"data").unwrap();
        let set_list = vec!["Song One".to_string(), "Song Two".to_string()];
        assert!(!all_tracks_present_on_disk(dir.path(), album, &set_list));
    }

    #[test]
    fn all_tracks_present_on_disk_true_for_empty_set_list() {
        let dir = tempfile::tempdir().unwrap();
        assert!(all_tracks_present_on_disk(dir.path(), "No Album", &[]));
    }

    #[test]
    fn tracks_present_on_disk_matches_per_title_presence() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Test Album";
        let cd = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(cd.join("Song One.m4a"), b"data").unwrap();
        let set_list = vec!["Song One".to_string(), "Song Two".to_string()];
        assert_eq!(
            tracks_present_on_disk(dir.path(), album, &set_list),
            vec![true, false]
        );
    }

    // ---------- source_redundant ----------

    fn make_song(start: f64, end: f64) -> concert_types::SongTimestamp {
        concert_types::SongTimestamp {
            title: "s".to_string(),
            start_time: start,
            end_time: end,
            duration: end - start,
        }
    }

    #[test]
    fn source_redundant_fails_closed_when_no_media_duration() {
        let dir = tempfile::tempdir().unwrap();
        let songs = vec![make_song(0.0, 100.0)];
        assert!(!source_redundant(
            dir.path(),
            "Album",
            &[true],
            Some(&songs),
            None
        ));
    }

    #[test]
    fn source_redundant_fails_closed_when_no_user_timestamps() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!source_redundant(
            dir.path(),
            "Album",
            &[true],
            None,
            Some(100.0)
        ));
    }

    // Writes `{album}.mp4` into the concert dir so `find_downloaded_file`
    // (source_redundant's first gate) sees a source present, letting the
    // rest of the test exercise the coverage logic it actually names.
    fn write_source_file(working_dir: &Path, album: &str) -> PathBuf {
        let cd = concert_dir(working_dir, album);
        std::fs::create_dir_all(&cd).unwrap();
        let path = cd.join(format!("{album}.mp4"));
        std::fs::write(&path, b"data").unwrap();
        path
    }

    #[test]
    fn source_redundant_false_when_source_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        // Full coverage, no gaps — would be redundant if a source file existed.
        let songs = vec![make_song(0.0, 100.0)];
        assert!(!source_redundant(
            dir.path(),
            "No Source Album",
            &[true],
            Some(&songs),
            Some(100.0)
        ));
    }

    #[test]
    fn source_redundant_false_when_a_song_track_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        write_source_file(dir.path(), "Album");
        let songs = vec![make_song(0.0, 50.0), make_song(50.0, 100.0)];
        // tracks_present says second track is missing
        assert!(!source_redundant(
            dir.path(),
            "Album",
            &[true, false],
            Some(&songs),
            Some(100.0)
        ));
    }

    #[test]
    fn source_redundant_true_when_full_coverage_no_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Full Album";
        write_source_file(dir.path(), album);

        // Songs cover [0, 200] with no gaps — no interlude files needed.
        let songs = vec![make_song(0.0, 100.0), make_song(100.0, 200.0)];
        assert!(source_redundant(
            dir.path(),
            album,
            &[true, true],
            Some(&songs),
            Some(200.0)
        ));
    }

    #[test]
    fn source_redundant_false_when_interlude_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Gap Album";
        write_source_file(dir.path(), album);

        // Song covers [5, 100]; head gap [0, 5) needs an interlude file.
        let songs = vec![make_song(5.0, 100.0)];
        assert!(!source_redundant(
            dir.path(),
            album,
            &[true],
            Some(&songs),
            Some(100.0)
        ));
    }

    #[test]
    fn source_redundant_true_when_all_interlude_files_present() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Gap Album";
        let cd = write_source_file(dir.path(), album)
            .parent()
            .unwrap()
            .to_path_buf();

        // Head gap [0, 5] + tail gap [95, 100] — two interlude files needed.
        let songs = vec![make_song(5.0, 95.0)];
        // Write interlude_01.mp4 (head) and interlude_02.mp4 (tail).
        std::fs::write(cd.join("interlude_01.mp4"), b"data").unwrap();
        std::fs::write(cd.join("interlude_02.mp4"), b"data").unwrap();

        assert!(source_redundant(
            dir.path(),
            album,
            &[true],
            Some(&songs),
            Some(100.0)
        ));
    }

    #[test]
    fn source_redundant_false_when_missing_album() {
        // No album/source dir at all — must fail closed rather than panic.
        let dir = tempfile::tempdir().unwrap();
        let songs = vec![make_song(0.0, 100.0)];
        assert!(!source_redundant(
            dir.path(),
            "Nonexistent Album",
            &[true],
            Some(&songs),
            Some(100.0)
        ));
    }

    // ---------- interlude lookup ----------

    #[test]
    fn find_interlude_file_finds_mp4_and_m4a() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Test";
        let cd = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&cd).unwrap();

        assert!(!find_interlude_file(dir.path(), album, 1));
        std::fs::write(cd.join("interlude_01.m4a"), b"audio").unwrap();
        assert!(find_interlude_file(dir.path(), album, 1));
    }

    #[test]
    fn find_interlude_track_file_returns_filename() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Test";
        let cd = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&cd).unwrap();

        assert_eq!(find_interlude_track_file(dir.path(), album, 1), None);
        std::fs::write(cd.join("interlude_01.m4a"), b"audio").unwrap();
        assert_eq!(
            find_interlude_track_file(dir.path(), album, 1),
            Some("interlude_01.m4a".to_string())
        );
    }

    #[test]
    fn find_interlude_track_file_prefers_mp4() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Test";
        let cd = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&cd).unwrap();

        std::fs::write(cd.join("interlude_01.mp4"), b"video").unwrap();
        std::fs::write(cd.join("interlude_01.m4a"), b"audio").unwrap();
        // mp4 is probed first
        assert_eq!(
            find_interlude_track_file(dir.path(), album, 1),
            Some("interlude_01.mp4".to_string())
        );
    }

    // ---------- build_reconstruction ----------

    /// Helper: create a concert directory and write stub files for the given song
    /// and interlude filenames.
    fn setup_reconstruction_dir(
        dir: &Path,
        album: &str,
        songs: &[&str],
        interludes: &[(usize, &str)], // (1-based index, ext)
    ) -> PathBuf {
        let cd = concert_dir(dir, album);
        std::fs::create_dir_all(&cd).unwrap();
        for s in songs {
            std::fs::write(cd.join(format!("{s}.m4a")), b"data").unwrap();
        }
        for (idx, ext) in interludes {
            let stem = concert_types::interlude_filename_stem(*idx);
            std::fs::write(cd.join(format!("{stem}.{ext}")), b"data").unwrap();
        }
        cd
    }

    fn ts(start: f64, end: f64, title: &str) -> concert_types::SongTimestamp {
        concert_types::SongTimestamp {
            title: title.to_string(),
            start_time: start,
            end_time: end,
            duration: end - start,
        }
    }

    #[test]
    fn build_reconstruction_no_user_ts_falls_back_to_songs_only() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Album";
        setup_reconstruction_dir(dir.path(), album, &["Song A", "Song B"], &[]);
        let set_list = vec!["Song A".to_string(), "Song B".to_string()];
        let tracks_present = vec![true, true];
        let items = build_reconstruction(
            dir.path(),
            album,
            &set_list,
            &tracks_present,
            &[],
            None,
            Some(100.0),
        );
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Song A");
        assert!(matches!(
            items[0].kind,
            PlaybackItemKind::Song { track_index: 0, .. }
        ));
        assert_eq!(items[1].title, "Song B");
        assert!(matches!(
            items[1].kind,
            PlaybackItemKind::Song { track_index: 1, .. }
        ));
    }

    #[test]
    fn build_reconstruction_no_user_ts_excludes_missing_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Album";
        setup_reconstruction_dir(dir.path(), album, &["Song A"], &[]); // only Song A on disk
        let set_list = vec!["Song A".to_string(), "Song B".to_string()];
        let tracks_present = vec![true, false];
        let items = build_reconstruction(
            dir.path(),
            album,
            &set_list,
            &tracks_present,
            &[],
            None,
            Some(100.0),
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Song A");
    }

    #[test]
    fn build_reconstruction_songs_and_interludes_in_time_order() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Album";
        // Head gap [0,5), songs at [5,50) and [60,100), inter-song gap [50,60), no tail gap.
        setup_reconstruction_dir(
            dir.path(),
            album,
            &["Song A", "Song B"],
            &[(1, "m4a"), (2, "m4a")],
        );
        let set_list = vec!["Song A".to_string(), "Song B".to_string()];
        let songs_ts = vec![ts(5.0, 50.0, "Song A"), ts(60.0, 100.0, "Song B")];
        let items = build_reconstruction(
            dir.path(),
            album,
            &set_list,
            &[true, true],
            &[],
            Some(&songs_ts),
            Some(100.0),
        );
        // Expected order: interlude_01, Song A, interlude_02, Song B
        assert_eq!(items.len(), 4);
        assert!(matches!(
            items[0].kind,
            PlaybackItemKind::Interlude { index: 1 }
        ));
        assert!(matches!(
            items[1].kind,
            PlaybackItemKind::Song { track_index: 0, .. }
        ));
        assert!(matches!(
            items[2].kind,
            PlaybackItemKind::Interlude { index: 2 }
        ));
        assert!(matches!(
            items[3].kind,
            PlaybackItemKind::Song { track_index: 1, .. }
        ));
    }

    #[test]
    fn build_reconstruction_deleted_song_drops_preceding_interlude() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Album";
        // Gap before Song B; Song B deleted.
        setup_reconstruction_dir(
            dir.path(),
            album,
            &["Song A"],   // Song B not on disk
            &[(1, "m4a")], // interlude_01 before Song B
        );
        let set_list = vec!["Song A".to_string(), "Song B".to_string()];
        let songs_ts = vec![ts(0.0, 50.0, "Song A"), ts(60.0, 100.0, "Song B")];
        let items = build_reconstruction(
            dir.path(),
            album,
            &set_list,
            &[true, false],
            &[],
            Some(&songs_ts),
            Some(100.0),
        );
        // interlude_01 is before deleted Song B → should be dropped
        assert_eq!(items.len(), 1);
        assert!(matches!(
            items[0].kind,
            PlaybackItemKind::Song { track_index: 0, .. }
        ));
    }

    #[test]
    fn build_reconstruction_tail_interlude_kept_when_last_song_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Album";
        // Song A present, Song B deleted; tail interlude after Song A.
        setup_reconstruction_dir(
            dir.path(),
            album,
            &["Song A"],
            &[(1, "m4a")], // tail interlude
        );
        let set_list = vec!["Song A".to_string(), "Song B".to_string()];
        // Song A ends at 40; Song B from 50 to 100 deleted; tail gap [40,50).
        let songs_ts = vec![ts(0.0, 40.0, "Song A"), ts(50.0, 100.0, "Song B")];
        let items = build_reconstruction(
            dir.path(),
            album,
            &set_list,
            &[true, false],
            &[],
            Some(&songs_ts),
            Some(100.0),
        );
        // interlude_01 is at [40,50) — before deleted Song B → dropped by deleted-song rule
        // (the interlude BEFORE a deleted song is dropped)
        assert_eq!(items.len(), 1);
        assert!(matches!(
            items[0].kind,
            PlaybackItemKind::Song { track_index: 0, .. }
        ));
    }

    #[test]
    fn build_reconstruction_interlude_after_deleted_song_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Album";
        // Song A deleted; interlude_01 after Song A before Song B; Song B present.
        // interlude_01 is AFTER the deleted song, so it should be kept.
        setup_reconstruction_dir(dir.path(), album, &["Song B"], &[(1, "m4a")]);
        let set_list = vec!["Song A".to_string(), "Song B".to_string()];
        // Song A [0,50), gap [50,60), Song B [60,100)
        let songs_ts = vec![ts(0.0, 50.0, "Song A"), ts(60.0, 100.0, "Song B")];
        let items = build_reconstruction(
            dir.path(),
            album,
            &set_list,
            &[false, true],
            &[],
            Some(&songs_ts),
            Some(100.0),
        );
        // Song A is deleted. interlude_01 is at [50,60) — AFTER deleted Song A.
        // Its next song is Song B (kept), so it should be included.
        assert_eq!(items.len(), 2);
        assert!(matches!(
            items[0].kind,
            PlaybackItemKind::Interlude { index: 1 }
        ));
        assert!(matches!(
            items[1].kind,
            PlaybackItemKind::Song { track_index: 1, .. }
        ));
    }

    #[test]
    fn build_reconstruction_missing_interlude_file_skips_it() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Album";
        // Both songs present but interlude file not on disk.
        setup_reconstruction_dir(dir.path(), album, &["Song A", "Song B"], &[]);
        let set_list = vec!["Song A".to_string(), "Song B".to_string()];
        let songs_ts = vec![ts(0.0, 40.0, "Song A"), ts(50.0, 100.0, "Song B")];
        let items = build_reconstruction(
            dir.path(),
            album,
            &set_list,
            &[true, true],
            &[],
            Some(&songs_ts),
            Some(100.0),
        );
        // interlude_01 file missing → skipped
        assert_eq!(items.len(), 2);
        assert!(matches!(
            items[0].kind,
            PlaybackItemKind::Song { track_index: 0, .. }
        ));
        assert!(matches!(
            items[1].kind,
            PlaybackItemKind::Song { track_index: 1, .. }
        ));
    }

    #[test]
    fn build_reconstruction_all_songs_deleted_with_tail_interlude() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Album";
        // No songs on disk; only a tail interlude (gap after the only song).
        setup_reconstruction_dir(dir.path(), album, &[], &[(1, "m4a")]);
        let set_list = vec!["Song A".to_string()];
        let songs_ts = vec![ts(0.0, 90.0, "Song A")];
        // tail interlude [90, 100)
        let items = build_reconstruction(
            dir.path(),
            album,
            &set_list,
            &[false],
            &[],
            Some(&songs_ts),
            Some(100.0),
        );
        // Song A deleted → interlude before it (none here; it's a tail) is kept;
        // but tail interlude has no next song → kept regardless.
        // However Song A is deleted (tracks_present[0]=false), so Song A slot is dropped.
        // The tail interlude [90,100) — its next song would be... none (tail). So it's kept.
        // But wait: the interlude is AFTER Song A (not before it), so the deleted-song rule
        // doesn't apply. Tail interlude should be included.
        assert_eq!(items.len(), 1);
        assert!(matches!(
            items[0].kind,
            PlaybackItemKind::Interlude { index: 1 }
        ));
    }

    #[test]
    fn build_reconstruction_empty_when_nothing_playable() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Album";
        // No files at all.
        setup_reconstruction_dir(dir.path(), album, &[], &[]);
        let set_list = vec!["Song A".to_string()];
        let songs_ts = vec![ts(0.0, 100.0, "Song A")];
        let items = build_reconstruction(
            dir.path(),
            album,
            &set_list,
            &[false],
            &[],
            Some(&songs_ts),
            Some(100.0),
        );
        assert!(items.is_empty());
    }

    #[test]
    fn build_reconstruction_liked_flag_propagates() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Album";
        setup_reconstruction_dir(dir.path(), album, &["Song A", "Song B"], &[]);
        let set_list = vec!["Song A".to_string(), "Song B".to_string()];
        let tracks_liked = vec![false, true];
        let songs_ts = vec![ts(0.0, 50.0, "Song A"), ts(50.0, 100.0, "Song B")];
        let items = build_reconstruction(
            dir.path(),
            album,
            &set_list,
            &[true, true],
            &tracks_liked,
            Some(&songs_ts),
            Some(100.0),
        );
        assert_eq!(items.len(), 2);
        assert!(matches!(
            items[0].kind,
            PlaybackItemKind::Song { liked: false, .. }
        ));
        assert!(matches!(
            items[1].kind,
            PlaybackItemKind::Song { liked: true, .. }
        ));
    }

    // ---------- list_all_track_details ----------

    #[test]
    fn list_all_track_details_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let set_list = vec!["Song A".to_string(), "Song B".to_string()];
        let tracks_present = vec![false, false];
        let tracks_liked = vec![true, false];
        let items = list_all_track_details(
            dir.path(),
            "Album",
            &set_list,
            &tracks_present,
            &tracks_liked,
        );
        assert_eq!(items.len(), 2);
        assert!(!items[0].available);
        assert!(!items[0].is_video);
        assert!(items[0].liked);
        assert!(!items[1].available);
        assert!(!items[1].liked);
    }

    #[test]
    fn list_all_track_details_with_audio_file() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Test Album";
        let album_dir = dir
            .path()
            .join("concerts")
            .join(crate::model::sanitize_album(album));
        std::fs::create_dir_all(&album_dir).unwrap();
        std::fs::write(album_dir.join("Song A.m4a"), b"").unwrap();
        let set_list = vec!["Song A".to_string(), "Song B".to_string()];
        let tracks_present = vec![true, false];
        let tracks_liked = vec![false, true];
        let items =
            list_all_track_details(dir.path(), album, &set_list, &tracks_present, &tracks_liked);
        assert_eq!(items.len(), 2);
        assert!(items[0].available);
        assert!(!items[0].is_video, "m4a is not video");
        assert!(!items[0].liked);
        assert!(!items[1].available);
        assert!(items[1].liked);
    }

    #[test]
    fn list_all_track_details_with_video_file() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Video Album";
        let album_dir = dir
            .path()
            .join("concerts")
            .join(crate::model::sanitize_album(album));
        std::fs::create_dir_all(&album_dir).unwrap();
        std::fs::write(album_dir.join("Song A.mp4"), b"").unwrap();
        let set_list = vec!["Song A".to_string()];
        let tracks_present = vec![true];
        let items = list_all_track_details(dir.path(), album, &set_list, &tracks_present, &[]);
        assert_eq!(items.len(), 1);
        assert!(items[0].available);
        assert!(items[0].is_video, "mp4 is video");
    }

    #[test]
    fn list_all_track_details_is_video_false_when_unavailable() {
        // Even if a file exists on disk, is_video is false when tracks_present says unavailable.
        let dir = tempfile::tempdir().unwrap();
        let album = "Mixed Album";
        let album_dir = dir
            .path()
            .join("concerts")
            .join(crate::model::sanitize_album(album));
        std::fs::create_dir_all(&album_dir).unwrap();
        std::fs::write(album_dir.join("Song A.mp4"), b"").unwrap();
        let set_list = vec!["Song A".to_string()];
        let tracks_present = vec![false]; // DB says unavailable
        let items = list_all_track_details(dir.path(), album, &set_list, &tracks_present, &[]);
        assert!(!items[0].available);
        assert!(
            !items[0].is_video,
            "is_video skips filesystem probe when unavailable"
        );
    }

    #[test]
    fn build_reconstruction_no_duration_falls_back_to_songs_only() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Album";
        setup_reconstruction_dir(dir.path(), album, &["Song A"], &[]);
        let set_list = vec!["Song A".to_string()];
        let songs_ts = vec![ts(0.0, 100.0, "Song A")];
        let items = build_reconstruction(
            dir.path(),
            album,
            &set_list,
            &[true],
            &[],
            Some(&songs_ts),
            None,
        );
        assert_eq!(items.len(), 1);
        assert!(matches!(
            items[0].kind,
            PlaybackItemKind::Song { track_index: 0, .. }
        ));
    }

    // ---------- ConcertMediaInventory ----------

    fn bare_concert(album: Option<&str>, set_list: Vec<String>) -> crate::model::Concert {
        let n = set_list.len();
        crate::model::Concert {
            id: 1,
            source_url: "https://npr.org/c/1".to_string(),
            title: "Test".to_string(),
            concert_date: None,
            teaser: None,
            artist: Some("Test Artist".to_string()),
            album: album.map(str::to_string),
            description: None,
            set_list,
            musicians: vec![],
            ignored: false,
            wanted: false,
            notes: None,
            download_started_at: None,
            downloaded_at: None,
            downloaded_extension: None,
            download_errors: vec![],
            split_started_at: None,
            split_at: None,
            split_errors: vec![],
            archive_started_at: None,
            archived_at: None,
            archive_errors: vec![],
            inserted_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: None,
            metadata_scraped_at: None,
            tracks_present: vec![false; n],
            tracks_liked: vec![false; n],
            media_duration: None,
        }
    }

    #[test]
    fn inventory_find_downloaded_file_accepts_uppercase_extension() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Foo Album";
        let cd = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(cd.join("Foo Album.MP4"), b"data").unwrap();
        let concert = bare_concert(Some(album), vec![]);
        let inv = ConcertMediaInventory::for_concert(dir.path(), &concert, None);
        assert_eq!(inv.find_downloaded_file(), Some(cd.join("Foo Album.MP4")));
    }

    #[test]
    fn inventory_find_downloaded_file_ignores_sidecar_and_image() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Foo Album";
        let cd = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(cd.join("Foo Album.json"), b"{}").unwrap();
        std::fs::write(cd.join("Foo Album.jpg"), b"data").unwrap();
        std::fs::write(cd.join("Foo Album.mp4"), b"data").unwrap();
        let concert = bare_concert(Some(album), vec![]);
        let inv = ConcertMediaInventory::for_concert(dir.path(), &concert, None);
        assert_eq!(inv.find_downloaded_file(), Some(cd.join("Foo Album.mp4")));
    }

    #[test]
    fn inventory_find_downloaded_file_none_when_multiple_media_present_returns_some_match() {
        // Current behavior: whichever known-extension file is found first wins —
        // no deterministic source-extension priority is introduced.
        let dir = tempfile::tempdir().unwrap();
        let album = "Foo Album";
        let cd = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(cd.join("Foo Album.mp4"), b"data").unwrap();
        std::fs::write(cd.join("Foo Album.m4a"), b"data").unwrap();
        let concert = bare_concert(Some(album), vec![]);
        let inv = ConcertMediaInventory::for_concert(dir.path(), &concert, None);
        let found = inv.find_downloaded_file();
        assert!(found == Some(cd.join("Foo Album.mp4")) || found == Some(cd.join("Foo Album.m4a")));
    }

    #[test]
    fn inventory_find_track_file_skips_non_browser_playable_extension() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Album";
        let cd = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(cd.join("Song.mkv"), b"data").unwrap();
        let concert = bare_concert(Some(album), vec!["Song".to_string()]);
        let inv = ConcertMediaInventory::for_concert(dir.path(), &concert, None);
        // find_track_file still finds it (mkv is a known extension)...
        assert_eq!(inv.find_track_file("Song"), Some("Song.mkv".to_string()));
        // ...but reconstruction excludes it because mkv isn't browser-playable.
        let mut concert = concert;
        concert.tracks_present = vec![true];
        let inv = ConcertMediaInventory::for_concert(dir.path(), &concert, None);
        assert!(inv.reconstruction_items().is_empty());
    }

    #[test]
    fn inventory_missing_album_returns_conservative_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let concert = bare_concert(None, vec!["Song".to_string()]);
        let inv = ConcertMediaInventory::for_concert(dir.path(), &concert, None);
        assert_eq!(inv.find_downloaded_file(), None);
        assert_eq!(inv.find_track_file("Song"), None);
        assert!(!inv.find_interlude_file(1));
        assert!(!inv.all_tracks_present_on_disk());
        assert_eq!(inv.tracks_present_on_disk(), vec![false]);
        assert!(!inv.source_redundant());
        assert!(inv.reconstruction_items().is_empty());
        assert!(!inv.can_play_concert());
        assert!(inv.track_details().is_empty());
    }

    #[test]
    fn inventory_all_tracks_present_on_disk_true_for_empty_set_list() {
        let dir = tempfile::tempdir().unwrap();
        let concert = bare_concert(Some("Album"), vec![]);
        let inv = ConcertMediaInventory::for_concert(dir.path(), &concert, None);
        assert!(inv.all_tracks_present_on_disk());
    }

    #[test]
    fn inventory_can_play_concert_true_when_source_present() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Album";
        let cd = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(cd.join("Album.mp4"), b"data").unwrap();
        let concert = bare_concert(Some(album), vec![]);
        let inv = ConcertMediaInventory::for_concert(dir.path(), &concert, None);
        assert!(inv.can_play_concert());
    }

    #[test]
    fn inventory_can_play_concert_true_via_reconstruction_when_source_gone() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Album";
        let cd = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(cd.join("Song A.m4a"), b"data").unwrap();
        let mut concert = bare_concert(Some(album), vec!["Song A".to_string()]);
        concert.tracks_present = vec![true];
        concert.split_at = Some("2026-07-22T00:00:00Z".to_string());
        let inv = ConcertMediaInventory::for_concert(dir.path(), &concert, None);
        assert!(inv.can_play_concert());
    }

    #[test]
    fn inventory_can_play_concert_false_when_nothing_playable() {
        let dir = tempfile::tempdir().unwrap();
        let concert = bare_concert(Some("Album"), vec!["Song A".to_string()]);
        let inv = ConcertMediaInventory::for_concert(dir.path(), &concert, None);
        assert!(!inv.can_play_concert());
    }

    #[test]
    fn inventory_source_redundant_true_when_full_coverage() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Album";
        write_source_file(dir.path(), album);
        let mut concert = bare_concert(Some(album), vec!["Song A".to_string()]);
        concert.tracks_present = vec![true];
        concert.split_at = Some("2026-07-22T00:00:00Z".to_string());
        concert.media_duration = Some(100.0);
        let songs = vec![make_song(0.0, 100.0)];
        let inv = ConcertMediaInventory::for_concert(dir.path(), &concert, Some(&songs));
        assert!(inv.source_redundant());
    }

    #[test]
    fn inventory_track_details_reports_availability_and_video_facts() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Album";
        let cd = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(cd.join("Song A.mp4"), b"data").unwrap();
        let mut concert = bare_concert(Some(album), vec!["Song A".to_string()]);
        concert.tracks_present = vec![true];
        let inv = ConcertMediaInventory::for_concert(dir.path(), &concert, None);
        let details = inv.track_details();
        assert_eq!(details.len(), 1);
        assert!(details[0].available);
        assert!(details[0].is_video);
    }

    #[test]
    fn recoverable_partial_track_is_available_but_cannot_reconstruct_concert() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Partial Album";
        let cd = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(cd.join("Song A.m4a"), b"partial").unwrap();
        let mut concert = bare_concert(
            Some(album),
            vec!["Song A".to_string(), "Song B".to_string()],
        );
        concert.tracks_present = vec![true, false];
        concert.media_duration = Some(100.0);
        let songs = vec![make_song(0.0, 50.0), make_song(50.0, 100.0)];
        let inventory = ConcertMediaInventory::for_concert(dir.path(), &concert, Some(&songs));

        assert_eq!(
            inventory.find_track_file("Song A"),
            Some("Song A.m4a".to_string())
        );
        assert!(inventory.reconstruction_items().is_empty());
        assert!(!inventory.source_redundant());
    }

    // ---------- remove_track_files / remove_interlude_files ----------

    /// Removal is a mutation of Published Concert Split state, so it keeps the
    /// manifest naming only files that still exist. Without that, the next
    /// Concert Split refuses to back up the prior generation it cannot find, and
    /// the concert could never be resplit.
    #[test]
    fn removing_a_track_keeps_the_published_manifest_true() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Test Album";
        let canonical = concert_dir(dir.path(), album);
        publish(
            &canonical,
            &[("Song One.m4a", b"one"), ("Song Two.m4a", b"two")],
        );

        let removed = remove_track_files(dir.path(), album, "Song Two").unwrap();

        assert_eq!(removed, vec![canonical.join("Song Two.m4a")]);
        let manifest = published_manifest(&canonical);
        assert!(manifest.contains("Song One.m4a"), "{manifest}");
        assert!(
            !manifest.contains("Song Two.m4a"),
            "the manifest must not claim a file the deletion removed: {manifest}"
        );

        // The deletion left the concert resplittable: publishing over the prior
        // generation needs every file the manifest still names.
        publish(
            &canonical,
            &[("Song One.m4a", b"one"), ("Song Two.m4a", b"two")],
        );
        assert_eq!(
            std::fs::read(canonical.join("Song Two.m4a")).unwrap(),
            b"two"
        );
    }

    /// A publication owns the canonical directory while it runs, so a deletion
    /// is refused rather than removing files from under it.
    #[test]
    fn removing_media_during_a_publication_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Test Album";
        let canonical = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&canonical).unwrap();
        std::fs::write(canonical.join("Song One.m4a"), b"one").unwrap();
        let publisher =
            live_set_splitter::publication::ExclusivePublicationLock::acquire(&canonical).unwrap();

        let outcome = remove_track_files(dir.path(), album, "Song One");

        assert!(
            matches!(outcome, Err(MediaRemovalError::PublicationInProgress)),
            "unexpected outcome: {outcome:?}"
        );
        assert!(
            canonical.join("Song One.m4a").exists(),
            "a refused deletion must leave the media alone"
        );

        drop(publisher);
        assert!(remove_track_files(dir.path(), album, "Song One").is_ok());
    }

    /// Some canonical files gone and the rest unremovable is a fault, not a
    /// successful deletion: the caller may not record it as done.
    #[test]
    fn a_partial_removal_is_an_error_rather_than_a_silent_success() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Test Album";
        let canonical = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&canonical).unwrap();
        std::fs::write(canonical.join("Song One.mp4"), b"one").unwrap();
        // An extension the removal probes that is not a removable file.
        std::fs::create_dir_all(canonical.join("Song One.m4a")).unwrap();

        let outcome = remove_track_files(dir.path(), album, "Song One");

        assert!(
            matches!(outcome, Err(MediaRemovalError::Failed(_))),
            "unexpected outcome: {outcome:?}"
        );
    }

    #[test]
    fn removing_an_absent_track_is_a_successful_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Test Album";
        std::fs::create_dir_all(concert_dir(dir.path(), album)).unwrap();

        assert_eq!(
            remove_track_files(dir.path(), album, "Song One").unwrap(),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn removing_an_interlude_byte_extension_matches_discovery() {
        let dir = tempfile::tempdir().unwrap();
        let album = "Test Album";
        let canonical = concert_dir(dir.path(), album);
        std::fs::create_dir_all(&canonical).unwrap();
        std::fs::write(canonical.join("interlude_00.m4a"), b"gap").unwrap();

        let removed = remove_interlude_files(dir.path(), album, 0).unwrap();

        assert_eq!(removed, vec![canonical.join("interlude_00.m4a")]);
        assert!(!find_interlude_file(dir.path(), album, 0));
    }
}
