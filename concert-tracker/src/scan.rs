use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::path::Path;

use crate::concert_media::tracks_present_on_disk;
use crate::db;
use crate::model::{
    concert_dir, decide_backfill_duration, sanitize_album, DurationSource, SourceState,
};

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
                    let ext = mp4_path
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("mp4");
                    db::set_downloaded_extension_if_missing(conn, concert.id, ext)?;
                    report.downloads_found += 1;
                }
                Err(e) => report.errors.push(format!("{}: {}", mp4_path.display(), e)),
            }
        }

        let split_dir = expected_split_dir(dir, &album);
        if split_dir.exists() && has_split_tracks(&split_dir, &album) {
            match mtime_iso(&split_dir) {
                Ok(at) => {
                    db::set_split_at_if_missing(conn, concert.id, &at)?;
                    if concert.tracks_present.is_empty() && !concert.set_list.is_empty() {
                        let present = tracks_present_on_disk(dir, &album, &concert.set_list);
                        db::set_tracks_present(conn, concert.id, &present)?;
                    }
                    report.splits_found += 1;
                }
                Err(e) => report
                    .errors
                    .push(format!("{}: {}", split_dir.display(), e)),
            }
        }
    }

    Ok(report)
}

pub fn expected_mp4_path(dir: &Path, album: &str) -> std::path::PathBuf {
    concert_dir(dir, album).join(format!("{}.mp4", sanitize_album(album)))
}

pub fn expected_split_dir(dir: &Path, album: &str) -> std::path::PathBuf {
    concert_dir(dir, album)
}

/// Returns true if `dir` contains audio track files belonging to per-song
/// splits. The full-concert `{sanitize_album(album)}.mp4` lives in the same
/// directory; per-song video files (`.mp4`) are detected via audio sidecars
/// (`.m4a`, `.mp3`, ...) so a downloaded-but-not-split concert does not
/// falsely register as split.
pub fn has_split_tracks(dir: &Path, album: &str) -> bool {
    let audio_exts = ["mp3", "m4a", "wav", "flac", "ogg", "opus", "aac"];
    let full_stem = sanitize_album(album);
    std::fs::read_dir(dir)
        .map(|entries| {
            entries.filter_map(|e| e.ok()).any(|e| {
                let path = e.path();
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                if stem == full_stem {
                    return false;
                }
                path.extension()
                    .and_then(|x| x.to_str())
                    .map(|x| audio_exts.contains(&x))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

pub fn backfill_tracks_present(conn: &Connection, working_dir: &Path) -> usize {
    let concerts = match db::list_concerts_needing_tracks_backfill(conn) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("tracks_present backfill query failed: {}", e);
            return 0;
        }
    };
    let mut count = 0;
    for c in &concerts {
        if let Some(album) = c.album.as_deref() {
            let present = tracks_present_on_disk(working_dir, album, &c.set_list);
            if db::set_tracks_present(conn, c.id, &present).is_ok() {
                count += 1;
            }
        }
    }
    count
}

/// One concert whose `media_duration` was (or would be) backfilled.
#[derive(Debug, PartialEq)]
pub struct MediaDurationBackfillRow {
    pub id: i64,
    pub title: String,
    pub duration: f64,
    pub source: DurationSource,
}

pub struct MediaDurationBackfillReport {
    /// Rows that were written (`apply = true`) or would be written (`apply = false`).
    pub planned: Vec<MediaDurationBackfillRow>,
    /// `(id, title, reason)` for concerts left untouched.
    pub skipped: Vec<(i64, String, &'static str)>,
}

/// Backfill `media_duration` for concerts that predate the column (see
/// `model::decide_backfill_duration` for the safety rule). Read-only when
/// `apply` is `false` — gathers the same inputs and reports what *would* be
/// written without touching the database, so `concert_db backfill-media-duration
/// --dry-run` and `--confirm` share one code path.
pub fn backfill_media_duration(
    conn: &Connection,
    working_dir: &Path,
    apply: bool,
) -> Result<MediaDurationBackfillReport> {
    let concerts = db::list_concerts_missing_media_duration(conn)?;
    let mut report = MediaDurationBackfillReport {
        planned: Vec::new(),
        skipped: Vec::new(),
    };

    for c in &concerts {
        let Some(album) = c.album.as_deref() else {
            report.skipped.push((c.id, c.title.clone(), "no album"));
            continue;
        };

        let downloaded = crate::concert_media::find_downloaded_file(working_dir, album);
        let source_present = downloaded.is_some();
        let source = match downloaded {
            Some(path) => SourceState::Present(ffprobe_duration_sync(&path)),
            None => SourceState::Absent,
        };

        // Timestamps, in priority order: user split -> automated split (DB) ->
        // automated split (on-disk timestamps.json, lazy backfill).
        let stored = match db::get_split_timestamps(conn, c.id) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "media_duration backfill: failed to read split timestamps for concert {}: {}",
                    c.id,
                    e
                );
                report
                    .skipped
                    .push((c.id, c.title.clone(), "failed to read split timestamps"));
                continue;
            }
        };
        let timestamps = stored.user.or(stored.auto).or_else(|| {
            crate::jobs::split::read_analysis_timestamps(&concert_dir(working_dir, album)).ok()
        });

        // All-or-nothing track-duration sum: only attempted when every set-list
        // entry is marked present AND resolves to a real, probeable file. A
        // single miss skips the concert rather than summing a partial list.
        let all_tracks_present =
            !c.tracks_present.is_empty() && c.tracks_present.iter().all(|&p| p);
        let track_durations: Option<Vec<f64>> = if all_tracks_present {
            let mut durations = Vec::with_capacity(c.set_list.len());
            let mut complete = true;
            for title in &c.set_list {
                let probed = crate::concert_media::find_track_file(working_dir, album, title)
                    .map(|filename| concert_dir(working_dir, album).join(filename))
                    .and_then(|path| ffprobe_duration_sync(&path).ok());
                match probed {
                    Some(d) => durations.push(d),
                    None => {
                        complete = false;
                        break;
                    }
                }
            }
            complete.then_some(durations)
        } else {
            None
        };

        match decide_backfill_duration(source, timestamps.as_deref(), track_durations.as_deref()) {
            Some((duration, source_kind)) => {
                if apply {
                    if let Err(e) = db::set_media_duration(conn, c.id, duration) {
                        tracing::warn!(
                            "media_duration backfill: failed to write concert {}: {}",
                            c.id,
                            e
                        );
                        report
                            .skipped
                            .push((c.id, c.title.clone(), "set_media_duration failed"));
                        continue;
                    }
                }
                report.planned.push(MediaDurationBackfillRow {
                    id: c.id,
                    title: c.title.clone(),
                    duration,
                    source: source_kind,
                });
            }
            None => {
                let reason = if source_present {
                    "source present but ffprobe failed"
                } else {
                    "no duration source (no timestamps, no complete track set)"
                };
                report.skipped.push((c.id, c.title.clone(), reason));
            }
        }
    }

    Ok(report)
}

fn mtime_iso(path: &Path) -> Result<String> {
    let meta = std::fs::metadata(path)?;
    let mtime: DateTime<Utc> = meta.modified()?.into();
    Ok(mtime.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

/// Returns the duration in seconds of a media file using `ffprobe`. Synchronous
/// (`std::process::Command`) so it can run from non-async contexts (the
/// `concert_db` CLI backfill); the async web handler delegates here via
/// `spawn_blocking` rather than duplicating the subprocess logic.
pub fn ffprobe_duration_sync(path: &Path) -> Result<f64> {
    let output = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_entries",
            "format=duration",
        ])
        .arg(path)
        .output()
        .context("ffprobe not found")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ffprobe exited non-zero: {}", stderr);
    }

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("ffprobe output not valid JSON")?;
    let duration_str = json["format"]["duration"]
        .as_str()
        .context("ffprobe JSON missing format.duration")?;
    duration_str
        .parse::<f64>()
        .context("ffprobe duration not a float")
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

    fn make_concert_dir(working_dir: &Path, album: &str) -> std::path::PathBuf {
        let cd = concert_dir(working_dir, album);
        fs::create_dir_all(&cd).unwrap();
        cd
    }

    #[test]
    fn expected_paths_use_concerts_subdir_and_sanitize_colons() {
        let dir = std::path::Path::new("/tmp");
        assert_eq!(
            expected_mp4_path(dir, "Bob Dylan: Live"),
            std::path::PathBuf::from("/tmp/concerts/Bob Dylan Live/Bob Dylan Live.mp4")
        );
        assert_eq!(
            expected_split_dir(dir, "Bob Dylan: Live"),
            std::path::PathBuf::from("/tmp/concerts/Bob Dylan Live")
        );
    }

    #[test]
    fn has_split_tracks_false_for_empty_dir() {
        let dir = temp_dir();
        assert!(!has_split_tracks(dir.path(), "Anything"));
    }

    #[test]
    fn has_split_tracks_false_for_non_audio_files() {
        let dir = temp_dir();
        fs::write(dir.path().join("cover.jpg"), b"").unwrap();
        fs::write(dir.path().join("notes.txt"), b"").unwrap();
        assert!(!has_split_tracks(dir.path(), "Anything"));
    }

    #[test]
    fn has_split_tracks_true_for_audio_files() {
        let dir = temp_dir();
        fs::write(dir.path().join("01 - Song.mp3"), b"").unwrap();
        assert!(has_split_tracks(dir.path(), "Anything"));
    }

    #[test]
    fn has_split_tracks_recognizes_all_audio_extensions() {
        for ext in &["m4a", "wav", "flac", "ogg", "opus", "aac"] {
            let dir = temp_dir();
            fs::write(dir.path().join(format!("track.{}", ext)), b"").unwrap();
            assert!(
                has_split_tracks(dir.path(), "Anything"),
                "should detect .{}",
                ext
            );
        }
    }

    #[test]
    fn has_split_tracks_ignores_full_concert_mp4() {
        let dir = temp_dir();
        fs::write(dir.path().join("Foo Album.mp4"), b"").unwrap();
        assert!(!has_split_tracks(dir.path(), "Foo Album"));
    }

    #[test]
    fn has_split_tracks_excludes_full_mp4_but_counts_per_song_audio() {
        let dir = temp_dir();
        fs::write(dir.path().join("Foo Album.mp4"), b"").unwrap();
        fs::write(dir.path().join("Track 1.m4a"), b"").unwrap();
        assert!(has_split_tracks(dir.path(), "Foo Album"));
    }

    #[test]
    fn scan_detects_mp4_and_sets_downloaded_at() {
        let dir = temp_dir();
        let conn = db::open_in_memory().unwrap();
        let id = seed_concert_with_album(&conn, "https://npr.org/c/1", "Test Album");
        let cd = make_concert_dir(dir.path(), "Test Album");
        fs::write(cd.join("Test Album.mp4"), b"fake mp4").unwrap();

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
        let cd = make_concert_dir(dir.path(), "Split Album");
        fs::write(cd.join("01 - Track.mp3"), b"").unwrap();

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
        let cd = make_concert_dir(dir.path(), "Existing Download");
        fs::write(cd.join("Existing Download.mp4"), b"").unwrap();

        scan(&conn, dir.path()).unwrap();
        let c = db::get_concert(&conn, id).unwrap();
        assert_eq!(c.downloaded_at, Some("2020-01-01T00:00:00Z".to_string()));
    }

    #[test]
    fn scan_does_not_count_split_when_only_full_mp4_present() {
        let dir = temp_dir();
        let conn = db::open_in_memory().unwrap();
        let id = seed_concert_with_album(&conn, "https://npr.org/c/4", "Just Downloaded");
        let cd = make_concert_dir(dir.path(), "Just Downloaded");
        fs::write(cd.join("Just Downloaded.mp4"), b"").unwrap();

        let report = scan(&conn, dir.path()).unwrap();
        assert_eq!(report.downloads_found, 1);
        assert_eq!(report.splits_found, 0);
        assert!(db::get_concert(&conn, id).unwrap().split_at.is_none());
    }

    // ── backfill_media_duration tests ─────────────────────────────────────────

    /// Create a tiny real audio file (a sine wave of `duration_secs`) at `path`
    /// using ffmpeg, so `ffprobe_duration_sync` has something real to measure.
    /// Returns `false` (and the caller should skip) when ffmpeg isn't installed.
    fn create_test_audio_sync(path: &Path, duration_secs: u32) -> bool {
        std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                &format!("sine=frequency=440:duration={duration_secs}"),
                "-c:a",
                "aac",
                "-b:a",
                "32k",
            ])
            .arg(path)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn seed_media_duration_concert(
        conn: &rusqlite::Connection,
        url: &str,
        album: &str,
        songs: &[&str],
    ) -> i64 {
        let id = seed_concert_with_album(conn, url, album);
        db::update_metadata(
            conn,
            id,
            &db::MetadataUpdate {
                artist: "Artist".to_string(),
                album: album.to_string(),
                description: None,
                set_list: songs.iter().map(|s| s.to_string()).collect(),
                musicians: vec![],
            },
        )
        .unwrap();
        id
    }

    #[test]
    fn backfill_media_duration_ffprobes_present_source() {
        let dir = temp_dir();
        let conn = db::open_in_memory().unwrap();
        let id = seed_media_duration_concert(&conn, "https://npr.org/m/1", "Probe Album", &[]);
        let cd = make_concert_dir(dir.path(), "Probe Album");
        let source = cd.join("Probe Album.m4a");
        if !create_test_audio_sync(&source, 5) {
            eprintln!("skipping: ffmpeg not available");
            return;
        }

        let report = backfill_media_duration(&conn, dir.path(), true).unwrap();
        assert_eq!(report.skipped, Vec::new());
        assert_eq!(report.planned.len(), 1);
        assert_eq!(report.planned[0].source, DurationSource::Ffprobe);
        assert!(
            (report.planned[0].duration - 5.0).abs() < 0.5,
            "expected ~5s, got {}",
            report.planned[0].duration
        );

        let c = db::get_concert(&conn, id).unwrap();
        let stored = c.media_duration.expect("media_duration should be set");
        assert!((stored - 5.0).abs() < 0.5);
    }

    #[test]
    fn backfill_media_duration_absent_source_uses_stored_auto_timestamps() {
        let dir = temp_dir();
        let conn = db::open_in_memory().unwrap();
        let id = seed_media_duration_concert(
            &conn,
            "https://npr.org/m/2",
            "Timestamps Album",
            &["Song A", "Song B"],
        );
        // No source file on disk — concert dir doesn't even need to exist.
        let ts = vec![
            concert_types::SongTimestamp {
                title: "Song A".to_string(),
                start_time: 0.0,
                end_time: 50.0,
                duration: 50.0,
            },
            concert_types::SongTimestamp {
                title: "Song B".to_string(),
                start_time: 55.0,
                end_time: 120.0,
                duration: 65.0,
            },
        ];
        db::set_auto_split_timestamps(&conn, id, &ts).unwrap();

        let report = backfill_media_duration(&conn, dir.path(), true).unwrap();
        assert_eq!(report.skipped, Vec::new());
        assert_eq!(report.planned.len(), 1);
        assert_eq!(report.planned[0].source, DurationSource::Timestamps);
        assert_eq!(report.planned[0].duration, 120.0);
        assert_eq!(
            db::get_concert(&conn, id).unwrap().media_duration,
            Some(120.0)
        );
    }

    #[test]
    fn backfill_media_duration_absent_source_no_timestamps_sums_tracks() {
        let dir = temp_dir();
        let conn = db::open_in_memory().unwrap();
        let id = seed_media_duration_concert(
            &conn,
            "https://npr.org/m/3",
            "TrackSum Album",
            &["Song A", "Song B"],
        );
        db::set_tracks_present(&conn, id, &[true, true]).unwrap();
        let cd = make_concert_dir(dir.path(), "TrackSum Album");
        if !create_test_audio_sync(&cd.join("Song A.m4a"), 3)
            || !create_test_audio_sync(&cd.join("Song B.m4a"), 4)
        {
            eprintln!("skipping: ffmpeg not available");
            return;
        }

        let report = backfill_media_duration(&conn, dir.path(), true).unwrap();
        assert_eq!(report.skipped, Vec::new());
        assert_eq!(report.planned.len(), 1);
        assert_eq!(report.planned[0].source, DurationSource::TrackSum);
        assert!(
            (report.planned[0].duration - 7.0).abs() < 1.0,
            "expected ~7s (3+4), got {}",
            report.planned[0].duration
        );
    }

    #[test]
    fn backfill_media_duration_skips_when_track_set_incomplete() {
        let dir = temp_dir();
        let conn = db::open_in_memory().unwrap();
        let id = seed_media_duration_concert(
            &conn,
            "https://npr.org/m/4",
            "Incomplete Album",
            &["Song A", "Song B"],
        );
        // tracks_present says both are there, but only one file actually exists.
        db::set_tracks_present(&conn, id, &[true, true]).unwrap();
        let cd = make_concert_dir(dir.path(), "Incomplete Album");
        if !create_test_audio_sync(&cd.join("Song A.m4a"), 3) {
            eprintln!("skipping: ffmpeg not available");
            return;
        }

        let report = backfill_media_duration(&conn, dir.path(), true).unwrap();
        assert_eq!(report.planned, Vec::new());
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].0, id);
        assert!(db::get_concert(&conn, id).unwrap().media_duration.is_none());
    }

    #[test]
    fn backfill_media_duration_dry_run_does_not_write() {
        let dir = temp_dir();
        let conn = db::open_in_memory().unwrap();
        let id = seed_media_duration_concert(&conn, "https://npr.org/m/5", "DryRun Album", &[]);
        let ts = vec![concert_types::SongTimestamp {
            title: "Only Song".to_string(),
            start_time: 0.0,
            end_time: 42.0,
            duration: 42.0,
        }];
        db::set_auto_split_timestamps(&conn, id, &ts).unwrap();

        let report = backfill_media_duration(&conn, dir.path(), false).unwrap();
        assert_eq!(report.planned.len(), 1);
        assert_eq!(report.planned[0].duration, 42.0);
        assert!(
            db::get_concert(&conn, id).unwrap().media_duration.is_none(),
            "dry run must not write to the database"
        );
    }

    #[test]
    fn backfill_media_duration_is_idempotent_and_excludes_already_set_rows() {
        let dir = temp_dir();
        let conn = db::open_in_memory().unwrap();
        let id = seed_media_duration_concert(&conn, "https://npr.org/m/6", "Idempotent Album", &[]);
        let ts = vec![concert_types::SongTimestamp {
            title: "Only Song".to_string(),
            start_time: 0.0,
            end_time: 33.0,
            duration: 33.0,
        }];
        db::set_auto_split_timestamps(&conn, id, &ts).unwrap();

        let first = backfill_media_duration(&conn, dir.path(), true).unwrap();
        assert_eq!(first.planned.len(), 1);
        assert_eq!(
            db::get_concert(&conn, id).unwrap().media_duration,
            Some(33.0)
        );

        // Second run: list_concerts_missing_media_duration excludes the now-set
        // row, so it's neither planned nor skipped — and the value is untouched.
        let second = backfill_media_duration(&conn, dir.path(), true).unwrap();
        assert!(second.planned.is_empty());
        assert!(second.skipped.is_empty());
        assert_eq!(
            db::get_concert(&conn, id).unwrap().media_duration,
            Some(33.0)
        );
    }
}
