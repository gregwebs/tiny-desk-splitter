use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::path::Path;

use crate::db;
use crate::model::sanitize_album;

pub struct ScanReport {
    pub downloads_found: usize,
    pub splits_found: usize,
    pub errors: Vec<String>,
}

/// Scan a directory for existing MP4 downloads and split directories.
/// Sets downloaded_at / split_at timestamps from filesystem mtimes when missing.
pub fn scan(conn: &Connection, dir: &Path) -> Result<ScanReport> {
    let concerts = db::list_concerts(conn)?;
    let mut report = ScanReport {
        downloads_found: 0,
        splits_found: 0,
        errors: Vec::new(),
    };

    for concert in &concerts {
        let album = match &concert.album {
            Some(a) => a.clone(),
            None => continue,
        };

        let mp4_path = expected_mp4_path(dir, &album);
        if mp4_path.exists() {
            match mtime_iso(&mp4_path) {
                Ok(at) => {
                    db::set_downloaded_at_if_missing(conn, concert.id, &at)?;
                    report.downloads_found += 1;
                }
                Err(e) => report.errors.push(format!("{}: {}", mp4_path.display(), e)),
            }
        }

        let split_dir = expected_split_dir(dir, &album);
        if split_dir.exists() && has_split_tracks(&split_dir) {
            match mtime_iso(&split_dir) {
                Ok(at) => {
                    db::set_split_at_if_missing(conn, concert.id, &at)?;
                    report.splits_found += 1;
                }
                Err(e) => report.errors.push(format!("{}: {}", split_dir.display(), e)),
            }
        }
    }

    Ok(report)
}

pub fn expected_mp4_path(dir: &Path, album: &str) -> std::path::PathBuf {
    dir.join(format!("{}.mp4", sanitize_album(album)))
}

pub fn expected_split_dir(dir: &Path, album: &str) -> std::path::PathBuf {
    dir.join(sanitize_album(album))
}

/// Returns true if the directory contains any audio track files.
pub fn has_split_tracks(dir: &Path) -> bool {
    let audio_exts = ["mp3", "m4a", "wav", "flac", "ogg", "opus", "aac"];
    std::fs::read_dir(dir)
        .map(|entries| {
            entries.filter_map(|e| e.ok()).any(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| audio_exts.contains(&x))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn mtime_iso(path: &Path) -> Result<String> {
    let meta = std::fs::metadata(path)?;
    let mtime: DateTime<Utc> = meta.modified()?.into();
    Ok(mtime.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{self, MetadataUpdate, NewListing};
    use std::fs;
    use tempfile::TempDir;

    fn temp_dir() -> TempDir {
        tempfile::tempdir().unwrap()
    }

    fn seed_concert_with_album(conn: &rusqlite::Connection, url: &str, album: &str) -> i64 {
        db::upsert_listing(
            conn,
            &NewListing {
                source_url: url.to_string(),
                title: album.to_string(),
                concert_date: None,
                teaser: None,
            },
        )
        .unwrap();
        let c = db::get_concert_by_url(conn, url).unwrap().unwrap();
        db::update_metadata(
            conn,
            c.id,
            &MetadataUpdate {
                artist: "Artist".to_string(),
                album: album.to_string(),
                description: None,
                set_list: vec![],
                musicians: vec![],
            },
        )
        .unwrap();
        c.id
    }

    #[test]
    fn expected_paths_sanitize_colons() {
        let dir = std::path::Path::new("/tmp");
        assert_eq!(
            expected_mp4_path(dir, "Bob Dylan: Live"),
            std::path::PathBuf::from("/tmp/Bob Dylan Live.mp4")
        );
        assert_eq!(
            expected_split_dir(dir, "Bob Dylan: Live"),
            std::path::PathBuf::from("/tmp/Bob Dylan Live")
        );
    }

    #[test]
    fn has_split_tracks_false_for_empty_dir() {
        let dir = temp_dir();
        assert!(!has_split_tracks(dir.path()));
    }

    #[test]
    fn has_split_tracks_false_for_non_audio_files() {
        let dir = temp_dir();
        fs::write(dir.path().join("cover.jpg"), b"").unwrap();
        fs::write(dir.path().join("notes.txt"), b"").unwrap();
        assert!(!has_split_tracks(dir.path()));
    }

    #[test]
    fn has_split_tracks_true_for_audio_files() {
        let dir = temp_dir();
        fs::write(dir.path().join("01 - Song.mp3"), b"").unwrap();
        assert!(has_split_tracks(dir.path()));
    }

    #[test]
    fn has_split_tracks_recognizes_all_audio_extensions() {
        for ext in &["m4a", "wav", "flac", "ogg", "opus", "aac"] {
            let dir = temp_dir();
            fs::write(dir.path().join(format!("track.{}", ext)), b"").unwrap();
            assert!(has_split_tracks(dir.path()), "should detect .{}", ext);
        }
    }

    #[test]
    fn scan_detects_mp4_and_sets_downloaded_at() {
        let dir = temp_dir();
        let conn = db::open_in_memory().unwrap();
        let id = seed_concert_with_album(&conn, "https://npr.org/c/1", "Test Album");
        fs::write(dir.path().join("Test Album.mp4"), b"fake mp4").unwrap();

        let report = scan(&conn, dir.path()).unwrap();
        assert_eq!(report.downloads_found, 1);
        assert!(report.errors.is_empty());
        assert!(db::get_concert(&conn, id).unwrap().downloaded_at.is_some());
    }

    #[test]
    fn scan_detects_split_dir_and_sets_split_at() {
        let dir = temp_dir();
        let conn = db::open_in_memory().unwrap();
        let id = seed_concert_with_album(&conn, "https://npr.org/c/2", "Split Album");
        let split_dir = dir.path().join("Split Album");
        fs::create_dir(&split_dir).unwrap();
        fs::write(split_dir.join("01 - Track.mp3"), b"").unwrap();

        let report = scan(&conn, dir.path()).unwrap();
        assert_eq!(report.splits_found, 1);
        assert!(db::get_concert(&conn, id).unwrap().split_at.is_some());
    }

    #[test]
    fn scan_skips_concerts_without_album() {
        let dir = temp_dir();
        let conn = db::open_in_memory().unwrap();
        db::upsert_listing(
            &conn,
            &NewListing {
                source_url: "https://npr.org/c/noalbum".to_string(),
                title: "No Album Concert".to_string(),
                concert_date: None,
                teaser: None,
            },
        )
        .unwrap();

        let report = scan(&conn, dir.path()).unwrap();
        assert_eq!(report.downloads_found, 0);
        assert_eq!(report.splits_found, 0);
    }

    #[test]
    fn scan_does_not_overwrite_existing_downloaded_at() {
        let dir = temp_dir();
        let conn = db::open_in_memory().unwrap();
        let id = seed_concert_with_album(&conn, "https://npr.org/c/3", "Existing Download");
        db::set_downloaded_at_if_missing(&conn, id, "2020-01-01T00:00:00Z").unwrap();
        fs::write(dir.path().join("Existing Download.mp4"), b"").unwrap();

        scan(&conn, dir.path()).unwrap();
        let c = db::get_concert(&conn, id).unwrap();
        assert_eq!(c.downloaded_at, Some("2020-01-01T00:00:00Z".to_string()));
    }
}
