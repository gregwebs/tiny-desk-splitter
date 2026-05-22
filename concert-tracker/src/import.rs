use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;
use tiny_desk_scraper::ConcertInfo;

use crate::scrape::apply_concert_info;

/// Import all *.json concert files from a directory (skipping listing_* files).
pub fn import_dir(conn: &Connection, dir: &Path) -> Result<usize> {
    let mut count = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name.starts_with("listing_") {
            continue;
        }
        match import_file(conn, &path) {
            Ok(()) => count += 1,
            Err(e) => eprintln!("Warning: skipping {}: {}", path.display(), e),
        }
    }
    Ok(count)
}

/// Deserialize a single ConcertInfo JSON file and apply it to the database.
pub fn import_file(conn: &Connection, path: &Path) -> Result<()> {
    let content = std::fs::read_to_string(path)?;
    let info: ConcertInfo = serde_json::from_str(&content)?;
    apply_concert_info(conn, &info)
}
