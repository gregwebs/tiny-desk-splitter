use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Deserialize;
use std::path::Path;

use crate::db;
use crate::events::Event;
use crate::model::sanitize_album;

#[derive(Deserialize)]
struct ArchiveJson {
    artist: String,
    album: String,
    #[allow(dead_code)]
    date: Option<String>,
    set_list: Vec<SetListEntry>,
}

#[derive(Deserialize)]
struct SetListEntry {
    title: String,
}

pub struct ImportReport {
    pub imported: usize,
    pub skipped: usize,
    pub not_in_db: Vec<String>,
    pub errors: Vec<String>,
}

enum ImportResult {
    Imported,
    AlreadyArchived,
    NotInDb(String),
}

pub fn import_archive(
    conn: &Connection,
    archive_dir: &Path,
    working_dir: &Path,
) -> Result<ImportReport> {
    let mut report = ImportReport {
        imported: 0,
        skipped: 0,
        not_in_db: Vec::new(),
        errors: Vec::new(),
    };

    let entries: Vec<_> = std::fs::read_dir(archive_dir)
        .with_context(|| {
            format!(
                "Failed to read archive directory: {}",
                archive_dir.display()
            )
        })?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .collect();

    for entry in &entries {
        let dir_path = entry.path();
        let dir_name = entry.file_name();
        let dir_name_str = dir_name.to_string_lossy();

        match import_one(conn, &dir_path, &dir_name_str, working_dir) {
            Ok(ImportResult::Imported) => {
                report.imported += 1;
                tracing::info!("imported archive: {}", dir_name_str);
            }
            Ok(ImportResult::AlreadyArchived) => {
                report.skipped += 1;
                tracing::debug!("skipped (already archived): {}", dir_name_str);
            }
            Ok(ImportResult::NotInDb(album)) => {
                report.skipped += 1;
                report.not_in_db.push(album);
            }
            Err(e) => {
                let msg = format!("{}: {}", dir_name_str, e);
                tracing::warn!("error importing {}", msg);
                report.errors.push(msg);
            }
        }
    }

    Ok(report)
}

fn import_one(
    conn: &Connection,
    dir_path: &Path,
    dir_name: &str,
    working_dir: &Path,
) -> Result<ImportResult> {
    let json_path = find_json_file(dir_path)?;
    let content = std::fs::read_to_string(&json_path)
        .with_context(|| format!("Failed to read {}", json_path.display()))?;
    let info: ArchiveJson = serde_json::from_str(&content)
        .with_context(|| format!("Bad JSON in {}", json_path.display()))?;

    let concert = match db::concerts::get_concert_by_album(conn, &info.album)? {
        Some(c) => c,
        None => return Ok(ImportResult::NotInDb(info.album)),
    };

    if concert.archived_at.is_some() {
        return Ok(ImportResult::AlreadyArchived);
    }

    if concert.metadata_scraped_at.is_none() {
        let set_list: Vec<String> = info.set_list.iter().map(|s| s.title.clone()).collect();
        db::concerts::update_metadata(
            conn,
            concert.id,
            &db::concerts::MetadataUpdate {
                artist: info.artist.clone(),
                album: info.album.clone(),
                description: None,
                set_list,
                musicians: vec![],
            },
        )?;
    }

    let mtime = dir_mtime(dir_path);
    db::lifecycle::set_downloaded_at_if_missing(conn, concert.id, &mtime)?;
    db::lifecycle::set_split_at_if_missing(conn, concert.id, &mtime)?;

    db::lifecycle::mark_archive_succeeded(conn, concert.id)?;

    let symlink_path = working_dir
        .join("concerts")
        .join(sanitize_album(&info.album));
    if !symlink_path.exists() {
        std::fs::create_dir_all(symlink_path.parent().unwrap())?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir_path, &symlink_path).with_context(|| {
            format!(
                "Failed to create symlink {} -> {}",
                symlink_path.display(),
                dir_path.display()
            )
        })?;
        tracing::debug!(
            "symlink {} -> {}",
            symlink_path.display(),
            dir_path.display()
        );
    }

    let json = serde_json::json!({"source": dir_name}).to_string();
    crate::events::record_now(conn, concert.id, Event::Import, Some(&json));

    Ok(ImportResult::Imported)
}

fn find_json_file(dir: &Path) -> Result<std::path::PathBuf> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "timestamps.json" {
            continue;
        }
        return Ok(path);
    }
    anyhow::bail!("no metadata JSON file found in {}", dir.display())
}

fn dir_mtime(dir: &Path) -> String {
    std::fs::metadata(dir)
        .and_then(|m| m.modified())
        .ok()
        .map(|t| {
            let dt: chrono::DateTime<chrono::Utc> = t.into();
            dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
        })
        .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn setup_db_with_concert(album: &str) -> (Connection, i64) {
        let conn = db::connection::open_in_memory().unwrap();
        let id = db::seeds::SeedContext::new(&conn)
            .seed_scraped_concert(db::seeds::SeedScrapedConcert {
                source_url: Some(format!("https://npr.org/{}", album)),
                title: Some(album.to_string()),
                concert_date: Some("2024-01-01".to_string()),
                artist: Some("Test".to_string()),
                album: Some(album.to_string()),
                set_list: Some(vec![]),
            })
            .unwrap()
            .id;
        (conn, id)
    }

    #[test]
    fn import_archive_creates_symlink_and_marks_archived() {
        let tmp = tempfile::tempdir().unwrap();
        let archive_dir = tmp.path().join("archive");
        let concert_dir = archive_dir.join("Test - Concert");
        std::fs::create_dir_all(&concert_dir).unwrap();

        let json = serde_json::json!({
            "artist": "Test",
            "album": "Test: Concert",
            "date": "2024-01-01",
            "set_list": [{"title": "Song A"}, {"title": "Song B"}]
        });
        std::fs::write(concert_dir.join("info.json"), json.to_string()).unwrap();
        std::fs::write(concert_dir.join("Song A.m4a"), b"audio").unwrap();

        let working_dir = tmp.path().join("workdir");
        std::fs::create_dir_all(&working_dir).unwrap();

        let (conn, id) = setup_db_with_concert("Test: Concert");
        let report = import_archive(&conn, &archive_dir, &working_dir).unwrap();

        assert_eq!(report.imported, 1);
        assert_eq!(report.skipped, 0);
        assert!(report.errors.is_empty());

        let c = db::concerts::get_concert(&conn, id).unwrap();
        assert!(c.archived_at.is_some());
        assert!(c.downloaded_at.is_some());
        assert!(c.split_at.is_some());

        let symlink = working_dir.join("concerts").join("Test Concert");
        assert!(symlink.is_symlink());
    }

    #[test]
    fn import_archive_skips_already_archived() {
        let tmp = tempfile::tempdir().unwrap();
        let archive_dir = tmp.path().join("archive");
        let concert_dir = archive_dir.join("Test - Concert");
        std::fs::create_dir_all(&concert_dir).unwrap();

        let json = serde_json::json!({
            "artist": "Test",
            "album": "Test: Concert",
            "set_list": []
        });
        std::fs::write(concert_dir.join("info.json"), json.to_string()).unwrap();

        let working_dir = tmp.path().join("workdir");
        std::fs::create_dir_all(&working_dir).unwrap();

        let (conn, _id) = setup_db_with_concert("Test: Concert");

        let r1 = import_archive(&conn, &archive_dir, &working_dir).unwrap();
        assert_eq!(r1.imported, 1);

        let r2 = import_archive(&conn, &archive_dir, &working_dir).unwrap();
        assert_eq!(r2.imported, 0);
        assert_eq!(r2.skipped, 1);
    }

    #[test]
    fn import_archive_skips_unknown_album() {
        let tmp = tempfile::tempdir().unwrap();
        let archive_dir = tmp.path().join("archive");
        let concert_dir = archive_dir.join("Unknown");
        std::fs::create_dir_all(&concert_dir).unwrap();

        let json = serde_json::json!({
            "artist": "Nobody",
            "album": "Not In DB",
            "set_list": []
        });
        std::fs::write(concert_dir.join("info.json"), json.to_string()).unwrap();

        let working_dir = tmp.path().join("workdir");
        let conn = db::connection::open_in_memory().unwrap();

        let report = import_archive(&conn, &archive_dir, &working_dir).unwrap();
        assert_eq!(report.imported, 0);
        assert_eq!(report.skipped, 1);
    }
}
