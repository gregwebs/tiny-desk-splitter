use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Strip colons from album names to produce safe filesystem paths.
/// Mirrors the logic in download.sh and extract.sh.
pub fn sanitize_album(album: &str) -> String {
    album.replace(':', "")
}

/// Sanitize a string for use as a filename. Mirrors the logic in
/// `live-set-song-splitter/src/io.rs` so derived paths match splitter output.
pub fn sanitize_filename(input: &str) -> String {
    let mut sanitized = input
        .replace(
            &['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0'][..],
            "_",
        )
        .replace("__", "_");
    sanitized = sanitized.trim().trim_matches('.').to_string();
    if sanitized.is_empty() {
        sanitized = "untitled".to_string();
    }
    sanitized
}

/// Per-concert directory under `working_dir/concerts/`. All artifacts for a
/// concert (full mp4, split tracks, preview image, metadata json) live here.
pub fn concert_dir(working_dir: &Path, album: &str) -> PathBuf {
    working_dir.join("concerts").join(sanitize_album(album))
}

pub fn is_video_extension(ext: &str) -> bool {
    matches!(ext.to_lowercase().as_str(), "mp4" | "webm")
}

pub fn is_browser_playable(ext: &str) -> bool {
    matches!(
        ext.to_lowercase().as_str(),
        "mp4" | "m4a" | "webm" | "mp3" | "ogg" | "opus" | "wav" | "flac"
    )
}

pub fn find_track_file(working_dir: &Path, album: &str, title: &str) -> Option<String> {
    let stem = sanitize_filename(title);
    let dir = concert_dir(working_dir, album);
    for ext in &[
        "mp4", "m4a", "webm", "mp3", "ogg", "opus", "wav", "flac", "mkv",
    ] {
        let filename = format!("{stem}.{ext}");
        if dir.join(&filename).exists() {
            return Some(filename);
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct TrackInfo {
    pub index: usize,
    pub title: String,
    pub available: bool,
    pub is_video: bool,
    pub liked: bool,
}

fn track_file_extension(dir: &Path, title: &str) -> Option<&'static str> {
    let stem = sanitize_filename(title);
    for ext in &[
        "mp4", "m4a", "webm", "mp3", "ogg", "opus", "wav", "flac", "mkv",
    ] {
        if dir.join(format!("{stem}.{ext}")).exists() {
            return Some(ext);
        }
    }
    None
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

pub fn list_tracks_from_events(
    set_list: &[String],
    deleted_indices: &HashSet<usize>,
) -> Vec<TrackInfo> {
    set_list
        .iter()
        .enumerate()
        .filter(|(i, _)| !deleted_indices.contains(i))
        .map(|(index, title)| TrackInfo {
            index,
            title: title.clone(),
            available: false,
            is_video: false,
            liked: false,
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
                is_video: ext.map_or(false, is_video_extension),
                liked: false,
            }
        })
        .collect()
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ErrorEntry {
    pub error: String,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConcertStatus {
    Ignored,
    Wanted,
    Available,
}

impl ConcertStatus {
    pub fn from_flags(ignored: bool, wanted: bool) -> Self {
        if ignored {
            ConcertStatus::Ignored
        } else if wanted {
            ConcertStatus::Wanted
        } else {
            ConcertStatus::Available
        }
    }

    pub fn slug(&self) -> &str {
        match self {
            ConcertStatus::Ignored => "ignored",
            ConcertStatus::Wanted => "wanted",
            ConcertStatus::Available => "available",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DownloadStatus {
    NotDownloaded,
    Downloading,
    Downloaded,
    DownloadError,
}

impl DownloadStatus {
    /// `download_started_at` outranks `downloaded_at`: a re-download (source
    /// file deleted out-of-band) runs with `downloaded_at` still set, and must
    /// surface as Downloading so the card shows it and keeps polling.
    pub fn from_concert(c: &Concert) -> Self {
        if c.download_started_at.is_some() {
            DownloadStatus::Downloading
        } else if c.downloaded_at.is_some() {
            DownloadStatus::Downloaded
        } else if !c.download_errors.is_empty() {
            DownloadStatus::DownloadError
        } else {
            DownloadStatus::NotDownloaded
        }
    }

    pub fn slug(&self) -> &str {
        match self {
            DownloadStatus::NotDownloaded => "not-downloaded",
            DownloadStatus::Downloading => "downloading",
            DownloadStatus::Downloaded => "downloaded",
            DownloadStatus::DownloadError => "download-error",
        }
    }

    pub fn label(&self) -> &str {
        self.slug()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SplitStatus {
    NotSplit,
    Splitting,
    Split,
    SplitError,
}

impl SplitStatus {
    /// `split_started_at` outranks `split_at`: a re-split (deleted track
    /// played) runs with `split_at` still set, and must surface as Splitting
    /// so the card disables the track buttons and keeps polling. If the
    /// re-split fails, `split_started_at` is cleared and the status falls
    /// back to Split — the surviving track files are still usable.
    pub fn from_concert(c: &Concert) -> Self {
        if c.split_started_at.is_some() {
            SplitStatus::Splitting
        } else if c.split_at.is_some() {
            SplitStatus::Split
        } else if !c.split_errors.is_empty() {
            SplitStatus::SplitError
        } else {
            SplitStatus::NotSplit
        }
    }

    pub fn slug(&self) -> &str {
        match self {
            SplitStatus::NotSplit => "not-split",
            SplitStatus::Splitting => "splitting",
            SplitStatus::Split => "split",
            SplitStatus::SplitError => "split-error",
        }
    }

    /// Diverges from `slug()` only where the slug reads awkwardly. `slug()`
    /// stays stable for CSS classes and URL filter values.
    pub fn label(&self) -> &str {
        match self {
            SplitStatus::Split => "tracks",
            _ => self.slug(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ArchiveStatus {
    NotArchived,
    Archiving,
    Archived,
    ArchiveError,
}

impl ArchiveStatus {
    pub fn from_concert(c: &Concert) -> Self {
        if c.archived_at.is_some() {
            ArchiveStatus::Archived
        } else if c.archive_started_at.is_some() {
            ArchiveStatus::Archiving
        } else if !c.archive_errors.is_empty() {
            ArchiveStatus::ArchiveError
        } else {
            ArchiveStatus::NotArchived
        }
    }

    pub fn slug(&self) -> &str {
        match self {
            ArchiveStatus::NotArchived => "not-archived",
            ArchiveStatus::Archiving => "archiving",
            ArchiveStatus::Archived => "archived",
            ArchiveStatus::ArchiveError => "archive-error",
        }
    }

    pub fn label(&self) -> &str {
        self.slug()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Musician {
    pub name: String,
    pub instruments: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Concert {
    pub id: i64,
    pub source_url: String,
    pub title: String,
    pub concert_date: Option<String>,
    pub teaser: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub description: Option<String>,
    pub set_list: Vec<String>,
    pub musicians: Vec<Musician>,
    pub ignored: bool,
    pub wanted: bool,
    pub notes: Option<String>,
    pub download_started_at: Option<String>,
    pub downloaded_at: Option<String>,
    pub downloaded_extension: Option<String>,
    pub download_errors: Vec<ErrorEntry>,
    pub split_started_at: Option<String>,
    pub split_at: Option<String>,
    pub split_errors: Vec<ErrorEntry>,
    pub archive_started_at: Option<String>,
    pub archived_at: Option<String>,
    pub archive_errors: Vec<ErrorEntry>,
    pub inserted_at: String,
    pub updated_at: Option<String>,
    pub metadata_scraped_at: Option<String>,
    pub tracks_present: Vec<bool>,
    pub tracks_liked: Vec<bool>,
}

impl Concert {
    pub fn concert_status(&self) -> ConcertStatus {
        ConcertStatus::from_flags(self.ignored, self.wanted)
    }

    pub fn download_status(&self) -> DownloadStatus {
        DownloadStatus::from_concert(self)
    }

    pub fn split_status(&self) -> SplitStatus {
        SplitStatus::from_concert(self)
    }

    pub fn archive_status(&self) -> ArchiveStatus {
        ArchiveStatus::from_concert(self)
    }

    pub fn track_count(&self) -> usize {
        self.tracks_present.iter().filter(|&&p| p).count()
    }

    pub fn track_total(&self) -> usize {
        if self.tracks_present.is_empty() {
            self.set_list.len()
        } else {
            self.tracks_present.len()
        }
    }

    /// Date portion of `concert_date` for display. Archive sync stores
    /// date-only strings like "2026-05-20"; full per-concert scrape stores
    /// ISO 8601 timestamps like "2026-05-22T05:00:00-04:00". Either way,
    /// we only want the YYYY-MM-DD prefix in the UI.
    pub fn display_date(&self) -> Option<String> {
        self.concert_date
            .as_ref()
            .map(|d| d.get(..10).unwrap_or(d).to_string())
    }

    /// Split the stored description into its source paragraphs. The scraper
    /// joins NPR's `<p>` blocks with `"\n\n"`; this reverses that so the
    /// renderer can emit one `<p>` per paragraph. Empty pieces are dropped.
    pub fn description_paragraphs(&self) -> Vec<&str> {
        match self.description.as_deref() {
            Some(s) => s
                .split("\n\n")
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .collect(),
            None => Vec::new(),
        }
    }

    /// Browser-visible URL for this concert's preview image, or `None` if the
    /// image hasn't been downloaded yet. Served via the `/concert-files`
    /// ServeDir mount (the `/concerts/:id` API route prevents reusing the
    /// `/concerts` URL prefix for static files).
    pub fn preview_image_url(&self, working_dir: &std::path::Path) -> Option<String> {
        let album = self.album.as_deref()?;
        let sanitized = sanitize_album(album);
        let on_disk = concert_dir(working_dir, album).join("preview.jpg");
        if !on_disk.exists() {
            return None;
        }
        Some(format!("/concert-files/{}/preview.jpg", sanitized))
    }

    /// DB-only version: returns the URL when metadata has been scraped (which
    /// downloads the preview image). No filesystem check — the browser handles
    /// a 404 if the file is missing.
    pub fn preview_image_url_from_db(&self) -> Option<String> {
        self.metadata_scraped_at.as_ref()?;
        let album = self.album.as_deref()?;
        let sanitized = sanitize_album(album);
        tracing::debug!(album, "preview_image_url_from_db");
        Some(format!("/concert-files/{}/preview.jpg", sanitized))
    }

    /// Browser URL for this concert's listing thumbnail, served from the
    /// always-local `thumbnails/` dir via the `/thumbnails` ServeDir mount.
    /// Like [`Self::preview_image_url_from_db`], returns a URL once metadata has
    /// been scraped (which generates the thumbnail); the browser handles a 404
    /// if the file is missing.
    pub fn thumbnail_url_from_db(&self) -> Option<String> {
        self.metadata_scraped_at.as_ref()?;
        let album = self.album.as_deref()?;
        let sanitized = sanitize_album(album);
        Some(format!("/thumbnails/{}.jpg", sanitized))
    }
}

pub fn list_all_tracks_from_db(
    set_list: &[String],
    tracks_present: &[bool],
    tracks_liked: &[bool],
) -> Vec<TrackInfo> {
    tracing::debug!(
        set_list_len = set_list.len(),
        tracks_present_len = tracks_present.len(),
        tracks_liked_len = tracks_liked.len(),
        "list_all_tracks_from_db"
    );
    set_list
        .iter()
        .enumerate()
        .map(|(index, title)| {
            let available = tracks_present.get(index).copied().unwrap_or(false);
            let liked = tracks_liked.get(index).copied().unwrap_or(false);
            TrackInfo {
                index,
                title: title.clone(),
                available,
                is_video: false,
                liked,
            }
        })
        .collect()
}

// ── Playlists ────────────────────────────────────────────────────────────────

/// A user-curated, ordered collection. Its items (see `PlaylistItem`) are
/// expanded to concrete tracks at read time by `crate::playlist::expand_playlist`.
#[derive(Debug, Clone)]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub inserted_at: String,
    pub updated_at: Option<String>,
}

/// What a single playlist item references. A row in `playlist_items` stores this
/// as an `item_type` discriminator plus nullable `concert_id`/`track_index`/
/// `child_playlist_id` columns; this enum is the validated, in-memory form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistItemKind {
    /// A single track: `concert.set_list[track_index]`.
    Track { concert_id: i64, track_index: usize },
    /// A whole concert — expands to all of its tracks.
    Concert { concert_id: i64 },
    /// Another playlist nested inside this one — expands recursively.
    Playlist { child_playlist_id: i64 },
}

impl PlaylistItemKind {
    /// The `item_type` string stored in the DB.
    pub fn type_str(&self) -> &'static str {
        match self {
            PlaylistItemKind::Track { .. } => "track",
            PlaylistItemKind::Concert { .. } => "concert",
            PlaylistItemKind::Playlist { .. } => "playlist",
        }
    }
}

/// One ordered entry in a playlist.
#[derive(Debug, Clone)]
pub struct PlaylistItem {
    pub id: i64,
    pub playlist_id: i64,
    pub position: i64,
    pub kind: PlaylistItemKind,
}

/// A track produced by flattening a playlist's items (the "live reference"
/// result). `duration` is `None` when the source concert has no split timestamps
/// for this index; `available` mirrors the concert's `tracks_present[index]`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTrack {
    pub concert_id: i64,
    pub track_index: usize,
    pub title: String,
    pub duration: Option<f64>,
    pub available: bool,
}

/// Aggregate view of a playlist for the list page: how many tracks it resolves
/// to, the summed duration of the tracks whose duration is known, how many have
/// unknown duration, and the first track that would play.
#[derive(Debug, Clone)]
pub struct PlaylistSummary {
    pub track_count: usize,
    pub known_duration_secs: f64,
    pub unknown_count: usize,
    pub first_track: Option<ResolvedTrack>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare_concert() -> Concert {
        Concert {
            id: 1,
            source_url: "https://npr.org/c/1".to_string(),
            title: "Test".to_string(),
            concert_date: None,
            teaser: None,
            artist: None,
            album: None,
            description: None,
            set_list: vec![],
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
            tracks_present: vec![],
            tracks_liked: vec![],
        }
    }

    #[test]
    fn sanitize_album_strips_colons() {
        assert_eq!(sanitize_album("Bob Dylan: Live"), "Bob Dylan Live");
        assert_eq!(sanitize_album("No Colons"), "No Colons");
        assert_eq!(sanitize_album("A: B: C"), "A B C");
        assert_eq!(sanitize_album(""), "");
    }

    #[test]
    fn sanitize_filename_replaces_special_chars() {
        assert_eq!(sanitize_filename("Hello/World"), "Hello_World");
        assert_eq!(sanitize_filename("A:B:C"), "A_B_C");
        assert_eq!(sanitize_filename("normal"), "normal");
        assert_eq!(sanitize_filename(""), "untitled");
        assert_eq!(sanitize_filename("..."), "untitled");
        assert_eq!(sanitize_filename("a*b?c"), "a_b_c");
    }

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
    fn concert_status_from_flags_all_combinations() {
        assert_eq!(
            ConcertStatus::from_flags(false, false),
            ConcertStatus::Available
        );
        assert_eq!(
            ConcertStatus::from_flags(true, false),
            ConcertStatus::Ignored
        );
        assert_eq!(
            ConcertStatus::from_flags(false, true),
            ConcertStatus::Wanted
        );
        // ignored takes priority if somehow both are set
        assert_eq!(
            ConcertStatus::from_flags(true, true),
            ConcertStatus::Ignored
        );
    }

    #[test]
    fn concert_status_slugs() {
        assert_eq!(ConcertStatus::Available.slug(), "available");
        assert_eq!(ConcertStatus::Ignored.slug(), "ignored");
        assert_eq!(ConcertStatus::Wanted.slug(), "wanted");
    }

    fn err(msg: &str) -> ErrorEntry {
        ErrorEntry {
            error: msg.to_string(),
            at: "2024-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn download_status_not_downloaded_when_no_timestamps() {
        let c = bare_concert();
        assert_eq!(c.download_status(), DownloadStatus::NotDownloaded);
        assert_eq!(c.split_status(), SplitStatus::NotSplit);
    }

    #[test]
    fn download_status_downloading_when_started_but_not_done() {
        let mut c = bare_concert();
        c.download_started_at = Some("2024-01-01T00:00:00Z".to_string());
        assert_eq!(c.download_status(), DownloadStatus::Downloading);
    }

    #[test]
    fn download_status_download_error_when_errors_only() {
        let mut c = bare_concert();
        c.download_errors = vec![err("failed")];
        assert_eq!(c.download_status(), DownloadStatus::DownloadError);
    }

    #[test]
    fn download_status_downloaded_after_success() {
        let mut c = bare_concert();
        c.downloaded_at = Some("2024-01-01T01:00:00Z".to_string());
        assert_eq!(c.download_status(), DownloadStatus::Downloaded);
    }

    #[test]
    fn download_status_in_progress_beats_downloaded() {
        // A re-download (source file deleted out-of-band) runs with
        // downloaded_at still set: it must report Downloading so the card
        // shows the job and keeps polling.
        let mut c = bare_concert();
        c.downloaded_at = Some("2024-01-01T01:00:00Z".to_string());
        c.download_started_at = Some("2024-01-01T00:00:00Z".to_string());
        c.download_errors = vec![err("earlier")];
        assert_eq!(c.download_status(), DownloadStatus::Downloading);
    }

    #[test]
    fn split_status_in_progress_beats_split() {
        // A re-split (deleted track played) runs with split_at still set: it
        // must report Splitting so the card disables tracks and keeps polling.
        let mut c = bare_concert();
        c.downloaded_at = Some("2024-01-01T01:00:00Z".to_string());
        c.split_at = Some("2024-01-01T02:00:00Z".to_string());
        c.split_started_at = Some("2024-01-01T03:00:00Z".to_string());
        assert_eq!(c.split_status(), SplitStatus::Splitting);
    }

    #[test]
    fn split_status_splitting_when_in_progress() {
        let mut c = bare_concert();
        c.downloaded_at = Some("2024-01-01T01:00:00Z".to_string());
        c.split_started_at = Some("2024-01-01T02:00:00Z".to_string());
        assert_eq!(c.split_status(), SplitStatus::Splitting);
    }

    #[test]
    fn split_status_split_error_when_errors_only() {
        let mut c = bare_concert();
        c.downloaded_at = Some("2024-01-01T01:00:00Z".to_string());
        c.split_errors = vec![err("split failed")];
        assert_eq!(c.split_status(), SplitStatus::SplitError);
    }

    #[test]
    fn split_status_split_beats_in_progress_and_errors() {
        let mut c = bare_concert();
        c.downloaded_at = Some("2024-01-01T01:00:00Z".to_string());
        c.split_at = Some("2024-01-01T03:00:00Z".to_string());
        c.split_errors = vec![err("old error")];
        assert_eq!(c.split_status(), SplitStatus::Split);
    }

    #[test]
    fn split_status_independent_of_download_status() {
        // A purely synthetic state (no downloaded_at) — split_status still derives
        // from its own columns. The DB layer enforces the cross-machine rule
        // that splits require a download; the model does not.
        let mut c = bare_concert();
        c.split_at = Some("2024-01-01T03:00:00Z".to_string());
        assert_eq!(c.split_status(), SplitStatus::Split);
        assert_eq!(c.download_status(), DownloadStatus::NotDownloaded);
    }

    #[test]
    fn display_date_strips_time_from_iso_timestamp() {
        let mut c = bare_concert();
        c.concert_date = Some("2026-05-22T05:00:00-04:00".to_string());
        assert_eq!(c.display_date(), Some("2026-05-22".to_string()));
    }

    #[test]
    fn display_date_passes_through_date_only_string() {
        let mut c = bare_concert();
        c.concert_date = Some("2026-05-20".to_string());
        assert_eq!(c.display_date(), Some("2026-05-20".to_string()));
    }

    #[test]
    fn display_date_returns_none_when_missing() {
        let c = bare_concert();
        assert_eq!(c.display_date(), None);
    }

    #[test]
    fn display_date_returns_whole_string_when_shorter_than_ten() {
        let mut c = bare_concert();
        c.concert_date = Some("2026".to_string());
        assert_eq!(c.display_date(), Some("2026".to_string()));
    }

    #[test]
    fn download_status_slugs() {
        assert_eq!(DownloadStatus::NotDownloaded.slug(), "not-downloaded");
        assert_eq!(DownloadStatus::Downloading.slug(), "downloading");
        assert_eq!(DownloadStatus::Downloaded.slug(), "downloaded");
        assert_eq!(DownloadStatus::DownloadError.slug(), "download-error");
    }

    #[test]
    fn split_status_slugs() {
        assert_eq!(SplitStatus::NotSplit.slug(), "not-split");
        assert_eq!(SplitStatus::Splitting.slug(), "splitting");
        assert_eq!(SplitStatus::Split.slug(), "split");
        assert_eq!(SplitStatus::SplitError.slug(), "split-error");
    }

    #[test]
    fn split_status_label_says_tracks_only_for_split() {
        assert_eq!(SplitStatus::Split.label(), "tracks");
        for ss in [
            SplitStatus::NotSplit,
            SplitStatus::Splitting,
            SplitStatus::SplitError,
        ] {
            assert_eq!(
                ss.label(),
                ss.slug(),
                "label should match slug for {:?}",
                ss
            );
        }
    }

    #[test]
    fn download_status_label_always_matches_slug() {
        for ds in [
            DownloadStatus::NotDownloaded,
            DownloadStatus::Downloading,
            DownloadStatus::Downloaded,
            DownloadStatus::DownloadError,
        ] {
            assert_eq!(
                ds.label(),
                ds.slug(),
                "label should match slug for {:?}",
                ds
            );
        }
    }

    #[test]
    fn description_paragraphs_none_yields_empty() {
        let c = bare_concert();
        assert!(c.description_paragraphs().is_empty());
    }

    #[test]
    fn description_paragraphs_single_paragraph() {
        let mut c = bare_concert();
        c.description = Some("Just one paragraph.".to_string());
        assert_eq!(c.description_paragraphs(), vec!["Just one paragraph."]);
    }

    #[test]
    fn description_paragraphs_splits_on_double_newline() {
        let mut c = bare_concert();
        c.description = Some("First paragraph.\n\nSecond paragraph.\n\nThird.".to_string());
        assert_eq!(
            c.description_paragraphs(),
            vec!["First paragraph.", "Second paragraph.", "Third."]
        );
    }

    #[test]
    fn description_paragraphs_trims_and_drops_empties() {
        let mut c = bare_concert();
        c.description = Some("  First.  \n\n\n\n  Second.  \n\n".to_string());
        assert_eq!(c.description_paragraphs(), vec!["First.", "Second."]);
    }

    #[test]
    fn preview_image_url_returns_none_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = bare_concert();
        c.album = Some("Foo Album".to_string());
        assert!(c.preview_image_url(dir.path()).is_none());
    }

    #[test]
    fn preview_image_url_returns_path_when_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let cd = concert_dir(dir.path(), "Foo Album");
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::File::create(cd.join("preview.jpg")).unwrap();
        let mut c = bare_concert();
        c.album = Some("Foo Album".to_string());
        assert_eq!(
            c.preview_image_url(dir.path()).as_deref(),
            Some("/concert-files/Foo Album/preview.jpg")
        );
    }

    #[test]
    fn preview_image_url_strips_colons_from_album() {
        let dir = tempfile::tempdir().unwrap();
        let cd = concert_dir(dir.path(), "Some: Concert");
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::File::create(cd.join("preview.jpg")).unwrap();
        let mut c = bare_concert();
        c.album = Some("Some: Concert".to_string());
        assert_eq!(
            c.preview_image_url(dir.path()).as_deref(),
            Some("/concert-files/Some Concert/preview.jpg")
        );
    }

    #[test]
    fn concert_dir_strips_colons() {
        assert_eq!(
            concert_dir(Path::new("/wd"), "Air: Tiny Desk Concert"),
            PathBuf::from("/wd/concerts/Air Tiny Desk Concert")
        );
        assert_eq!(
            concert_dir(Path::new("/wd"), "Plain"),
            PathBuf::from("/wd/concerts/Plain")
        );
    }

    #[test]
    fn preview_image_url_returns_none_when_album_missing() {
        let dir = tempfile::tempdir().unwrap();
        let c = bare_concert();
        assert!(c.preview_image_url(dir.path()).is_none());
    }

    #[test]
    fn preview_image_url_from_db_returns_url_when_scraped() {
        let mut c = bare_concert();
        c.album = Some("Foo Album".to_string());
        c.metadata_scraped_at = Some("2024-01-01T00:00:00Z".to_string());
        assert_eq!(
            c.preview_image_url_from_db().as_deref(),
            Some("/concert-files/Foo Album/preview.jpg")
        );
    }

    #[test]
    fn preview_image_url_from_db_returns_none_when_not_scraped() {
        let mut c = bare_concert();
        c.album = Some("Foo Album".to_string());
        assert!(c.preview_image_url_from_db().is_none());
    }

    #[test]
    fn preview_image_url_from_db_returns_none_when_album_missing() {
        let mut c = bare_concert();
        c.metadata_scraped_at = Some("2024-01-01T00:00:00Z".to_string());
        assert!(c.preview_image_url_from_db().is_none());
    }

    #[test]
    fn preview_image_url_from_db_strips_colons() {
        let mut c = bare_concert();
        c.album = Some("Some: Concert".to_string());
        c.metadata_scraped_at = Some("2024-01-01T00:00:00Z".to_string());
        assert_eq!(
            c.preview_image_url_from_db().as_deref(),
            Some("/concert-files/Some Concert/preview.jpg")
        );
    }

    #[test]
    fn thumbnail_url_from_db_returns_url_when_scraped() {
        let mut c = bare_concert();
        c.album = Some("Some: Concert".to_string());
        c.metadata_scraped_at = Some("2024-01-01T00:00:00Z".to_string());
        assert_eq!(
            c.thumbnail_url_from_db().as_deref(),
            Some("/thumbnails/Some Concert.jpg")
        );
    }

    #[test]
    fn thumbnail_url_from_db_returns_none_when_not_scraped_or_no_album() {
        let mut c = bare_concert();
        c.album = Some("Foo Album".to_string());
        assert!(c.thumbnail_url_from_db().is_none());

        let mut c = bare_concert();
        c.metadata_scraped_at = Some("2024-01-01T00:00:00Z".to_string());
        assert!(c.thumbnail_url_from_db().is_none());
    }

    #[test]
    fn list_all_tracks_from_db_maps_presence() {
        let set_list = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let tracks_present = vec![true, false, true];
        let tracks = list_all_tracks_from_db(&set_list, &tracks_present, &[]);
        assert_eq!(tracks.len(), 3);
        assert!(tracks[0].available);
        assert!(!tracks[1].available);
        assert!(tracks[2].available);
        assert!(!tracks[0].is_video);
        assert!(!tracks[0].liked);
    }

    #[test]
    fn list_all_tracks_from_db_empty_set_list() {
        let tracks = list_all_tracks_from_db(&[], &[], &[]);
        assert!(tracks.is_empty());
    }

    #[test]
    fn list_all_tracks_from_db_empty_tracks_present() {
        let set_list = vec!["A".to_string(), "B".to_string()];
        let tracks = list_all_tracks_from_db(&set_list, &[], &[]);
        assert_eq!(tracks.len(), 2);
        assert!(!tracks[0].available);
        assert!(!tracks[1].available);
    }

    #[test]
    fn list_all_tracks_from_db_short_tracks_present() {
        let set_list = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let tracks_present = vec![true];
        let tracks = list_all_tracks_from_db(&set_list, &tracks_present, &[]);
        assert_eq!(tracks.len(), 3);
        assert!(tracks[0].available);
        assert!(!tracks[1].available);
        assert!(!tracks[2].available);
    }

    #[test]
    fn list_all_tracks_from_db_maps_liked() {
        let set_list = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let tracks_liked = vec![false, true, false];
        let tracks = list_all_tracks_from_db(&set_list, &[], &tracks_liked);
        assert!(!tracks[0].liked);
        assert!(tracks[1].liked);
        assert!(!tracks[2].liked);
    }

    #[test]
    fn list_all_tracks_from_db_handles_short_tracks_liked() {
        let set_list = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let tracks_liked = vec![true];
        let tracks = list_all_tracks_from_db(&set_list, &[], &tracks_liked);
        assert!(tracks[0].liked);
        assert!(!tracks[1].liked);
        assert!(!tracks[2].liked);
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
    fn list_tracks_from_events_defaults_liked_false() {
        let set_list = vec!["A".to_string(), "B".to_string()];
        let tracks = list_tracks_from_events(&set_list, &HashSet::new());
        assert_eq!(tracks.len(), 2);
        assert!(!tracks[0].liked);
        assert!(!tracks[1].liked);
    }

    #[test]
    fn list_all_tracks_defaults_liked_false() {
        let dir = tempfile::tempdir().unwrap();
        let set_list = vec!["Song A".to_string()];
        let tracks = list_all_tracks(dir.path(), "No Album", &set_list);
        assert_eq!(tracks.len(), 1);
        assert!(!tracks[0].liked);
    }

    #[test]
    fn archive_status_not_archived_by_default() {
        let c = bare_concert();
        assert_eq!(c.archive_status(), ArchiveStatus::NotArchived);
    }

    #[test]
    fn archive_status_archiving_when_started() {
        let mut c = bare_concert();
        c.archive_started_at = Some("2024-01-01T00:00:00Z".to_string());
        assert_eq!(c.archive_status(), ArchiveStatus::Archiving);
    }

    #[test]
    fn archive_status_archived_beats_started_and_errors() {
        let mut c = bare_concert();
        c.archived_at = Some("2024-01-01T01:00:00Z".to_string());
        c.archive_started_at = Some("2024-01-01T00:00:00Z".to_string());
        c.archive_errors = vec![err("old")];
        assert_eq!(c.archive_status(), ArchiveStatus::Archived);
    }

    #[test]
    fn archive_status_error_when_only_errors() {
        let mut c = bare_concert();
        c.archive_errors = vec![err("disk full")];
        assert_eq!(c.archive_status(), ArchiveStatus::ArchiveError);
    }

    #[test]
    fn archive_status_slugs() {
        assert_eq!(ArchiveStatus::NotArchived.slug(), "not-archived");
        assert_eq!(ArchiveStatus::Archiving.slug(), "archiving");
        assert_eq!(ArchiveStatus::Archived.slug(), "archived");
        assert_eq!(ArchiveStatus::ArchiveError.slug(), "archive-error");
    }

    #[test]
    fn list_tracks_from_events_excludes_deleted() {
        let set_list = vec![
            "Song A".to_string(),
            "Song B".to_string(),
            "Song C".to_string(),
        ];
        let deleted = HashSet::from([1]);
        let tracks = list_tracks_from_events(&set_list, &deleted);
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].index, 0);
        assert_eq!(tracks[0].title, "Song A");
        assert!(!tracks[0].available);
        assert_eq!(tracks[1].index, 2);
        assert_eq!(tracks[1].title, "Song C");
    }

    #[test]
    fn list_tracks_from_events_with_no_deletions() {
        let set_list = vec!["Song A".to_string(), "Song B".to_string()];
        let deleted = HashSet::new();
        let tracks = list_tracks_from_events(&set_list, &deleted);
        assert_eq!(tracks.len(), 2);
    }

    #[test]
    fn track_count_counts_true_values() {
        let mut c = bare_concert();
        c.tracks_present = vec![true, false, true, true, false];
        assert_eq!(c.track_count(), 3);
        assert_eq!(c.track_total(), 5);
    }

    #[test]
    fn track_count_empty_falls_back_to_set_list_len() {
        let mut c = bare_concert();
        c.set_list = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        assert_eq!(c.track_count(), 0);
        assert_eq!(c.track_total(), 3);
    }

    #[test]
    fn track_count_all_present() {
        let mut c = bare_concert();
        c.tracks_present = vec![true, true, true];
        assert_eq!(c.track_count(), 3);
        assert_eq!(c.track_total(), 3);
    }

    #[test]
    fn is_video_extension_recognizes_video_formats() {
        assert!(is_video_extension("mp4"));
        assert!(is_video_extension("webm"));
        assert!(is_video_extension("MP4"));
        assert!(!is_video_extension("m4a"));
        assert!(!is_video_extension("mp3"));
        assert!(!is_video_extension("mkv"));
    }

    #[test]
    fn is_browser_playable_recognizes_supported_formats() {
        for ext in &["mp4", "m4a", "webm", "mp3", "ogg", "opus", "wav", "flac"] {
            assert!(is_browser_playable(ext), "{ext} should be playable");
        }
        assert!(!is_browser_playable("mkv"));
        assert!(!is_browser_playable("avi"));
    }

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
}
