use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone)]
pub struct TrackInfo {
    pub index: usize,
    pub title: String,
}

pub fn list_tracks(working_dir: &Path, album: &str, set_list: &[String]) -> Vec<TrackInfo> {
    let dir = concert_dir(working_dir, album);
    set_list
        .iter()
        .enumerate()
        .filter(|(_, title)| {
            let stem = sanitize_filename(title);
            dir.join(format!("{stem}.mp4")).exists() || dir.join(format!("{stem}.m4a")).exists()
        })
        .map(|(index, title)| TrackInfo {
            index,
            title: title.clone(),
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
    pub fn from_concert(c: &Concert) -> Self {
        if c.downloaded_at.is_some() {
            DownloadStatus::Downloaded
        } else if c.download_started_at.is_some() {
            DownloadStatus::Downloading
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
    pub fn from_concert(c: &Concert) -> Self {
        if c.split_at.is_some() {
            SplitStatus::Split
        } else if c.split_started_at.is_some() {
            SplitStatus::Splitting
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
    pub download_errors: Vec<ErrorEntry>,
    pub split_started_at: Option<String>,
    pub split_at: Option<String>,
    pub split_errors: Vec<ErrorEntry>,
    pub first_seen_at: String,
    pub metadata_scraped_at: Option<String>,
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
    /// image hasn't been downloaded yet. Mirrors the on-disk layout under
    /// `<working_dir>/concerts/<album>/<album>.jpg`. Served via the
    /// `/concert-files` ServeDir mount (the `/concerts/:id` API route prevents
    /// reusing the `/concerts` URL prefix for static files).
    pub fn preview_image_url(&self, working_dir: &std::path::Path) -> Option<String> {
        let album = self.album.as_deref()?;
        let sanitized = sanitize_album(album);
        let on_disk = concert_dir(working_dir, album).join(format!("{}.jpg", sanitized));
        if !on_disk.exists() {
            return None;
        }
        Some(format!("/concert-files/{}/{}.jpg", sanitized, sanitized))
    }
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
            download_errors: vec![],
            split_started_at: None,
            split_at: None,
            split_errors: vec![],
            first_seen_at: "2024-01-01T00:00:00Z".to_string(),
            metadata_scraped_at: None,
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
        assert_eq!(tracks[1].index, 2);
        assert_eq!(tracks[1].title, "Song Three");
    }

    #[test]
    fn list_tracks_returns_empty_when_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let set_list = vec!["Song A".to_string()];
        let tracks = list_tracks(dir.path(), "No Album", &set_list);
        assert!(tracks.is_empty());
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
    fn download_status_downloaded_beats_in_progress_and_errors() {
        let mut c = bare_concert();
        c.downloaded_at = Some("2024-01-01T01:00:00Z".to_string());
        c.download_started_at = Some("2024-01-01T00:00:00Z".to_string());
        c.download_errors = vec![err("earlier")];
        assert_eq!(c.download_status(), DownloadStatus::Downloaded);
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
        std::fs::File::create(cd.join("Foo Album.jpg")).unwrap();
        let mut c = bare_concert();
        c.album = Some("Foo Album".to_string());
        assert_eq!(
            c.preview_image_url(dir.path()).as_deref(),
            Some("/concert-files/Foo Album/Foo Album.jpg")
        );
    }

    #[test]
    fn preview_image_url_strips_colons_from_album() {
        let dir = tempfile::tempdir().unwrap();
        let cd = concert_dir(dir.path(), "Some: Concert");
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::File::create(cd.join("Some Concert.jpg")).unwrap();
        let mut c = bare_concert();
        c.album = Some("Some: Concert".to_string());
        assert_eq!(
            c.preview_image_url(dir.path()).as_deref(),
            Some("/concert-files/Some Concert/Some Concert.jpg")
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
}
