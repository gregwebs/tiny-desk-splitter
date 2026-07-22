use std::path::Path;

use crate::concert_media::{
    find_downloaded_file, is_video_extension, list_all_track_details, ConcertMediaInventory,
};
use crate::model::{self, Concert, PlaybackItem, TrackDetailItem};

#[derive(Debug, Clone)]
pub enum PlaybackPlan {
    Source(SourceMedia),
    Reconstruction(Vec<PlaybackItem>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMedia {
    pub filename: String,
    pub title: String,
    pub artist: String,
    pub is_video: bool,
    pub playable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackMedia {
    pub filename: String,
    pub title: String,
    pub artist: String,
    pub is_video: bool,
    pub playable: bool,
    pub track_index: usize,
    pub has_next: bool,
    pub has_prev: bool,
    pub liked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackLookupError {
    NotPlayable,
    MarkedDownloadedButMissing { concert_id: i64 },
    InvalidFilename,
}

pub fn source_media(
    working_dir: &Path,
    concert: &Concert,
) -> Result<SourceMedia, PlaybackLookupError> {
    let album = concert
        .album
        .as_deref()
        .ok_or(PlaybackLookupError::NotPlayable)?;
    let Some(path) = find_downloaded_file(working_dir, album) else {
        if concert.downloaded_at.is_some() {
            tracing::warn!(
                concert_id = concert.id,
                album,
                "playback source missing despite downloaded state"
            );
            return Err(PlaybackLookupError::MarkedDownloadedButMissing {
                concert_id: concert.id,
            });
        }
        tracing::debug!(concert_id = concert.id, album, "playback source not found");
        return Err(PlaybackLookupError::NotPlayable);
    };
    tracing::debug!(
        concert_id = concert.id,
        path = %path.display(),
        "playback source found"
    );
    source_media_from_path(concert, album, &path)
}

pub fn concert_playback_plan(
    working_dir: &Path,
    concert: &Concert,
    user_timestamps: Option<&[concert_types::SongTimestamp]>,
) -> Result<PlaybackPlan, PlaybackLookupError> {
    let album = concert
        .album
        .as_deref()
        .ok_or(PlaybackLookupError::NotPlayable)?;
    if let Some(path) = find_downloaded_file(working_dir, album) {
        return source_media_from_path(concert, album, &path).map(PlaybackPlan::Source);
    }

    let items = ConcertMediaInventory::for_concert(working_dir, concert, user_timestamps)
        .reconstruction_items();
    tracing::debug!(
        concert_id = concert.id,
        item_count = items.len(),
        "playback reconstruction fallback"
    );
    if items.is_empty() {
        Err(PlaybackLookupError::NotPlayable)
    } else {
        Ok(PlaybackPlan::Reconstruction(items))
    }
}

pub fn reconstruction_items(
    working_dir: &Path,
    concert: &Concert,
    user_timestamps: Option<&[concert_types::SongTimestamp]>,
) -> Result<Vec<PlaybackItem>, PlaybackLookupError> {
    if concert.album.is_none() {
        return Err(PlaybackLookupError::NotPlayable);
    }
    let items = ConcertMediaInventory::for_concert(working_dir, concert, user_timestamps)
        .reconstruction_items();
    tracing::debug!(
        concert_id = concert.id,
        item_count = items.len(),
        "playback reconstruction items"
    );
    Ok(items)
}

pub fn track_media(
    working_dir: &Path,
    concert: &Concert,
    track_index: usize,
) -> Result<TrackMedia, PlaybackLookupError> {
    track_media_inner(working_dir, concert, track_index, false)
}

pub fn next_track_media(
    working_dir: &Path,
    concert: &Concert,
    after_index: usize,
) -> Result<TrackMedia, PlaybackLookupError> {
    let Some((track_index, _filename)) = find_playable_track(
        working_dir,
        concert,
        after_index.saturating_add(1)..concert.set_list.len(),
    ) else {
        tracing::debug!(
            concert_id = concert.id,
            after_index,
            "next playable track not found"
        );
        return Err(PlaybackLookupError::NotPlayable);
    };
    track_media_inner(working_dir, concert, track_index, true)
}

pub fn prev_track_media(
    working_dir: &Path,
    concert: &Concert,
    before_index: usize,
) -> Result<TrackMedia, PlaybackLookupError> {
    let upper = before_index.min(concert.set_list.len());
    let Some((track_index, _filename)) =
        find_playable_track(working_dir, concert, (0..upper).rev())
    else {
        tracing::debug!(
            concert_id = concert.id,
            before_index,
            "previous playable track not found"
        );
        return Err(PlaybackLookupError::NotPlayable);
    };
    track_media_inner(working_dir, concert, track_index, true)
}

pub fn track_details(
    working_dir: &Path,
    concert: &Concert,
) -> Result<Vec<TrackDetailItem>, PlaybackLookupError> {
    let Some(_album) = concert.album.as_deref() else {
        return Ok(list_all_track_details(
            working_dir,
            "",
            &concert.set_list,
            &concert.tracks_present,
            &concert.tracks_liked,
        ));
    };
    Ok(ConcertMediaInventory::for_concert(working_dir, concert, None).track_details())
}

fn source_media_from_path(
    concert: &Concert,
    album: &str,
    path: &Path,
) -> Result<SourceMedia, PlaybackLookupError> {
    let filename = path
        .file_name()
        .and_then(|f| f.to_str())
        .ok_or(PlaybackLookupError::InvalidFilename)?
        .to_string();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    Ok(SourceMedia {
        filename,
        title: album.to_string(),
        artist: concert.artist.clone().unwrap_or_default(),
        is_video: is_video_extension(ext),
        playable: model::is_browser_playable(ext),
    })
}

fn track_media_inner(
    working_dir: &Path,
    concert: &Concert,
    track_index: usize,
    require_browser_playable: bool,
) -> Result<TrackMedia, PlaybackLookupError> {
    if concert.album.is_none() {
        return Err(PlaybackLookupError::NotPlayable);
    }
    let title = concert
        .set_list
        .get(track_index)
        .ok_or(PlaybackLookupError::NotPlayable)?
        .clone();
    let filename = ConcertMediaInventory::for_concert(working_dir, concert, None)
        .find_track_file(&title)
        .ok_or(PlaybackLookupError::NotPlayable)?;
    let ext = filename.rsplit('.').next().unwrap_or("");
    let playable = model::is_browser_playable(ext);
    let is_video = is_video_extension(ext);
    if require_browser_playable && !playable {
        tracing::debug!(
            concert_id = concert.id,
            track_index,
            ext,
            "skipping non-browser-playable track"
        );
        return Err(PlaybackLookupError::NotPlayable);
    }
    Ok(TrackMedia {
        filename,
        title,
        artist: concert.artist.clone().unwrap_or_default(),
        is_video,
        playable,
        track_index,
        has_next: find_playable_track(
            working_dir,
            concert,
            track_index.saturating_add(1)..concert.set_list.len(),
        )
        .is_some(),
        has_prev: find_playable_track(working_dir, concert, (0..track_index).rev()).is_some(),
        liked: concert
            .tracks_liked
            .get(track_index)
            .copied()
            .unwrap_or(false),
    })
}

fn find_playable_track<I>(
    working_dir: &Path,
    concert: &Concert,
    indices: I,
) -> Option<(usize, String)>
where
    I: IntoIterator<Item = usize>,
{
    concert.album.as_deref()?;
    for index in indices {
        let Some(title) = concert.set_list.get(index) else {
            continue;
        };
        let Some(filename) =
            ConcertMediaInventory::for_concert(working_dir, concert, None).find_track_file(title)
        else {
            continue;
        };
        let ext = filename.rsplit('.').next().unwrap_or("");
        if !model::is_browser_playable(ext) {
            tracing::debug!(
                concert_id = concert.id,
                track_index = index,
                ext,
                "skipping non-browser-playable track"
            );
            continue;
        }
        return Some((index, filename));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{concert_dir, sanitize_filename, Concert, ErrorEntry};

    fn bare_concert(album: Option<&str>, set_list: &[&str]) -> Concert {
        Concert {
            id: 7,
            source_url: "https://npr.org/c/7".to_string(),
            title: "Test".to_string(),
            concert_date: None,
            teaser: None,
            artist: Some("Artist".to_string()),
            album: album.map(str::to_string),
            description: None,
            set_list: set_list.iter().map(|s| s.to_string()).collect(),
            musicians: vec![],
            ignored: false,
            wanted: false,
            notes: None,
            download_started_at: None,
            downloaded_at: None,
            downloaded_extension: None,
            download_errors: vec![],
            split_started_at: None,
            split_at: Some("2026-07-07 00:00:00".to_string()),
            split_errors: vec![],
            archive_started_at: None,
            archived_at: None,
            archive_errors: vec![],
            inserted_at: "2026-07-07 00:00:00".to_string(),
            updated_at: None,
            metadata_scraped_at: None,
            tracks_present: vec![],
            tracks_liked: vec![],
            media_duration: None,
        }
    }

    fn write_source(workdir: &Path, album: &str, ext: &str) {
        let dir = concert_dir(workdir, album);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{album}.{ext}")), b"media").unwrap();
    }

    fn write_track(workdir: &Path, album: &str, title: &str, ext: &str) {
        let dir = concert_dir(workdir, album);
        std::fs::create_dir_all(&dir).unwrap();
        let stem = sanitize_filename(title);
        std::fs::write(dir.join(format!("{stem}.{ext}")), b"media").unwrap();
    }

    fn err(msg: &str) -> ErrorEntry {
        ErrorEntry {
            error: msg.to_string(),
            at: "2026-07-07 00:00:00".to_string(),
        }
    }

    #[test]
    fn source_mode_when_source_file_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Source Album";
        write_source(tmp.path(), album, "mp4");
        let mut concert = bare_concert(Some(album), &[]);
        concert.downloaded_at = Some("2026-07-07 00:00:00".to_string());

        let media = source_media(tmp.path(), &concert).unwrap();

        assert_eq!(media.filename, "Source Album.mp4");
        assert_eq!(media.title, album);
        assert_eq!(media.artist, "Artist");
        assert!(media.is_video);
        assert!(media.playable);
    }

    #[test]
    fn reconstruction_mode_when_source_gone_and_items_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Recon Album";
        write_track(tmp.path(), album, "Song A", "m4a");
        let mut concert = bare_concert(Some(album), &["Song A"]);
        concert.tracks_present = vec![true];

        let plan = concert_playback_plan(tmp.path(), &concert, None).unwrap();

        let PlaybackPlan::Reconstruction(items) = plan else {
            panic!("expected reconstruction");
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Song A");
    }

    #[test]
    fn reconstruction_items_do_not_depend_on_source_presence() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Recon With Source Album";
        write_source(tmp.path(), album, "mp4");
        write_track(tmp.path(), album, "Song A", "m4a");
        let mut concert = bare_concert(Some(album), &["Song A"]);
        concert.tracks_present = vec![true];

        let items = reconstruction_items(tmp.path(), &concert, None).unwrap();

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Song A");
    }

    #[test]
    fn no_playable_reconstruction_returns_not_playable() {
        let tmp = tempfile::tempdir().unwrap();
        let mut concert = bare_concert(Some("Empty Album"), &["Song A"]);
        concert.tracks_present = vec![true];

        let err = concert_playback_plan(tmp.path(), &concert, None).unwrap_err();

        assert_eq!(err, PlaybackLookupError::NotPlayable);
    }

    #[test]
    fn downloaded_state_with_missing_source_is_integrity_error() {
        let tmp = tempfile::tempdir().unwrap();
        let mut concert = bare_concert(Some("Missing Source Album"), &[]);
        concert.downloaded_at = Some("2026-07-07 00:00:00".to_string());
        concert.download_errors = vec![err("old")];

        let err = source_media(tmp.path(), &concert).unwrap_err();

        assert_eq!(
            err,
            PlaybackLookupError::MarkedDownloadedButMissing { concert_id: 7 }
        );
    }

    #[test]
    fn missing_album_and_out_of_range_track_are_not_playable() {
        let tmp = tempfile::tempdir().unwrap();
        let concert = bare_concert(None, &["Song A"]);

        assert_eq!(
            source_media(tmp.path(), &concert).unwrap_err(),
            PlaybackLookupError::NotPlayable
        );
        assert_eq!(
            track_media(tmp.path(), &concert, 9).unwrap_err(),
            PlaybackLookupError::NotPlayable
        );
        assert_eq!(
            next_track_media(tmp.path(), &concert, usize::MAX).unwrap_err(),
            PlaybackLookupError::NotPlayable
        );
    }

    #[test]
    fn track_media_reports_like_and_neighbor_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Track Album";
        write_track(tmp.path(), album, "Song A", "mp3");
        write_track(tmp.path(), album, "Song B", "mp4");
        write_track(tmp.path(), album, "Song C", "mp3");
        let mut concert = bare_concert(Some(album), &["Song A", "Song B", "Song C"]);
        concert.tracks_present = vec![true, true, true];
        concert.tracks_liked = vec![false, true, false];

        let media = track_media(tmp.path(), &concert, 1).unwrap();

        assert_eq!(media.title, "Song B");
        assert_eq!(media.track_index, 1);
        assert!(media.is_video);
        assert!(media.playable);
        assert!(media.liked);
        assert!(media.has_next);
        assert!(media.has_prev);
    }

    #[test]
    fn next_and_prev_skip_unavailable_and_non_browser_playable_tracks() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Skip Album";
        write_track(tmp.path(), album, "Song A", "mp3");
        write_track(tmp.path(), album, "Song B", "mkv");
        write_track(tmp.path(), album, "Song C", "mp3");
        let concert = bare_concert(Some(album), &["Song A", "Song B", "Song C"]);

        let next = next_track_media(tmp.path(), &concert, 0).unwrap();
        let prev = prev_track_media(tmp.path(), &concert, 2).unwrap();

        assert_eq!(next.track_index, 2);
        assert_eq!(next.title, "Song C");
        assert_eq!(prev.track_index, 0);
        assert_eq!(prev.title, "Song A");
    }

    #[test]
    fn track_details_reports_availability_video_and_liked_facts() {
        let tmp = tempfile::tempdir().unwrap();
        let album = "Details Album";
        write_track(tmp.path(), album, "Song A", "mp4");
        let mut concert = bare_concert(Some(album), &["Song A", "Song B"]);
        concert.tracks_present = vec![true, false];
        concert.tracks_liked = vec![true, true];

        let details = track_details(tmp.path(), &concert).unwrap();

        assert_eq!(details.len(), 2);
        assert!(details[0].available);
        assert!(details[0].is_video);
        assert!(details[0].liked);
        assert!(!details[1].available);
        assert!(!details[1].is_video);
        assert!(details[1].liked);
    }

    #[test]
    fn track_details_preserves_empty_album_behavior() {
        let tmp = tempfile::tempdir().unwrap();
        let mut concert = bare_concert(None, &["Song A"]);
        concert.tracks_present = vec![true];
        concert.tracks_liked = vec![true];

        let details = track_details(tmp.path(), &concert).unwrap();

        assert_eq!(details.len(), 1);
        assert!(details[0].available);
        assert!(!details[0].is_video);
        assert!(details[0].liked);
    }
}
