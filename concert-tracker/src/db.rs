use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, Row};
use std::path::Path;

use crate::model::{Concert, ErrorEntry, Musician};

const MIGRATION: &str = include_str!("../migrations/0001_init.sql");

pub struct NewListing {
    pub source_url: String,
    pub title: String,
    pub concert_date: Option<String>,
    pub teaser: Option<String>,
}

pub struct MetadataUpdate {
    pub artist: String,
    pub album: String,
    pub description: Option<String>,
    pub set_list: Vec<String>,
    pub musicians: Vec<Musician>,
}

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).context("Failed to open database")?;
    configure(&conn)?;
    Ok(conn)
}

pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory().context("Failed to open in-memory database")?;
    configure(&conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
        .context("Failed to configure pragmas")?;
    run_migrations(conn)
}

fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(MIGRATION)
        .context("Failed to run migrations")
}

fn concert_from_row(row: &Row) -> rusqlite::Result<Concert> {
    let set_list_json: Option<String> = row.get("set_list_json")?;
    let musicians_json: Option<String> = row.get("musicians_json")?;
    let download_errors_json: String = row.get("download_errors_json")?;
    let split_errors_json: String = row.get("split_errors_json")?;
    let ignored: i64 = row.get("ignored")?;
    let wanted: i64 = row.get("wanted")?;

    let set_list: Vec<String> = set_list_json
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default();
    let musicians: Vec<Musician> = musicians_json
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default();
    let download_errors: Vec<ErrorEntry> =
        serde_json::from_str(&download_errors_json).unwrap_or_default();
    let split_errors: Vec<ErrorEntry> =
        serde_json::from_str(&split_errors_json).unwrap_or_default();

    Ok(Concert {
        id: row.get("id")?,
        source_url: row.get("source_url")?,
        title: row.get("title")?,
        concert_date: row.get("concert_date")?,
        teaser: row.get("teaser")?,
        artist: row.get("artist")?,
        album: row.get("album")?,
        description: row.get("description")?,
        set_list,
        musicians,
        ignored: ignored != 0,
        wanted: wanted != 0,
        notes: row.get("notes")?,
        download_started_at: row.get("download_started_at")?,
        downloaded_at: row.get("downloaded_at")?,
        download_errors,
        split_started_at: row.get("split_started_at")?,
        split_at: row.get("split_at")?,
        split_errors,
        first_seen_at: row.get("first_seen_at")?,
        metadata_scraped_at: row.get("metadata_scraped_at")?,
    })
}

pub fn upsert_listing(conn: &Connection, listing: &NewListing) -> Result<()> {
    conn.execute(
        "INSERT INTO concerts (source_url, title, concert_date, teaser)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(source_url) DO UPDATE SET
             title = excluded.title,
             concert_date = COALESCE(excluded.concert_date, concerts.concert_date),
             teaser = COALESCE(excluded.teaser, concerts.teaser)",
        params![
            listing.source_url,
            listing.title,
            listing.concert_date,
            listing.teaser
        ],
    )
    .context("Failed to upsert listing")?;
    Ok(())
}

pub fn update_metadata(conn: &Connection, id: i64, update: &MetadataUpdate) -> Result<()> {
    let set_list_json = serde_json::to_string(&update.set_list)?;
    let musicians_json = serde_json::to_string(&update.musicians)?;
    conn.execute(
        "UPDATE concerts SET artist = ?1, album = ?2, description = ?3,
             set_list_json = ?4, musicians_json = ?5, metadata_scraped_at = datetime('now')
         WHERE id = ?6",
        params![
            update.artist,
            update.album,
            update.description,
            set_list_json,
            musicians_json,
            id
        ],
    )
    .context("Failed to update metadata")?;
    Ok(())
}

pub fn toggle_ignored(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE concerts SET
             ignored = CASE WHEN ignored = 0 THEN 1 ELSE 0 END,
             wanted  = CASE WHEN ignored = 0 THEN 0 ELSE wanted END
         WHERE id = ?1",
        params![id],
    )
    .context("Failed to toggle ignored")?;
    Ok(())
}

pub fn toggle_wanted(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE concerts SET
             wanted  = CASE WHEN wanted = 0 THEN 1 ELSE 0 END,
             ignored = CASE WHEN wanted = 0 THEN 0 ELSE ignored END
         WHERE id = ?1",
        params![id],
    )
    .context("Failed to toggle wanted")?;
    Ok(())
}

pub fn set_notes(conn: &Connection, id: i64, notes: &str) -> Result<()> {
    conn.execute(
        "UPDATE concerts SET notes = ?1 WHERE id = ?2",
        params![notes, id],
    )
    .context("Failed to set notes")?;
    Ok(())
}

/// Returns false if download is already in progress (started_at IS NOT NULL).
pub fn try_mark_download_started(conn: &Connection, id: i64) -> Result<bool> {
    let rows = conn
        .execute(
            "UPDATE concerts SET download_started_at = datetime('now')
             WHERE id = ?1 AND download_started_at IS NULL",
            params![id],
        )
        .context("Failed to mark download started")?;
    Ok(rows > 0)
}

pub fn mark_download_succeeded(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE concerts SET downloaded_at = datetime('now'), download_started_at = NULL
         WHERE id = ?1",
        params![id],
    )
    .context("Failed to mark download succeeded")?;
    Ok(())
}

pub fn mark_download_failed(conn: &Connection, id: i64, error: &str) -> Result<()> {
    append_error(conn, id, "download_errors_json", error)?;
    conn.execute(
        "UPDATE concerts SET download_started_at = NULL WHERE id = ?1",
        params![id],
    )
    .context("Failed to clear download_started_at")?;
    Ok(())
}

/// Returns false if split is already in progress or concert is not yet downloaded.
pub fn try_mark_split_started(conn: &Connection, id: i64) -> Result<bool> {
    let rows = conn
        .execute(
            "UPDATE concerts SET split_started_at = datetime('now')
             WHERE id = ?1 AND split_started_at IS NULL AND downloaded_at IS NOT NULL",
            params![id],
        )
        .context("Failed to mark split started")?;
    Ok(rows > 0)
}

pub fn mark_split_succeeded(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE concerts SET split_at = datetime('now'), split_started_at = NULL
         WHERE id = ?1",
        params![id],
    )
    .context("Failed to mark split succeeded")?;
    Ok(())
}

pub fn mark_split_failed(conn: &Connection, id: i64, error: &str) -> Result<()> {
    append_error(conn, id, "split_errors_json", error)?;
    conn.execute(
        "UPDATE concerts SET split_started_at = NULL WHERE id = ?1",
        params![id],
    )
    .context("Failed to clear split_started_at")?;
    Ok(())
}

/// Clear all download-related state. Wipes downloaded_at, download_started_at,
/// split_at, split_started_at, and split_errors. download_errors is preserved
/// (its history still applies to the failed-to-download state). split_errors
/// is wiped because those errors describe splitting a file that no longer
/// exists — keeping them would leave ProcessingStatus stuck at SplitError,
/// hiding the Download button.
pub fn clear_download_state(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE concerts SET downloaded_at = NULL, download_started_at = NULL,
                             split_at = NULL, split_started_at = NULL,
                             split_errors_json = '[]'
         WHERE id = ?1",
        params![id],
    )
    .context("Failed to clear download state")?;
    Ok(())
}

/// Clear split-related timestamps. Error history is preserved.
pub fn clear_split_state(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE concerts SET split_at = NULL, split_started_at = NULL WHERE id = ?1",
        params![id],
    )
    .context("Failed to clear split state")?;
    Ok(())
}

/// Set downloaded_at from filesystem mtime if not already set (for scan/recovery).
pub fn set_downloaded_at_if_missing(conn: &Connection, id: i64, at: &str) -> Result<()> {
    conn.execute(
        "UPDATE concerts SET downloaded_at = ?1 WHERE id = ?2 AND downloaded_at IS NULL",
        params![at, id],
    )
    .context("Failed to set downloaded_at")?;
    Ok(())
}

/// Set split_at from filesystem mtime if not already set (for scan/recovery).
pub fn set_split_at_if_missing(conn: &Connection, id: i64, at: &str) -> Result<()> {
    conn.execute(
        "UPDATE concerts SET split_at = ?1 WHERE id = ?2 AND split_at IS NULL",
        params![at, id],
    )
    .context("Failed to set split_at")?;
    Ok(())
}

/// Mark any concert whose download or split was in progress as failed with
/// the given error. Used at server startup to recover from an unclean
/// shutdown — the previous process's in-flight job is no longer running, so
/// the row must not stay pinned at Downloading / Splitting (which hides every
/// retry button in the UI). Each orphaned row gets an `ErrorEntry` appended
/// to its `*_errors_json`, leaving the concert in DownloadError / SplitError
/// state where the slot UI already exposes a retry button.
///
/// Returns `(download_count, split_count)` of rows touched.
pub fn fail_in_progress_jobs(conn: &Connection, error: &str) -> Result<(usize, usize)> {
    let dl_ids: Vec<i64> = conn
        .prepare("SELECT id FROM concerts WHERE download_started_at IS NOT NULL")?
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<_>>()
        .context("Failed to read in-progress downloads")?;
    for id in &dl_ids {
        mark_download_failed(conn, *id, error)?;
    }

    let sp_ids: Vec<i64> = conn
        .prepare("SELECT id FROM concerts WHERE split_started_at IS NOT NULL")?
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<_>>()
        .context("Failed to read in-progress splits")?;
    for id in &sp_ids {
        mark_split_failed(conn, *id, error)?;
    }

    Ok((dl_ids.len(), sp_ids.len()))
}

/// Clear all stale in-progress flags (e.g. after an unclean shutdown).
pub fn reset_in_progress(conn: &Connection) -> Result<usize> {
    let rows = conn
        .execute(
            "UPDATE concerts SET download_started_at = NULL, split_started_at = NULL
             WHERE download_started_at IS NOT NULL OR split_started_at IS NOT NULL",
            [],
        )
        .context("Failed to reset in-progress")?;
    Ok(rows)
}

pub fn list_concerts(conn: &Connection) -> Result<Vec<Concert>> {
    let mut stmt =
        conn.prepare("SELECT * FROM concerts ORDER BY concert_date DESC, first_seen_at DESC")?;
    let concerts = stmt
        .query_map([], concert_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to list concerts")?;
    Ok(concerts)
}

pub fn get_concert(conn: &Connection, id: i64) -> Result<Concert> {
    conn.query_row(
        "SELECT * FROM concerts WHERE id = ?1",
        params![id],
        concert_from_row,
    )
    .context("Concert not found")
}

pub fn get_concert_by_url(conn: &Connection, url: &str) -> Result<Option<Concert>> {
    let mut stmt = conn.prepare("SELECT * FROM concerts WHERE source_url = ?1")?;
    let mut iter = stmt.query_map(params![url], concert_from_row)?;
    match iter.next() {
        Some(row) => Ok(Some(row.context("Failed to read concert")?)),
        None => Ok(None),
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    fn listing(url: &str, title: &str) -> NewListing {
        NewListing {
            source_url: url.to_string(),
            title: title.to_string(),
            concert_date: Some("2024-06-01".to_string()),
            teaser: Some("Great show".to_string()),
        }
    }

    fn seed(conn: &Connection) -> i64 {
        upsert_listing(conn, &listing("https://npr.org/c/1", "Test Concert")).unwrap();
        let c = get_concert_by_url(conn, "https://npr.org/c/1")
            .unwrap()
            .unwrap();
        c.id
    }

    fn seed_with_album(conn: &Connection) -> i64 {
        let id = seed(conn);
        update_metadata(
            conn,
            id,
            &MetadataUpdate {
                artist: "Test Artist".to_string(),
                album: "Test Album".to_string(),
                description: Some("A great concert".to_string()),
                set_list: vec!["Song A".to_string(), "Song B".to_string()],
                musicians: vec![Musician {
                    name: "Alice".to_string(),
                    instruments: vec!["guitar".to_string()],
                }],
            },
        )
        .unwrap();
        id
    }

    #[test]
    fn upsert_listing_inserts_new_row() {
        let conn = open_in_memory().unwrap();
        upsert_listing(&conn, &listing("https://npr.org/c/1", "Concert A")).unwrap();
        let concerts = list_concerts(&conn).unwrap();
        assert_eq!(concerts.len(), 1);
        assert_eq!(concerts[0].title, "Concert A");
        assert_eq!(concerts[0].concert_date, Some("2024-06-01".to_string()));
    }

    #[test]
    fn upsert_listing_updates_title_on_conflict() {
        let conn = open_in_memory().unwrap();
        upsert_listing(&conn, &listing("https://npr.org/c/1", "Old Title")).unwrap();
        upsert_listing(&conn, &listing("https://npr.org/c/1", "New Title")).unwrap();
        let concerts = list_concerts(&conn).unwrap();
        assert_eq!(concerts.len(), 1);
        assert_eq!(concerts[0].title, "New Title");
    }

    #[test]
    fn upsert_listing_preserves_intent_on_conflict() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        toggle_wanted(&conn, id).unwrap();
        upsert_listing(&conn, &listing("https://npr.org/c/1", "Updated")).unwrap();
        let c = get_concert(&conn, id).unwrap();
        assert!(c.wanted, "re-upsert must not clear wanted flag");
    }

    #[test]
    fn update_metadata_stores_all_fields() {
        let conn = open_in_memory().unwrap();
        let id = seed_with_album(&conn);
        let c = get_concert(&conn, id).unwrap();
        assert_eq!(c.artist, Some("Test Artist".to_string()));
        assert_eq!(c.album, Some("Test Album".to_string()));
        assert_eq!(c.description, Some("A great concert".to_string()));
        assert_eq!(c.set_list, vec!["Song A", "Song B"]);
        assert_eq!(c.musicians.len(), 1);
        assert_eq!(c.musicians[0].name, "Alice");
        assert!(c.metadata_scraped_at.is_some());
    }

    #[test]
    fn toggle_ignored_flips_flag_and_clears_wanted() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);

        // Start wanted, then ignore — wanted should be cleared
        toggle_wanted(&conn, id).unwrap();
        assert!(get_concert(&conn, id).unwrap().wanted);
        toggle_ignored(&conn, id).unwrap();
        let c = get_concert(&conn, id).unwrap();
        assert!(c.ignored);
        assert!(!c.wanted);

        // Toggle ignored off — ignored cleared, wanted stays off
        toggle_ignored(&conn, id).unwrap();
        let c = get_concert(&conn, id).unwrap();
        assert!(!c.ignored);
        assert!(!c.wanted);
    }

    #[test]
    fn toggle_wanted_flips_flag_and_clears_ignored() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);

        toggle_ignored(&conn, id).unwrap();
        assert!(get_concert(&conn, id).unwrap().ignored);
        toggle_wanted(&conn, id).unwrap();
        let c = get_concert(&conn, id).unwrap();
        assert!(c.wanted);
        assert!(!c.ignored);
    }

    #[test]
    fn set_notes_persists_text() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        set_notes(&conn, id, "saw this live, amazing").unwrap();
        let c = get_concert(&conn, id).unwrap();
        assert_eq!(c.notes, Some("saw this live, amazing".to_string()));
    }

    #[test]
    fn try_mark_download_started_blocks_double_start() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        assert!(try_mark_download_started(&conn, id).unwrap());
        assert!(!try_mark_download_started(&conn, id).unwrap());
        assert!(get_concert(&conn, id)
            .unwrap()
            .download_started_at
            .is_some());
    }

    #[test]
    fn mark_download_succeeded_clears_started_at_and_sets_downloaded_at() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id).unwrap();
        let c = get_concert(&conn, id).unwrap();
        assert!(c.downloaded_at.is_some());
        assert!(c.download_started_at.is_none());
    }

    #[test]
    fn mark_download_failed_clears_started_at_and_accumulates_errors() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_failed(&conn, id, "timeout").unwrap();
        let c = get_concert(&conn, id).unwrap();
        assert!(c.download_started_at.is_none());
        assert_eq!(c.download_errors.len(), 1);
        assert_eq!(c.download_errors[0].error, "timeout");

        // Second failure appends, does not replace
        try_mark_download_started(&conn, id).unwrap();
        mark_download_failed(&conn, id, "403 forbidden").unwrap();
        let c2 = get_concert(&conn, id).unwrap();
        assert_eq!(c2.download_errors.len(), 2);
    }

    #[test]
    fn try_mark_split_started_requires_downloaded_at() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        // No downloaded_at yet — should return false
        assert!(!try_mark_split_started(&conn, id).unwrap());

        mark_download_succeeded(&conn, id).unwrap();
        // Now it should succeed
        assert!(try_mark_split_started(&conn, id).unwrap());
        // Double start blocked
        assert!(!try_mark_split_started(&conn, id).unwrap());
    }

    #[test]
    fn mark_split_succeeded_and_failed() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        mark_download_succeeded(&conn, id).unwrap();

        try_mark_split_started(&conn, id).unwrap();
        mark_split_failed(&conn, id, "ffmpeg error").unwrap();
        let c = get_concert(&conn, id).unwrap();
        assert!(c.split_started_at.is_none());
        assert_eq!(c.split_errors.len(), 1);

        try_mark_split_started(&conn, id).unwrap();
        mark_split_succeeded(&conn, id).unwrap();
        let c2 = get_concert(&conn, id).unwrap();
        assert!(c2.split_at.is_some());
        assert!(c2.split_started_at.is_none());
    }

    #[test]
    fn reset_in_progress_clears_stale_flags_and_returns_count() {
        let conn = open_in_memory().unwrap();
        let id1 = seed(&conn);
        upsert_listing(&conn, &listing("https://npr.org/c/2", "B")).unwrap();
        let id2 = get_concert_by_url(&conn, "https://npr.org/c/2")
            .unwrap()
            .unwrap()
            .id;

        try_mark_download_started(&conn, id1).unwrap();
        mark_download_succeeded(&conn, id1).unwrap();
        try_mark_split_started(&conn, id1).unwrap(); // split in progress
        try_mark_download_started(&conn, id2).unwrap(); // download in progress

        let cleared = reset_in_progress(&conn).unwrap();
        assert_eq!(cleared, 2);
        assert!(get_concert(&conn, id1).unwrap().split_started_at.is_none());
        assert!(get_concert(&conn, id2)
            .unwrap()
            .download_started_at
            .is_none());
    }

    #[test]
    fn fail_in_progress_jobs_appends_error_and_clears_flags() {
        let conn = open_in_memory().unwrap();
        let id1 = seed(&conn);
        upsert_listing(&conn, &listing("https://npr.org/c/2", "B")).unwrap();
        let id2 = get_concert_by_url(&conn, "https://npr.org/c/2")
            .unwrap()
            .unwrap()
            .id;

        // id1: split in progress; id2: download in progress.
        try_mark_download_started(&conn, id1).unwrap();
        mark_download_succeeded(&conn, id1).unwrap();
        try_mark_split_started(&conn, id1).unwrap();
        try_mark_download_started(&conn, id2).unwrap();

        let (dl, sp) = fail_in_progress_jobs(&conn, "server restarted").unwrap();
        assert_eq!(dl, 1);
        assert_eq!(sp, 1);

        let c1 = get_concert(&conn, id1).unwrap();
        assert!(c1.split_started_at.is_none());
        assert_eq!(c1.split_errors.last().unwrap().error, "server restarted");

        let c2 = get_concert(&conn, id2).unwrap();
        assert!(c2.download_started_at.is_none());
        assert_eq!(c2.download_errors.last().unwrap().error, "server restarted");

        // Idempotent: a second call on the now-clean state touches nothing.
        let (dl2, sp2) = fail_in_progress_jobs(&conn, "server restarted").unwrap();
        assert_eq!((dl2, sp2), (0, 0));
    }

    #[test]
    fn set_downloaded_at_if_missing_is_idempotent() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        set_downloaded_at_if_missing(&conn, id, "2024-01-01T00:00:00Z").unwrap();
        set_downloaded_at_if_missing(&conn, id, "2025-12-31T00:00:00Z").unwrap(); // must not overwrite
        let c = get_concert(&conn, id).unwrap();
        assert_eq!(c.downloaded_at, Some("2024-01-01T00:00:00Z".to_string()));
    }

    #[test]
    fn clear_download_state_nulls_timestamps_and_resets_split_errors() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_failed(&conn, id, "earlier 403").unwrap();
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id).unwrap();
        try_mark_split_started(&conn, id).unwrap();
        mark_split_failed(&conn, id, "ffmpeg blew up").unwrap();
        try_mark_split_started(&conn, id).unwrap();
        mark_split_succeeded(&conn, id).unwrap();

        clear_download_state(&conn, id).unwrap();

        let c = get_concert(&conn, id).unwrap();
        assert!(c.downloaded_at.is_none());
        assert!(c.download_started_at.is_none());
        assert!(c.split_at.is_none());
        assert!(c.split_started_at.is_none());
        // download_errors stays as audit trail of past download attempts.
        assert_eq!(c.download_errors.len(), 1);
        assert_eq!(c.download_errors[0].error, "earlier 403");
        // split_errors must be wiped — they described a file that no longer
        // exists, and preserving them would pin ProcessingStatus at SplitError
        // and hide the Download button.
        assert!(c.split_errors.is_empty());
    }

    #[test]
    fn clear_split_state_nulls_only_split_columns() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id).unwrap();
        try_mark_split_started(&conn, id).unwrap();
        mark_split_failed(&conn, id, "first try").unwrap();
        try_mark_split_started(&conn, id).unwrap();
        mark_split_succeeded(&conn, id).unwrap();

        clear_split_state(&conn, id).unwrap();

        let c = get_concert(&conn, id).unwrap();
        assert!(
            c.downloaded_at.is_some(),
            "download state must be untouched"
        );
        assert!(c.split_at.is_none());
        assert!(c.split_started_at.is_none());
        assert_eq!(c.split_errors.len(), 1, "split errors preserved");
    }

    #[test]
    fn list_concerts_returns_all_rows() {
        let conn = open_in_memory().unwrap();
        upsert_listing(&conn, &listing("https://npr.org/c/1", "A")).unwrap();
        upsert_listing(&conn, &listing("https://npr.org/c/2", "B")).unwrap();
        assert_eq!(list_concerts(&conn).unwrap().len(), 2);
    }

    #[test]
    fn get_concert_by_url_returns_none_when_missing() {
        let conn = open_in_memory().unwrap();
        let result = get_concert_by_url(&conn, "https://npr.org/missing").unwrap();
        assert!(result.is_none());
    }
}

fn append_error(conn: &Connection, id: i64, column: &str, error: &str) -> Result<()> {
    assert!(
        column == "download_errors_json" || column == "split_errors_json",
        "invalid error column"
    );
    let current: String = conn
        .query_row(
            &format!("SELECT {} FROM concerts WHERE id = ?1", column),
            params![id],
            |row| row.get(0),
        )
        .context("Failed to read error column")?;

    let mut errors: Vec<ErrorEntry> = serde_json::from_str(&current).unwrap_or_default();
    errors.push(ErrorEntry {
        error: error.to_string(),
        at: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    });
    let new_json = serde_json::to_string(&errors).context("Failed to serialize errors")?;

    conn.execute(
        &format!("UPDATE concerts SET {} = ?1 WHERE id = ?2", column),
        params![new_json, id],
    )
    .context("Failed to write error column")?;
    Ok(())
}
