use rusqlite::{params, Connection};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Listen,
    Import,
    Scraped,
    DownloadStarted,
    DownloadError,
    Downloaded,
    DownloadDelete,
    SplitStarted,
    Split,
    SplitError,
    SplitDelete,
    TrackDelete,
    TrackLiked,
    TrackLikedDelete,
    Wanted,
    WantedDelete,
    Ignored,
    IgnoredDelete,
    ArchiveStarted,
    Archived,
    ArchiveError,
    ArchiveDelete,
    Watch,
    /// User-supplied timestamps were stored; tracks re-cut with them.
    SplitTimestampsUser,
    /// User timestamps cleared; tracks reset to automated boundaries.
    SplitTimestampsReset,
    /// The original source file was deleted after full interlude coverage was
    /// confirmed (distinct from DownloadDelete for a clean audit trail).
    SourceRedundantDelete,
    /// An interlude track file was deleted. Distinct from TrackDelete so that
    /// `tracks_from_events` (which parses `track_index` from every `track_delete`
    /// row) is not corrupted by interlude deletions.
    InterludeDelete,
}

impl Event {
    /// Every variant, for validating an event name supplied by a caller
    /// (Test Control's `assert_concert_events`) against the real vocabulary
    /// instead of accepting an arbitrary typo'd string that would then
    /// vacuously never match.
    pub const ALL: [Event; 27] = [
        Event::Listen,
        Event::Import,
        Event::Scraped,
        Event::DownloadStarted,
        Event::DownloadError,
        Event::Downloaded,
        Event::DownloadDelete,
        Event::SplitStarted,
        Event::Split,
        Event::SplitError,
        Event::SplitDelete,
        Event::TrackDelete,
        Event::TrackLiked,
        Event::TrackLikedDelete,
        Event::Wanted,
        Event::WantedDelete,
        Event::Ignored,
        Event::IgnoredDelete,
        Event::ArchiveStarted,
        Event::Archived,
        Event::ArchiveError,
        Event::ArchiveDelete,
        Event::Watch,
        Event::SplitTimestampsUser,
        Event::SplitTimestampsReset,
        Event::SourceRedundantDelete,
        Event::InterludeDelete,
    ];

    /// Parse an event name (the same string `as_str` produces) back into an
    /// `Event`, or `None` if it doesn't match any known variant.
    pub fn parse(name: &str) -> Option<Event> {
        Self::ALL.into_iter().find(|e| e.as_str() == name)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Event::Listen => "listen",
            Event::Import => "import",
            Event::Scraped => "scraped",
            Event::DownloadStarted => "download_started",
            Event::DownloadError => "download_error",
            Event::Downloaded => "downloaded",
            Event::DownloadDelete => "download_delete",
            Event::SplitStarted => "split_started",
            Event::Split => "split",
            Event::SplitError => "split_error",
            Event::SplitDelete => "split_delete",
            Event::TrackDelete => "track_delete",
            Event::TrackLiked => "track_liked",
            Event::TrackLikedDelete => "track_liked_delete",
            Event::Wanted => "wanted",
            Event::WantedDelete => "wanted_delete",
            Event::Ignored => "ignored",
            Event::IgnoredDelete => "ignored_delete",
            Event::ArchiveStarted => "archive_started",
            Event::Archived => "archived",
            Event::ArchiveError => "archive_error",
            Event::ArchiveDelete => "archive_delete",
            Event::Watch => "watch",
            Event::SplitTimestampsUser => "split_timestamps_user",
            Event::SplitTimestampsReset => "split_timestamps_reset",
            Event::SourceRedundantDelete => "source_redundant_delete",
            Event::InterludeDelete => "interlude_delete",
        }
    }
}

#[derive(Debug, Clone)]
pub struct EventRow {
    pub event: String,
    pub at: String,
    pub json: Option<String>,
}

const LIST_FOR_CONCERT_SQL: &str =
    "SELECT event, at, json FROM events WHERE concert_id = ?1 ORDER BY at ASC, id ASC";

fn event_row_from_sql(row: &rusqlite::Row) -> rusqlite::Result<EventRow> {
    Ok(EventRow {
        event: row.get(0)?,
        at: row.get(1)?,
        json: row.get(2)?,
    })
}

/// Fallible variant of [`list_for_concert`], for callers that must treat a
/// query failure as a genuine error rather than an empty result — e.g.
/// `test_control::assert_concert_events`, where silently reading "no
/// events" on a failed query would let an `absent` expectation vacuously
/// pass. Propagates a prepare/query failure *or any single row's decode
/// failure* as `Err` — deliberately stricter than [`list_for_concert`],
/// which keeps whatever rows it could decode.
pub fn try_list_for_concert(conn: &Connection, concert_id: i64) -> rusqlite::Result<Vec<EventRow>> {
    let mut stmt = conn.prepare(LIST_FOR_CONCERT_SQL)?;
    let rows = stmt.query_map(params![concert_id], event_row_from_sql)?;
    rows.collect()
}

/// Best-effort variant for production rendering paths, where an event-log
/// hiccup should degrade gracefully rather than break the page: a
/// prepare/query failure returns an empty list (logged), and a single row
/// that fails to decode is dropped rather than discarding every other row
/// that decoded fine. Callers that need a query failure to surface as a
/// real error (not be indistinguishable from "no events") should call
/// [`try_list_for_concert`] instead.
pub fn list_for_concert(conn: &Connection, concert_id: i64) -> Vec<EventRow> {
    let mut stmt = match conn.prepare(LIST_FOR_CONCERT_SQL) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("failed to list events for concert {}: {}", concert_id, e);
            return Vec::new();
        }
    };
    stmt.query_map(params![concert_id], event_row_from_sql)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// Fallible variant of [`record`]. Callers that need an insert failure to
/// roll back an enclosing transaction (e.g. a Job Run terminal commit, where
/// "lifecycle state + event + Failed Job commit atomically" is a correctness
/// requirement, not a nice-to-have) must use this instead of the best-effort
/// `record`/`record_now`, which log and swallow the error.
pub fn try_record(
    conn: &Connection,
    concert_id: i64,
    event: Event,
    at: &str,
    json: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO events (concert_id, event, at, json) VALUES (?1, ?2, ?3, ?4)",
        params![concert_id, event.as_str(), at, json],
    )?;
    Ok(())
}

pub fn try_record_now(
    conn: &Connection,
    concert_id: i64,
    event: Event,
    json: Option<&str>,
) -> rusqlite::Result<()> {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    try_record(conn, concert_id, event, &now, json)
}

pub fn record(conn: &Connection, concert_id: i64, event: Event, at: &str, json: Option<&str>) {
    if let Err(e) = try_record(conn, concert_id, event, at, json) {
        tracing::warn!(
            "failed to record event {:?} for concert {}: {}",
            event,
            concert_id,
            e
        );
    }
}

pub fn record_now(conn: &Connection, concert_id: i64, event: Event, json: Option<&str>) {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    record(conn, concert_id, event, &now, json);
}

/// Generate historical events from existing concert data. Idempotent: skips
/// concerts that already have events.
pub fn backfill(conn: &Connection) -> anyhow::Result<usize> {
    let concerts = crate::db::concerts::list_concerts(conn)?;

    let mut stmt = conn.prepare("SELECT DISTINCT concert_id FROM events")?;
    let existing: std::collections::HashSet<i64> = stmt
        .query_map([], |row| row.get::<_, i64>(0))?
        .filter_map(|r| r.ok())
        .collect();

    let mut count = 0;
    for c in &concerts {
        if existing.contains(&c.id) {
            continue;
        }

        record(conn, c.id, Event::Import, &c.inserted_at, None);
        count += 1;

        if let Some(ref at) = c.metadata_scraped_at {
            record(conn, c.id, Event::Scraped, at, None);
            count += 1;
        }

        if let Some(ref at) = c.downloaded_at {
            record(conn, c.id, Event::Downloaded, at, None);
            count += 1;
        } else if let Some(ref at) = c.download_started_at {
            record(conn, c.id, Event::DownloadStarted, at, None);
            count += 1;
        }

        for err_entry in &c.download_errors {
            let json = serde_json::json!({"error": &err_entry.error}).to_string();
            record(conn, c.id, Event::DownloadError, &err_entry.at, Some(&json));
            count += 1;
        }

        if let Some(ref at) = c.split_at {
            let json = if c.set_list.is_empty() {
                None
            } else {
                Some(
                    serde_json::json!({
                        "track_count": c.set_list.len(),
                        "tracks": &c.set_list,
                    })
                    .to_string(),
                )
            };
            record(conn, c.id, Event::Split, at, json.as_deref());
            count += 1;
        } else if let Some(ref at) = c.split_started_at {
            record(conn, c.id, Event::SplitStarted, at, None);
            count += 1;
        }

        for err_entry in &c.split_errors {
            let json = serde_json::json!({"error": &err_entry.error}).to_string();
            record(conn, c.id, Event::SplitError, &err_entry.at, Some(&json));
            count += 1;
        }

        if c.wanted {
            record(conn, c.id, Event::Wanted, &c.inserted_at, None);
            count += 1;
        }
        if c.ignored {
            record(conn, c.id, Event::Ignored, &c.inserted_at, None);
            count += 1;
        }
    }

    tracing::info!(
        "backfill: generated {} events for {} concerts",
        count,
        concerts.len() - existing.len()
    );
    Ok(count)
}

/// Backfill track_delete events by comparing set_list against files on disk.
/// A split concert with a missing track file implies that track was deleted.
pub fn backfill_track_deletes(
    conn: &Connection,
    working_dir: &std::path::Path,
) -> anyhow::Result<usize> {
    let concerts = crate::db::concerts::list_concerts(conn)?;
    let mut count = 0;

    for c in &concerts {
        let album = match (c.split_at.as_ref(), c.album.as_deref()) {
            (Some(_), Some(a)) if !c.set_list.is_empty() => a,
            _ => continue,
        };

        let tracks = crate::concert_media::list_all_tracks(working_dir, album, &c.set_list);
        for t in &tracks {
            if t.available {
                continue;
            }
            let already_exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) > 0 FROM events
                     WHERE concert_id = ?1 AND event = 'track_delete'
                     AND json LIKE ?2",
                    params![c.id, format!("%\"track_index\":{}%", t.index)],
                    |row| row.get(0),
                )
                .unwrap_or(false);

            if !already_exists {
                let json = serde_json::json!({"track_index": t.index, "track_title": &t.title})
                    .to_string();
                record_now(conn, c.id, Event::TrackDelete, Some(&json));
                tracing::info!(
                    "backfill_track_deletes: concert {} track {}: {}",
                    c.id,
                    t.index,
                    t.title
                );
                count += 1;
            }
        }
    }

    tracing::info!("backfill_track_deletes: generated {} events", count);
    Ok(count)
}

/// Backfill split events that are missing track info in their JSON payload.
/// Updates existing rows in place rather than creating new events.
pub fn backfill_split_tracks(conn: &Connection) -> anyhow::Result<usize> {
    let mut event_stmt =
        conn.prepare("SELECT id, concert_id FROM events WHERE event = 'split' AND json IS NULL")?;
    let rows: Vec<(i64, i64)> = event_stmt
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    let mut count = 0;
    for (event_id, concert_id) in &rows {
        let concert = match crate::db::concerts::get_concert(conn, *concert_id) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if concert.set_list.is_empty() {
            continue;
        }
        let json = serde_json::json!({
            "track_count": concert.set_list.len(),
            "tracks": &concert.set_list,
        })
        .to_string();
        conn.execute(
            "UPDATE events SET json = ?1 WHERE id = ?2",
            rusqlite::params![json, event_id],
        )?;
        tracing::debug!(
            "backfill_split_tracks: event {} concert {} -> {} tracks",
            event_id,
            concert_id,
            concert.set_list.len()
        );
        count += 1;
    }

    tracing::info!("backfill_split_tracks: updated {} events", count);
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        crate::db::connection::open_in_memory().unwrap()
    }

    /// Identical fixture values to `db::tests::seed` — delegates there rather
    /// than duplicating the `upsert_listing` call.
    fn seed(conn: &Connection) -> i64 {
        crate::db::tests::seed(conn)
    }

    fn event_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap()
    }

    fn events_for(conn: &Connection, concert_id: i64) -> Vec<(String, String)> {
        let mut stmt = conn
            .prepare("SELECT event, at FROM events WHERE concert_id = ?1 ORDER BY id")
            .unwrap();
        stmt.query_map(params![concert_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    fn event_json_for(conn: &Connection, concert_id: i64, event: &str) -> Option<String> {
        conn.query_row(
            "SELECT json FROM events WHERE concert_id = ?1 AND event = ?2 ORDER BY id DESC LIMIT 1",
            params![concert_id, event],
            |row| row.get(0),
        )
        .ok()
    }

    #[test]
    fn all_events_round_trip_through_as_str_and_parse() {
        for event in Event::ALL {
            let name = event.as_str();
            assert_eq!(
                Event::parse(name),
                Some(event),
                "Event::ALL is missing (or has a stale entry for) {name:?}"
            );
        }
    }

    #[test]
    fn parse_rejects_unknown_name() {
        assert_eq!(Event::parse("not_a_real_event"), None);
    }

    #[test]
    fn try_list_for_concert_returns_recorded_events() {
        let conn = setup();
        let id = seed(&conn);
        conn.execute("DELETE FROM events", []).unwrap();
        record_now(&conn, id, Event::Downloaded, None);

        let events = try_list_for_concert(&conn, id).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "downloaded");
    }

    #[test]
    fn try_list_for_concert_propagates_a_query_failure_instead_of_swallowing_it() {
        let conn = Connection::open_in_memory().unwrap();
        // no migrations run — no events table, so the query itself fails.
        let err = try_list_for_concert(&conn, 1).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("no such table"));
    }

    #[test]
    fn list_for_concert_swallows_a_query_failure_into_an_empty_list() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(list_for_concert(&conn, 1).len(), 0);
    }

    /// A BLOB stored in `json` fails to decode as `Option<String>` (rusqlite's
    /// `FromSql` for `String` only accepts a Text value) — a stand-in for a
    /// single malformed row among otherwise well-formed ones.
    fn insert_row_with_undecodable_json(conn: &Connection, concert_id: i64) {
        conn.execute(
            "INSERT INTO events (concert_id, event, at, json) VALUES (?1, 'listen', '2024-01-01T00:00:00Z', ?2)",
            params![concert_id, vec![0xFFu8, 0xFE, 0x00]],
        )
        .unwrap();
    }

    #[test]
    fn list_for_concert_keeps_well_formed_rows_when_one_row_is_malformed() {
        let conn = setup();
        let id = seed(&conn);
        conn.execute("DELETE FROM events", []).unwrap();
        record_now(&conn, id, Event::Downloaded, None);
        insert_row_with_undecodable_json(&conn, id);

        let events = list_for_concert(&conn, id);
        assert_eq!(
            events.len(),
            1,
            "the one malformed row must be dropped, not the whole list"
        );
        assert_eq!(events[0].event, "downloaded");
    }

    #[test]
    fn try_list_for_concert_fails_the_whole_call_when_one_row_is_malformed() {
        let conn = setup();
        let id = seed(&conn);
        conn.execute("DELETE FROM events", []).unwrap();
        record_now(&conn, id, Event::Downloaded, None);
        insert_row_with_undecodable_json(&conn, id);

        try_list_for_concert(&conn, id)
            .expect_err("a single malformed row must fail the whole call, not just be dropped");
    }

    #[test]
    fn record_inserts_correct_row() {
        let conn = setup();
        let id = seed(&conn);
        // seed already creates an import event; clear to test record directly
        conn.execute("DELETE FROM events", []).unwrap();

        record(&conn, id, Event::Downloaded, "2024-06-01T12:00:00Z", None);

        let events = events_for(&conn, id);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "downloaded");
        assert_eq!(events[0].1, "2024-06-01T12:00:00Z");
    }

    #[test]
    fn record_with_json() {
        let conn = setup();
        let id = seed(&conn);
        conn.execute("DELETE FROM events", []).unwrap();

        let json = r#"{"error":"timeout"}"#;
        record(
            &conn,
            id,
            Event::DownloadError,
            "2024-06-01T12:00:00Z",
            Some(json),
        );

        let stored: String = conn
            .query_row(
                "SELECT json FROM events WHERE concert_id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, json);
    }

    #[test]
    fn record_now_sets_timestamp() {
        let conn = setup();
        let id = seed(&conn);
        conn.execute("DELETE FROM events", []).unwrap();

        record_now(&conn, id, Event::Listen, None);

        let events = events_for(&conn, id);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "listen");
        assert!(!events[0].1.is_empty());
    }

    #[test]
    fn record_swallows_errors_when_no_table() {
        let conn = Connection::open_in_memory().unwrap();
        // no migrations run — no events table
        record(&conn, 1, Event::Listen, "2024-01-01T00:00:00Z", None);
        // should not panic
    }

    #[test]
    fn inserted_at_is_set_automatically() {
        let conn = setup();
        let id = seed(&conn);

        let inserted_at: String = conn
            .query_row(
                "SELECT inserted_at FROM events WHERE concert_id = ?1 LIMIT 1",
                params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!inserted_at.is_empty());
    }

    #[test]
    fn import_event_on_new_concert() {
        let conn = setup();
        let id = seed(&conn);
        let events = events_for(&conn, id);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "import");
    }

    #[test]
    fn no_import_event_on_upsert_update() {
        let conn = setup();
        let _id = seed(&conn);
        let count_before = event_count(&conn);

        crate::db::concerts::upsert_listing(
            &conn,
            &crate::db::concerts::NewListing {
                source_url: "https://npr.org/c/1".to_string(),
                title: "Updated Title".to_string(),
                concert_date: None,
                teaser: None,
            },
        )
        .unwrap();

        assert_eq!(event_count(&conn), count_before);
    }

    #[test]
    fn scraped_event_on_update_metadata() {
        let conn = setup();
        let id = seed(&conn);
        crate::db::concerts::update_metadata(
            &conn,
            id,
            &crate::db::concerts::MetadataUpdate {
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                description: None,
                set_list: vec![],
                musicians: vec![],
            },
        )
        .unwrap();

        let events = events_for(&conn, id);
        assert!(events.iter().any(|(e, _)| e == "scraped"));
    }

    #[test]
    fn toggle_ignored_emits_correct_events() {
        let conn = setup();
        let id = seed(&conn);
        conn.execute("DELETE FROM events", []).unwrap();

        let is_ignored = crate::db::concerts::toggle_ignored(&conn, id).unwrap();
        assert!(is_ignored);
        let events = events_for(&conn, id);
        assert_eq!(events.last().unwrap().0, "ignored");

        let is_ignored = crate::db::concerts::toggle_ignored(&conn, id).unwrap();
        assert!(!is_ignored);
        let events = events_for(&conn, id);
        assert_eq!(events.last().unwrap().0, "ignored_delete");
    }

    #[test]
    fn toggle_wanted_emits_correct_events() {
        let conn = setup();
        let id = seed(&conn);
        conn.execute("DELETE FROM events", []).unwrap();

        let is_wanted = crate::db::concerts::toggle_wanted(&conn, id).unwrap();
        assert!(is_wanted);
        let events = events_for(&conn, id);
        assert_eq!(events.last().unwrap().0, "wanted");

        let is_wanted = crate::db::concerts::toggle_wanted(&conn, id).unwrap();
        assert!(!is_wanted);
        let events = events_for(&conn, id);
        assert_eq!(events.last().unwrap().0, "wanted_delete");
    }

    #[test]
    fn download_lifecycle_events() {
        let conn = setup();
        let id = seed(&conn);
        conn.execute("DELETE FROM events", []).unwrap();

        crate::db::lifecycle::try_mark_download_started(&conn, id).unwrap();
        crate::db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();

        let events = events_for(&conn, id);
        let event_types: Vec<&str> = events.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(event_types, vec!["download_started", "downloaded"]);
    }

    #[test]
    fn download_error_event_has_json() {
        let conn = setup();
        let id = seed(&conn);
        conn.execute("DELETE FROM events", []).unwrap();

        crate::db::lifecycle::try_mark_download_started(&conn, id).unwrap();
        crate::db::lifecycle::mark_download_failed(&conn, id, "timeout").unwrap();

        let json = event_json_for(&conn, id, "download_error").unwrap();
        assert!(json.contains("timeout"));
    }

    #[test]
    fn split_lifecycle_events() {
        let conn = setup();
        let id = seed(&conn);
        conn.execute("DELETE FROM events", []).unwrap();

        crate::db::lifecycle::try_mark_download_started(&conn, id).unwrap();
        crate::db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();
        crate::db::lifecycle::try_mark_split_started(&conn, id).unwrap();
        crate::db::lifecycle::mark_split_succeeded(&conn, id).unwrap();

        let events = events_for(&conn, id);
        let event_types: Vec<&str> = events.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(
            event_types,
            vec!["download_started", "downloaded", "split_started", "split"]
        );
    }

    #[test]
    fn split_error_event_has_json() {
        let conn = setup();
        let id = seed(&conn);
        conn.execute("DELETE FROM events", []).unwrap();

        crate::db::lifecycle::try_mark_download_started(&conn, id).unwrap();
        crate::db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();
        crate::db::lifecycle::try_mark_split_started(&conn, id).unwrap();
        crate::db::lifecycle::mark_split_failed(&conn, id, "ffmpeg error").unwrap();

        let json = event_json_for(&conn, id, "split_error").unwrap();
        assert!(json.contains("ffmpeg error"));
    }

    #[test]
    fn clear_download_state_emits_download_delete() {
        let conn = setup();
        let id = seed(&conn);
        conn.execute("DELETE FROM events", []).unwrap();

        crate::db::lifecycle::try_mark_download_started(&conn, id).unwrap();
        crate::db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();
        crate::db::lifecycle::clear_download_state(&conn, id).unwrap();

        let events = events_for(&conn, id);
        assert_eq!(events.last().unwrap().0, "download_delete");
    }

    #[test]
    fn clear_split_state_emits_split_delete() {
        let conn = setup();
        let id = seed(&conn);
        conn.execute("DELETE FROM events", []).unwrap();

        crate::db::lifecycle::try_mark_download_started(&conn, id).unwrap();
        crate::db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();
        crate::db::lifecycle::try_mark_split_started(&conn, id).unwrap();
        crate::db::lifecycle::mark_split_succeeded(&conn, id).unwrap();
        crate::db::lifecycle::clear_split_state(&conn, id).unwrap();

        let events = events_for(&conn, id);
        assert_eq!(events.last().unwrap().0, "split_delete");
    }

    #[test]
    fn backfill_generates_events_for_existing_concert() {
        let conn = setup();
        let id = seed(&conn);
        // Simulate a concert that was imported and downloaded before events existed
        conn.execute("DELETE FROM events", []).unwrap();
        crate::db::lifecycle::try_mark_download_started(&conn, id).unwrap();
        crate::db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();
        conn.execute("DELETE FROM events", []).unwrap();
        assert_eq!(event_count(&conn), 0);

        let count = backfill(&conn).unwrap();
        assert!(count >= 2); // at least import + downloaded

        let events = events_for(&conn, id);
        let event_types: Vec<&str> = events.iter().map(|(e, _)| e.as_str()).collect();
        assert!(event_types.contains(&"import"));
        assert!(event_types.contains(&"downloaded"));
    }

    #[test]
    fn backfill_is_idempotent() {
        let conn = setup();
        let id = seed(&conn);
        conn.execute("DELETE FROM events", []).unwrap();
        crate::db::concerts::toggle_wanted(&conn, id).unwrap();
        conn.execute("DELETE FROM events", []).unwrap();

        let count1 = backfill(&conn).unwrap();
        let count2 = backfill(&conn).unwrap();
        assert!(count1 > 0);
        assert_eq!(count2, 0);
        assert_eq!(event_count(&conn), count1 as i64);
    }

    #[test]
    fn backfill_skips_concerts_with_existing_events() {
        let conn = setup();
        let _id1 = seed(&conn); // has import event from seed
        crate::db::concerts::upsert_listing(
            &conn,
            &crate::db::concerts::NewListing {
                source_url: "https://npr.org/c/2".to_string(),
                title: "Concert B".to_string(),
                concert_date: None,
                teaser: None,
            },
        )
        .unwrap();

        // Both concerts already have import events from upsert_listing
        let count = backfill(&conn).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn backfill_generates_error_events_with_json() {
        let conn = setup();
        let id = seed(&conn);
        conn.execute("DELETE FROM events", []).unwrap();

        // Add a download error directly to the concert
        crate::db::lifecycle::try_mark_download_started(&conn, id).unwrap();
        crate::db::lifecycle::mark_download_failed(&conn, id, "403 forbidden").unwrap();
        conn.execute("DELETE FROM events", []).unwrap();

        backfill(&conn).unwrap();

        let json = event_json_for(&conn, id, "download_error").unwrap();
        assert!(json.contains("403 forbidden"));
    }

    #[test]
    fn backfill_wanted_and_ignored() {
        let conn = setup();
        let id = seed(&conn);
        conn.execute("DELETE FROM events", []).unwrap();
        crate::db::concerts::toggle_wanted(&conn, id).unwrap();
        conn.execute("DELETE FROM events", []).unwrap();

        backfill(&conn).unwrap();

        let events = events_for(&conn, id);
        let event_types: Vec<&str> = events.iter().map(|(e, _)| e.as_str()).collect();
        assert!(event_types.contains(&"wanted"));
    }

    #[test]
    fn backfill_track_deletes_creates_events_for_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let conn = setup();
        let id = seed(&conn);

        crate::db::concerts::update_metadata(
            &conn,
            id,
            &crate::db::concerts::MetadataUpdate {
                artist: "Artist".to_string(),
                album: "Test Album".to_string(),
                description: None,
                set_list: vec![
                    "Song One".to_string(),
                    "Song Two".to_string(),
                    "Song Three".to_string(),
                ],
                musicians: vec![],
            },
        )
        .unwrap();
        crate::db::lifecycle::try_mark_download_started(&conn, id).unwrap();
        crate::db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();
        crate::db::lifecycle::try_mark_split_started(&conn, id).unwrap();
        crate::db::lifecycle::mark_split_succeeded(&conn, id).unwrap();

        // Create only Song One on disk — Song Two and Song Three are "deleted"
        let concert_dir = crate::model::concert_dir(dir.path(), "Test Album");
        std::fs::create_dir_all(&concert_dir).unwrap();
        std::fs::File::create(concert_dir.join("Song One.m4a")).unwrap();

        conn.execute("DELETE FROM events", []).unwrap();

        let count = backfill_track_deletes(&conn, dir.path()).unwrap();
        assert_eq!(count, 2);

        let events = events_for(&conn, id);
        let delete_events: Vec<&(String, String)> =
            events.iter().filter(|(e, _)| e == "track_delete").collect();
        assert_eq!(delete_events.len(), 2);

        // Idempotent
        let count2 = backfill_track_deletes(&conn, dir.path()).unwrap();
        assert_eq!(count2, 0);
    }

    #[test]
    fn split_event_includes_track_json() {
        let conn = setup();
        let id = seed(&conn);

        crate::db::concerts::update_metadata(
            &conn,
            id,
            &crate::db::concerts::MetadataUpdate {
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                description: None,
                set_list: vec!["Song A".to_string(), "Song B".to_string()],
                musicians: vec![],
            },
        )
        .unwrap();
        conn.execute("DELETE FROM events", []).unwrap();

        crate::db::lifecycle::try_mark_download_started(&conn, id).unwrap();
        crate::db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();
        crate::db::lifecycle::try_mark_split_started(&conn, id).unwrap();
        crate::db::lifecycle::mark_split_succeeded(&conn, id).unwrap();

        let json = event_json_for(&conn, id, "split").unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["track_count"], 2);
        assert_eq!(v["tracks"][0], "Song A");
        assert_eq!(v["tracks"][1], "Song B");
    }

    #[test]
    fn split_event_no_json_when_empty_set_list() {
        let conn = setup();
        let id = seed(&conn);
        conn.execute("DELETE FROM events", []).unwrap();

        crate::db::lifecycle::try_mark_download_started(&conn, id).unwrap();
        crate::db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();
        crate::db::lifecycle::try_mark_split_started(&conn, id).unwrap();
        crate::db::lifecycle::mark_split_succeeded(&conn, id).unwrap();

        let json = event_json_for(&conn, id, "split");
        assert!(json.is_none());
    }

    #[test]
    fn backfill_split_tracks_updates_null_json() {
        let conn = setup();
        let id = seed(&conn);

        crate::db::concerts::update_metadata(
            &conn,
            id,
            &crate::db::concerts::MetadataUpdate {
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                description: None,
                set_list: vec![
                    "Track 1".to_string(),
                    "Track 2".to_string(),
                    "Track 3".to_string(),
                ],
                musicians: vec![],
            },
        )
        .unwrap();

        // Simulate a split event with no json (the old behavior)
        conn.execute("DELETE FROM events", []).unwrap();
        record(&conn, id, Event::Split, "2024-06-01T12:00:00Z", None);

        let count = backfill_split_tracks(&conn).unwrap();
        assert_eq!(count, 1);

        let json = event_json_for(&conn, id, "split").unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["track_count"], 3);
        assert_eq!(v["tracks"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn backfill_split_tracks_is_idempotent() {
        let conn = setup();
        let id = seed(&conn);

        crate::db::concerts::update_metadata(
            &conn,
            id,
            &crate::db::concerts::MetadataUpdate {
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                description: None,
                set_list: vec!["Track 1".to_string()],
                musicians: vec![],
            },
        )
        .unwrap();

        conn.execute("DELETE FROM events", []).unwrap();
        record(&conn, id, Event::Split, "2024-06-01T12:00:00Z", None);

        let count1 = backfill_split_tracks(&conn).unwrap();
        assert_eq!(count1, 1);

        let count2 = backfill_split_tracks(&conn).unwrap();
        assert_eq!(count2, 0);
    }

    #[test]
    fn backfill_split_tracks_skips_empty_set_list() {
        let conn = setup();
        let id = seed(&conn);

        conn.execute("DELETE FROM events", []).unwrap();
        record(&conn, id, Event::Split, "2024-06-01T12:00:00Z", None);

        let count = backfill_split_tracks(&conn).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn backfill_generates_split_event_with_tracks() {
        let conn = setup();
        let id = seed(&conn);
        conn.execute("DELETE FROM events", []).unwrap();

        crate::db::concerts::update_metadata(
            &conn,
            id,
            &crate::db::concerts::MetadataUpdate {
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                description: None,
                set_list: vec!["Song X".to_string(), "Song Y".to_string()],
                musicians: vec![],
            },
        )
        .unwrap();
        crate::db::lifecycle::try_mark_download_started(&conn, id).unwrap();
        crate::db::lifecycle::mark_download_succeeded(&conn, id, "mp4").unwrap();
        crate::db::lifecycle::try_mark_split_started(&conn, id).unwrap();
        crate::db::lifecycle::mark_split_succeeded(&conn, id).unwrap();
        conn.execute("DELETE FROM events", []).unwrap();

        backfill(&conn).unwrap();

        let json = event_json_for(&conn, id, "split").unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["track_count"], 2);
        assert_eq!(v["tracks"][0], "Song X");
    }
}
