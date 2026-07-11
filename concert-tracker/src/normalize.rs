use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;
use tiny_desk_scraper::ConcertInfo;

use crate::db;
use crate::model::sanitize_album;
use crate::scrape::{apply_concert_info, scrape_url};

#[derive(Debug, Default)]
pub struct NormalizeReport {
    pub merged: usize,
    pub scraped: usize,
    pub renamed: usize,
    pub already_ok: usize,
    pub imported_to_db: usize,
    pub old_files_removed: usize,
    pub missing_source: Vec<String>,
    pub errors: Vec<String>,
}

pub fn normalize_metadata(
    conn: &Connection,
    working_dir: &Path,
    metadata_dir: &Path,
    dry_run: bool,
) -> Result<NormalizeReport> {
    let mut report = NormalizeReport::default();

    let concerts_dir = working_dir.join("concerts");
    if !concerts_dir.is_dir() {
        return Ok(report);
    }

    let concerts = db::concerts::list_concerts(conn)?;
    let album_lookup: HashMap<String, &crate::model::Concert> = concerts
        .iter()
        .filter_map(|c| c.album.as_deref().map(|a| (sanitize_album(a), c)))
        .collect();

    let mut entries: Vec<_> = fs::read_dir(&concerts_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let dir = entry.path();
        let dir_name = entry.file_name().to_string_lossy().to_string();
        let concert_json = dir.join("concert.json");

        if concert_json.exists() {
            report.already_ok += 1;
            continue;
        }

        let non_standard_json = find_non_standard_json(&dir);

        match non_standard_json {
            Some(ref in_dir_path) => {
                let in_dir_filename = in_dir_path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                let metadata_path = metadata_dir.join(&in_dir_filename);

                if metadata_path.exists() {
                    match merge_metadata_and_timestamps(&metadata_path, in_dir_path) {
                        Ok(merged) => {
                            if merged.source.is_empty() {
                                report.missing_source.push(dir_name.clone());
                            }
                            if dry_run {
                                tracing::info!(
                                    "[dry-run] would write merged concert.json to {}",
                                    concert_json.display()
                                );
                            } else {
                                write_concert_json(&concert_json, &merged)?;
                                apply_concert_info(conn, &merged)?;
                                report.imported_to_db += 1;
                                fs::remove_file(in_dir_path).with_context(|| {
                                    format!("remove old json {}", in_dir_path.display())
                                })?;
                                report.old_files_removed += 1;
                                tracing::info!(
                                    "merged concert-metadata + timestamps -> {}",
                                    concert_json.display()
                                );
                            }
                            report.merged += 1;
                        }
                        Err(e) => {
                            report.errors.push(format!("{}: {}", dir_name, e));
                        }
                    }
                } else {
                    // No concert-metadata match — rename in-dir json to concert.json
                    if dry_run {
                        tracing::info!(
                            "[dry-run] would rename {} -> concert.json",
                            in_dir_path.display()
                        );
                    } else {
                        fs::rename(in_dir_path, &concert_json).with_context(|| {
                            format!("rename {} -> concert.json", in_dir_path.display())
                        })?;
                        let info: ConcertInfo =
                            serde_json::from_str(&fs::read_to_string(&concert_json)?)?;
                        if info.source.is_empty() {
                            report.missing_source.push(dir_name.clone());
                        } else {
                            apply_concert_info(conn, &info)?;
                            report.imported_to_db += 1;
                        }
                        tracing::info!("renamed -> {}", concert_json.display());
                    }
                    report.renamed += 1;
                }
            }
            None => {
                // No JSON at all — try to re-scrape from DB source URL
                let db_concert = album_lookup
                    .get(dir_name.as_str())
                    .or_else(|| find_by_prefix(&album_lookup, &dir_name));
                if let Some(concert) = db_concert {
                    if dry_run {
                        tracing::info!(
                            "[dry-run] would scrape {} for {}",
                            concert.source_url,
                            dir_name
                        );
                    } else {
                        match scrape_url(conn, &concert.source_url, working_dir) {
                            Ok(()) => {
                                tracing::info!(
                                    "scraped {} -> {}",
                                    concert.source_url,
                                    concert_json.display()
                                );
                            }
                            Err(e) => {
                                report
                                    .errors
                                    .push(format!("{}: scrape failed: {}", dir_name, e));
                            }
                        }
                    }
                    report.scraped += 1;
                } else {
                    report
                        .errors
                        .push(format!("{}: no JSON and not found in database", dir_name));
                }
            }
        }
    }

    Ok(report)
}

fn find_by_prefix<'a>(
    lookup: &'a HashMap<String, &'a crate::model::Concert>,
    prefix: &str,
) -> Option<&'a &'a crate::model::Concert> {
    let matches: Vec<_> = lookup
        .iter()
        .filter(|(sanitized, _)| sanitized.starts_with(prefix))
        .collect();
    if matches.len() == 1 {
        Some(matches[0].1)
    } else {
        None
    }
}

fn find_non_standard_json(dir: &Path) -> Option<std::path::PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path.file_name()?.to_string_lossy().to_string();
        if name.ends_with(".json") && name != "concert.json" && name != "timestamps.json" {
            return Some(path);
        }
    }
    None
}

fn merge_metadata_and_timestamps(
    metadata_path: &Path,
    timestamps_path: &Path,
) -> Result<ConcertInfo> {
    let metadata_content = fs::read_to_string(metadata_path)
        .with_context(|| format!("read {}", metadata_path.display()))?;
    let mut merged: ConcertInfo = serde_json::from_str(&metadata_content)
        .with_context(|| format!("parse {}", metadata_path.display()))?;

    let ts_content = fs::read_to_string(timestamps_path)
        .with_context(|| format!("read {}", timestamps_path.display()))?;
    let ts_info: ConcertInfo = serde_json::from_str(&ts_content)
        .with_context(|| format!("parse {}", timestamps_path.display()))?;

    if merged.timestamps.is_none() {
        merged.timestamps = ts_info.timestamps;
    }

    Ok(merged)
}

fn write_concert_json(path: &Path, info: &ConcertInfo) -> Result<()> {
    let json = serde_json::to_string_pretty(info).context("serialize concert info")?;
    fs::write(path, json).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use tempfile::TempDir;

    fn setup_db() -> (TempDir, Connection) {
        let td = TempDir::new().unwrap();
        let db_path = td.path().join("test.db");
        let conn = db::connection::open(&db_path).unwrap();
        (td, conn)
    }

    fn make_concert_dir(base: &Path, name: &str) -> std::path::PathBuf {
        let dir = base.join("concerts").join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_json(path: &Path, json: &serde_json::Value) {
        fs::write(path, serde_json::to_string_pretty(json).unwrap()).unwrap();
    }

    fn minimal_indir_json() -> serde_json::Value {
        serde_json::json!({
            "artist": "Test Artist",
            "album": "Test Artist Tiny Desk Concert",
            "date": "2025-01-01T05:00:00-05:00",
            "show": "Tiny Desk Concerts",
            "set_list": [{"title": "Song One"}, {"title": "Song Two"}],
            "timestamps": [
                {"title": "Song One", "start_time": 0.0, "end_time": 180.0, "duration": 180.0},
                {"title": "Song Two", "start_time": 180.0, "end_time": 360.0, "duration": 180.0}
            ]
        })
    }

    fn full_metadata_json() -> serde_json::Value {
        serde_json::json!({
            "artist": "Test Artist",
            "source": "https://www.npr.org/test-artist-tiny-desk-concert",
            "show": "Tiny Desk Concerts",
            "date": "2025-01-01T05:00:00-05:00",
            "album": "Test Artist Tiny Desk Concert",
            "description": "A wonderful performance.",
            "set_list": [{"title": "Song One"}, {"title": "Song Two"}],
            "musicians": [
                {"name": "Test Artist", "instruments": ["vocals", "guitar"]}
            ]
        })
    }

    #[test]
    fn merge_with_metadata_and_timestamps() {
        let td = TempDir::new().unwrap();
        let metadata_path = td.path().join("test.json");
        let indir_path = td.path().join("indir.json");

        write_json(&metadata_path, &full_metadata_json());
        write_json(&indir_path, &minimal_indir_json());

        let merged = merge_metadata_and_timestamps(&metadata_path, &indir_path).unwrap();

        assert_eq!(
            merged.source,
            "https://www.npr.org/test-artist-tiny-desk-concert"
        );
        assert_eq!(
            merged.description.as_deref(),
            Some("A wonderful performance.")
        );
        assert_eq!(merged.musicians.len(), 1);
        assert!(merged.timestamps.is_some());
        assert_eq!(merged.timestamps.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn skip_if_concert_json_exists() {
        let (_db_td, conn) = setup_db();
        let td = TempDir::new().unwrap();
        let dir = make_concert_dir(td.path(), "Already Done Tiny Desk Concert");
        write_json(&dir.join("concert.json"), &full_metadata_json());

        let metadata_dir = td.path().join("concert-metadata");
        fs::create_dir_all(&metadata_dir).unwrap();

        let report = normalize_metadata(&conn, td.path(), &metadata_dir, false).unwrap();

        assert_eq!(report.already_ok, 1);
        assert_eq!(report.merged, 0);
    }

    #[test]
    fn merge_writes_concert_json_and_removes_old() {
        let (_db_td, conn) = setup_db();
        let td = TempDir::new().unwrap();

        let dir = make_concert_dir(td.path(), "Test Artist Tiny Desk Concert");
        let indir_json = dir.join("test_artist.json");
        write_json(&indir_json, &minimal_indir_json());

        let metadata_dir = td.path().join("concert-metadata");
        fs::create_dir_all(&metadata_dir).unwrap();
        write_json(
            &metadata_dir.join("test_artist.json"),
            &full_metadata_json(),
        );

        let report = normalize_metadata(&conn, td.path(), &metadata_dir, false).unwrap();

        assert_eq!(report.merged, 1);
        assert_eq!(report.old_files_removed, 1);
        assert_eq!(report.imported_to_db, 1);

        assert!(dir.join("concert.json").exists());
        assert!(!indir_json.exists());

        let written: ConcertInfo =
            serde_json::from_str(&fs::read_to_string(dir.join("concert.json")).unwrap()).unwrap();
        assert_eq!(
            written.source,
            "https://www.npr.org/test-artist-tiny-desk-concert"
        );
        assert!(written.timestamps.is_some());
    }

    #[test]
    fn rename_when_no_metadata_match() {
        let (_db_td, conn) = setup_db();
        let td = TempDir::new().unwrap();

        let dir = make_concert_dir(td.path(), "Obscure Band Tiny Desk Concert");
        let mut json = minimal_indir_json();
        json["source"] = serde_json::json!("https://www.npr.org/obscure-band");
        write_json(&dir.join("obscure_band.json"), &json);

        let metadata_dir = td.path().join("concert-metadata");
        fs::create_dir_all(&metadata_dir).unwrap();

        let report = normalize_metadata(&conn, td.path(), &metadata_dir, false).unwrap();

        assert_eq!(report.renamed, 1);
        assert!(dir.join("concert.json").exists());
        assert!(!dir.join("obscure_band.json").exists());
    }

    #[test]
    fn dry_run_makes_no_changes() {
        let (_db_td, conn) = setup_db();
        let td = TempDir::new().unwrap();

        let dir = make_concert_dir(td.path(), "Test Artist Tiny Desk Concert");
        let indir_json = dir.join("test_artist.json");
        write_json(&indir_json, &minimal_indir_json());

        let metadata_dir = td.path().join("concert-metadata");
        fs::create_dir_all(&metadata_dir).unwrap();
        write_json(
            &metadata_dir.join("test_artist.json"),
            &full_metadata_json(),
        );

        let report = normalize_metadata(&conn, td.path(), &metadata_dir, true).unwrap();

        assert_eq!(report.merged, 1);
        assert_eq!(report.old_files_removed, 0);
        assert_eq!(report.imported_to_db, 0);

        assert!(!dir.join("concert.json").exists());
        assert!(indir_json.exists());
    }

    #[test]
    fn reports_missing_source() {
        let (_db_td, conn) = setup_db();
        let td = TempDir::new().unwrap();

        let dir = make_concert_dir(td.path(), "No Source Tiny Desk Concert");
        write_json(&dir.join("no_source.json"), &minimal_indir_json());

        let metadata_dir = td.path().join("concert-metadata");
        fs::create_dir_all(&metadata_dir).unwrap();
        // metadata file also lacks source
        let mut meta = minimal_indir_json();
        meta.as_object_mut().unwrap().remove("timestamps");
        write_json(&metadata_dir.join("no_source.json"), &meta);

        let report = normalize_metadata(&conn, td.path(), &metadata_dir, false).unwrap();

        assert_eq!(report.missing_source.len(), 1);
        assert_eq!(report.missing_source[0], "No Source Tiny Desk Concert");
    }

    #[test]
    fn idempotent_rerun() {
        let (_db_td, conn) = setup_db();
        let td = TempDir::new().unwrap();

        let dir = make_concert_dir(td.path(), "Test Artist Tiny Desk Concert");
        write_json(&dir.join("test_artist.json"), &minimal_indir_json());

        let metadata_dir = td.path().join("concert-metadata");
        fs::create_dir_all(&metadata_dir).unwrap();
        write_json(
            &metadata_dir.join("test_artist.json"),
            &full_metadata_json(),
        );

        normalize_metadata(&conn, td.path(), &metadata_dir, false).unwrap();
        let report2 = normalize_metadata(&conn, td.path(), &metadata_dir, false).unwrap();

        assert_eq!(report2.already_ok, 1);
        assert_eq!(report2.merged, 0);
    }
}
