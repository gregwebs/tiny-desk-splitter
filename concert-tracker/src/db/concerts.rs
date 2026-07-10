use anyhow::{Context, Result};
use rusqlite::{params, Connection, Row};

use crate::events::{self, Event};
use crate::model::{Concert, ErrorEntry, Musician};

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

pub(super) fn concert_from_row(row: &Row) -> rusqlite::Result<Concert> {
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
    let archive_errors_json: String = row.get("archive_errors_json")?;
    let tracks_present_json: Option<String> = row.get("tracks_present")?;
    let tracks_liked_json: Option<String> = row.get("tracks_liked")?;
    let download_errors: Vec<ErrorEntry> =
        serde_json::from_str(&download_errors_json).unwrap_or_default();
    let split_errors: Vec<ErrorEntry> =
        serde_json::from_str(&split_errors_json).unwrap_or_default();
    let archive_errors: Vec<ErrorEntry> =
        serde_json::from_str(&archive_errors_json).unwrap_or_default();

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
        downloaded_extension: row.get("downloaded_extension")?,
        download_errors,
        split_started_at: row.get("split_started_at")?,
        split_at: row.get("split_at")?,
        split_errors,
        archive_started_at: row.get("archive_started_at")?,
        archived_at: row.get("archived_at")?,
        archive_errors,
        inserted_at: row.get("inserted_at")?,
        updated_at: row.get("updated_at")?,
        metadata_scraped_at: row.get("metadata_scraped_at")?,
        tracks_present: tracks_present_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        tracks_liked: tracks_liked_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        media_duration: row.get("media_duration")?,
    })
}

pub fn upsert_listing(conn: &Connection, listing: &NewListing) -> Result<()> {
    let is_new = conn
        .query_row(
            "SELECT COUNT(*) FROM concerts WHERE source_url = ?1",
            params![listing.source_url],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        == 0;

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

    if is_new {
        let id = conn.last_insert_rowid();
        events::record_now(conn, id, Event::Import, None);
    }

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
    events::record_now(conn, id, Event::Scraped, None);
    Ok(())
}

pub fn toggle_ignored(conn: &Connection, id: i64) -> Result<bool> {
    conn.execute(
        "UPDATE concerts SET
             ignored = CASE WHEN ignored = 0 THEN 1 ELSE 0 END,
             wanted  = CASE WHEN ignored = 0 THEN 0 ELSE wanted END
         WHERE id = ?1",
        params![id],
    )
    .context("Failed to toggle ignored")?;

    let new_ignored: bool = conn
        .query_row(
            "SELECT ignored FROM concerts WHERE id = ?1",
            params![id],
            |row| row.get::<_, i64>(0).map(|v| v != 0),
        )
        .context("Failed to read new ignored value")?;

    if new_ignored {
        events::record_now(conn, id, Event::Ignored, None);
    } else {
        events::record_now(conn, id, Event::IgnoredDelete, None);
    }

    Ok(new_ignored)
}

pub fn toggle_wanted(conn: &Connection, id: i64) -> Result<bool> {
    conn.execute(
        "UPDATE concerts SET
             wanted  = CASE WHEN wanted = 0 THEN 1 ELSE 0 END,
             ignored = CASE WHEN wanted = 0 THEN 0 ELSE ignored END
         WHERE id = ?1",
        params![id],
    )
    .context("Failed to toggle wanted")?;

    let new_wanted: bool = conn
        .query_row(
            "SELECT wanted FROM concerts WHERE id = ?1",
            params![id],
            |row| row.get::<_, i64>(0).map(|v| v != 0),
        )
        .context("Failed to read new wanted value")?;

    if new_wanted {
        events::record_now(conn, id, Event::Wanted, None);
    } else {
        events::record_now(conn, id, Event::WantedDelete, None);
    }

    Ok(new_wanted)
}

pub fn set_notes(conn: &Connection, id: i64, notes: &str) -> Result<()> {
    conn.execute(
        "UPDATE concerts SET notes = ?1 WHERE id = ?2",
        params![notes, id],
    )
    .context("Failed to set notes")?;
    Ok(())
}

pub fn list_concerts(conn: &Connection) -> Result<Vec<Concert>> {
    let mut stmt =
        conn.prepare("SELECT * FROM concerts ORDER BY concert_date DESC, inserted_at DESC")?;
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

/// Like `get_concert`, but `Ok(None)` for a genuinely-absent row while still
/// propagating real database errors. Callers that treat "concert gone" as a
/// normal, recoverable outcome (e.g. playlist expansion skipping a dangling
/// reference) must use this so a transient failure isn't silently swallowed.
pub fn get_concert_opt(conn: &Connection, id: i64) -> Result<Option<Concert>> {
    let mut stmt = conn.prepare("SELECT * FROM concerts WHERE id = ?1")?;
    let mut iter = stmt.query_map(params![id], concert_from_row)?;
    match iter.next() {
        Some(row) => Ok(Some(row.context("Failed to read concert")?)),
        None => Ok(None),
    }
}

pub fn list_concerts_missing_teaser(conn: &Connection) -> Result<Vec<Concert>> {
    let mut stmt = conn.prepare("SELECT * FROM concerts WHERE teaser IS NULL ORDER BY id")?;
    let concerts = stmt
        .query_map([], concert_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to list concerts missing teaser")?;
    Ok(concerts)
}

pub fn set_teaser(conn: &Connection, id: i64, teaser: &str) -> Result<()> {
    conn.execute(
        "UPDATE concerts SET teaser = ?1 WHERE id = ?2",
        params![teaser, id],
    )
    .context("Failed to set teaser")?;
    Ok(())
}

pub fn get_concert_by_url(conn: &Connection, url: &str) -> Result<Option<Concert>> {
    let mut stmt = conn.prepare("SELECT * FROM concerts WHERE source_url = ?1")?;
    let mut iter = stmt.query_map(params![url], concert_from_row)?;
    match iter.next() {
        Some(row) => Ok(Some(row.context("Failed to read concert")?)),
        None => Ok(None),
    }
}

pub fn get_concert_by_album(conn: &Connection, album: &str) -> Result<Option<Concert>> {
    let mut stmt = conn.prepare("SELECT * FROM concerts WHERE album = ?1")?;
    let mut iter = stmt.query_map(params![album], concert_from_row)?;
    match iter.next() {
        Some(row) => Ok(Some(row.context("Failed to read concert")?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::connection::open_in_memory;
    use crate::db::tests::{events_for, listing, seed, seed_with_album};

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

    #[test]
    fn list_concerts_missing_teaser_returns_rows_without_teaser() {
        let conn = open_in_memory().unwrap();
        upsert_listing(&conn, &listing("https://npr.org/c/1", "A")).unwrap();
        upsert_listing(
            &conn,
            &NewListing {
                source_url: "https://npr.org/c/2".to_string(),
                title: "B".to_string(),
                concert_date: None,
                teaser: None,
            },
        )
        .unwrap();
        let missing = list_concerts_missing_teaser(&conn).unwrap();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].title, "B");
    }

    #[test]
    fn set_teaser_updates_concert() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        set_teaser(&conn, id, "A great show").unwrap();
        let c = get_concert(&conn, id).unwrap();
        assert_eq!(c.teaser, Some("A great show".to_string()));
    }

    // ── Event characterization (#64) ────────────────────────────────────────

    #[test]
    fn upsert_listing_emits_import_only_on_insert() {
        let conn = open_in_memory().unwrap();
        upsert_listing(&conn, &listing("https://npr.org/c/1", "A")).unwrap();
        let id = get_concert_by_url(&conn, "https://npr.org/c/1")
            .unwrap()
            .unwrap()
            .id;
        assert_eq!(events_for(&conn, id), vec![("import".to_string(), None)]);

        // Conflict-update must not record a second import event.
        upsert_listing(&conn, &listing("https://npr.org/c/1", "B")).unwrap();
        assert_eq!(events_for(&conn, id), vec![("import".to_string(), None)]);
    }

    #[test]
    fn update_metadata_emits_scraped_event() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        update_metadata(
            &conn,
            id,
            &MetadataUpdate {
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                description: None,
                set_list: vec![],
                musicians: vec![],
            },
        )
        .unwrap();
        assert_eq!(
            events_for(&conn, id).last().unwrap(),
            &("scraped".to_string(), None)
        );
    }

    #[test]
    fn toggle_ignored_emits_ignored_then_ignored_delete() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        toggle_ignored(&conn, id).unwrap();
        assert_eq!(
            events_for(&conn, id).last().unwrap(),
            &("ignored".to_string(), None)
        );
        toggle_ignored(&conn, id).unwrap();
        assert_eq!(
            events_for(&conn, id).last().unwrap(),
            &("ignored_delete".to_string(), None)
        );
    }

    #[test]
    fn toggle_wanted_emits_wanted_then_wanted_delete() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        toggle_wanted(&conn, id).unwrap();
        assert_eq!(
            events_for(&conn, id).last().unwrap(),
            &("wanted".to_string(), None)
        );
        toggle_wanted(&conn, id).unwrap();
        assert_eq!(
            events_for(&conn, id).last().unwrap(),
            &("wanted_delete".to_string(), None)
        );
    }
}
