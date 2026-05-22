use serde::{Deserialize, Serialize};

/// Strip colons from album names to produce safe filesystem paths.
/// Mirrors the logic in download.sh and extract.sh.
pub fn sanitize_album(album: &str) -> String {
    album.replace(':', "")
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
pub enum ProcessingStatus {
    Split,
    Splitting,
    SplitError,
    Downloaded,
    Downloading,
    DownloadError,
    NotStarted,
}

impl ProcessingStatus {
    pub fn from_concert(c: &Concert) -> Self {
        if c.split_at.is_some() {
            ProcessingStatus::Split
        } else if c.split_started_at.is_some() {
            ProcessingStatus::Splitting
        } else if !c.split_errors.is_empty() {
            ProcessingStatus::SplitError
        } else if c.downloaded_at.is_some() {
            ProcessingStatus::Downloaded
        } else if c.download_started_at.is_some() {
            ProcessingStatus::Downloading
        } else if !c.download_errors.is_empty() {
            ProcessingStatus::DownloadError
        } else {
            ProcessingStatus::NotStarted
        }
    }

    pub fn slug(&self) -> &str {
        match self {
            ProcessingStatus::Split => "split",
            ProcessingStatus::Splitting => "splitting",
            ProcessingStatus::SplitError => "split-error",
            ProcessingStatus::Downloaded => "downloaded",
            ProcessingStatus::Downloading => "downloading",
            ProcessingStatus::DownloadError => "download-error",
            ProcessingStatus::NotStarted => "not-started",
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

    pub fn processing_status(&self) -> ProcessingStatus {
        ProcessingStatus::from_concert(self)
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
    fn concert_status_from_flags_all_combinations() {
        assert_eq!(ConcertStatus::from_flags(false, false), ConcertStatus::Available);
        assert_eq!(ConcertStatus::from_flags(true, false), ConcertStatus::Ignored);
        assert_eq!(ConcertStatus::from_flags(false, true), ConcertStatus::Wanted);
        // ignored takes priority if somehow both are set
        assert_eq!(ConcertStatus::from_flags(true, true), ConcertStatus::Ignored);
    }

    #[test]
    fn concert_status_slugs() {
        assert_eq!(ConcertStatus::Available.slug(), "available");
        assert_eq!(ConcertStatus::Ignored.slug(), "ignored");
        assert_eq!(ConcertStatus::Wanted.slug(), "wanted");
    }

    #[test]
    fn processing_status_not_started_when_no_timestamps() {
        let c = bare_concert();
        assert_eq!(c.processing_status(), ProcessingStatus::NotStarted);
    }

    #[test]
    fn processing_status_downloading_when_started_but_not_done() {
        let mut c = bare_concert();
        c.download_started_at = Some("2024-01-01T00:00:00Z".to_string());
        assert_eq!(c.processing_status(), ProcessingStatus::Downloading);
    }

    #[test]
    fn processing_status_download_error_when_errors_and_no_started_at() {
        let mut c = bare_concert();
        c.download_errors = vec![ErrorEntry {
            error: "failed".to_string(),
            at: "2024-01-01T00:00:00Z".to_string(),
        }];
        assert_eq!(c.processing_status(), ProcessingStatus::DownloadError);
    }

    #[test]
    fn processing_status_downloaded_after_success() {
        let mut c = bare_concert();
        c.downloaded_at = Some("2024-01-01T01:00:00Z".to_string());
        assert_eq!(c.processing_status(), ProcessingStatus::Downloaded);
    }

    #[test]
    fn processing_status_splitting_when_split_started() {
        let mut c = bare_concert();
        c.downloaded_at = Some("2024-01-01T01:00:00Z".to_string());
        c.split_started_at = Some("2024-01-01T02:00:00Z".to_string());
        assert_eq!(c.processing_status(), ProcessingStatus::Splitting);
    }

    #[test]
    fn processing_status_split_error() {
        let mut c = bare_concert();
        c.downloaded_at = Some("2024-01-01T01:00:00Z".to_string());
        c.split_errors = vec![ErrorEntry {
            error: "split failed".to_string(),
            at: "2024-01-01T02:00:00Z".to_string(),
        }];
        assert_eq!(c.processing_status(), ProcessingStatus::SplitError);
    }

    #[test]
    fn processing_status_split_takes_priority_over_all() {
        let mut c = bare_concert();
        c.downloaded_at = Some("2024-01-01T01:00:00Z".to_string());
        c.split_at = Some("2024-01-01T03:00:00Z".to_string());
        c.split_errors = vec![ErrorEntry {
            error: "old error".to_string(),
            at: "2024-01-01T02:00:00Z".to_string(),
        }];
        assert_eq!(c.processing_status(), ProcessingStatus::Split);
    }

    #[test]
    fn processing_status_slugs() {
        assert_eq!(ProcessingStatus::NotStarted.slug(), "not-started");
        assert_eq!(ProcessingStatus::Downloading.slug(), "downloading");
        assert_eq!(ProcessingStatus::DownloadError.slug(), "download-error");
        assert_eq!(ProcessingStatus::Downloaded.slug(), "downloaded");
        assert_eq!(ProcessingStatus::Splitting.slug(), "splitting");
        assert_eq!(ProcessingStatus::SplitError.slug(), "split-error");
        assert_eq!(ProcessingStatus::Split.slug(), "split");
    }
}
