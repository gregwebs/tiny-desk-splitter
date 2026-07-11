use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

use crate::events;

const MIGRATION: &str = include_str!("../../migrations/0001_init.sql");
const MIGRATION_002: &str = include_str!("../../migrations/0002_archive.sql");
const MIGRATION_003: &str = include_str!("../../migrations/0003_audit_timestamps.sql");
const MIGRATION_004: &str = include_str!("../../migrations/0004_playlists.sql");

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
    // recursive_triggers stays OFF (its default) on purpose: the audit-timestamp
    // AFTER UPDATE triggers (migration 0003) run `UPDATE ... SET updated_at` in
    // their own body, which must not re-fire any trigger. Set explicitly so the
    // invariant is local to the connection setup rather than an implicit default.
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=OFF;",
    )
    .context("Failed to configure pragmas")?;
    run_migrations(conn)
}

fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(MIGRATION)
        .context("Failed to run migration 001")?;
    conn.execute_batch(MIGRATION_002)
        .context("Failed to run migration 002")?;
    // Rename the legacy first_seen_at column before anything below reads
    // inserted_at. No-op on fresh DBs (created with inserted_at) and on
    // already-migrated DBs.
    rename_column_if_exists(conn, "concerts", "first_seen_at", "inserted_at")?;
    add_column_if_missing(conn, "concerts", "updated_at", "TEXT")?;
    add_column_if_missing(conn, "jobs", "inserted_at", "TEXT")?;
    add_column_if_missing(conn, "jobs", "updated_at", "TEXT")?;
    add_column_if_missing(conn, "settings", "inserted_at", "TEXT")?;
    add_column_if_missing(conn, "settings", "updated_at", "TEXT")?;
    add_column_if_missing(conn, "concerts", "archive_started_at", "TEXT")?;
    add_column_if_missing(conn, "concerts", "archived_at", "TEXT")?;
    add_column_if_missing(
        conn,
        "concerts",
        "archive_errors_json",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    add_column_if_missing(conn, "concerts", "tracks_present", "TEXT")?;
    add_column_if_missing(conn, "concerts", "tracks_liked", "TEXT")?;
    add_column_if_missing(conn, "concerts", "downloaded_extension", "TEXT")?;
    add_column_if_missing(conn, "concerts", "auto_split_timestamps_json", "TEXT")?;
    add_column_if_missing(conn, "concerts", "user_split_timestamps_json", "TEXT")?;
    // Persisted source duration in seconds (from ffprobe at user-split time).
    // Survives source-file deletion so the coverage gate stays functional.
    add_column_if_missing(conn, "concerts", "media_duration", "REAL")?;
    add_column_if_missing(
        conn,
        "settings",
        "theme",
        "TEXT NOT NULL DEFAULT 'system' CHECK (theme IN ('system','light','dark'))",
    )?;
    conn.execute_batch(
        "UPDATE concerts SET downloaded_extension = 'mp4'
         WHERE downloaded_at IS NOT NULL AND downloaded_extension IS NULL",
    )
    .context("Failed to backfill downloaded_extension")?;
    events::backfill(conn).context("Failed to backfill events")?;
    // Backfill the audit timestamps from history BEFORE creating the triggers,
    // otherwise the backfill UPDATEs would fire the AFTER UPDATE triggers and
    // overwrite the historical values with now(). Idempotent on later startups
    // (the WHERE ... IS NULL guards match nothing once populated).
    backfill_audit_timestamps(conn).context("Failed to backfill audit timestamps")?;
    conn.execute_batch(MIGRATION_003)
        .context("Failed to run migration 003")?;
    conn.execute_batch(MIGRATION_004)
        .context("Failed to run migration 004")?;
    Ok(())
}

/// Populate the newly added audit-timestamp columns for pre-existing rows.
/// Concerts derive `updated_at` from their event history (the latest `at`),
/// falling back to `inserted_at` for concerts with no events. Jobs reuse their
/// `failed_at`. Settings have no history, so they fall back to now. Guarded by
/// `IS NULL` so re-running on an already-populated DB is a no-op.
///
/// The event log stores timestamps in two formats — `datetime('now')` space
/// format (`2026-06-09 20:33:05`, used by the column default and the backfilled
/// import/wanted/ignored events) and chrono ISO format (`2026-06-09T20:33:05Z`,
/// used by `events::record_now`). These are NOT lexicographically comparable
/// (the space byte 0x20 sorts before both digits and `T`), so a raw `MAX(at)`
/// can pick a chronologically earlier row. `datetime(at)` parses both forms and
/// re-emits the canonical space format, making `MAX` correct and leaving
/// `updated_at` in the same format the triggers write.
fn backfill_audit_timestamps(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE concerts SET updated_at =
             COALESCE(
                 (SELECT MAX(datetime(at)) FROM events e WHERE e.concert_id = concerts.id),
                 datetime(inserted_at)
             )
         WHERE updated_at IS NULL",
        [],
    )
    .context("Failed to backfill concerts.updated_at")?;
    conn.execute(
        "UPDATE jobs SET inserted_at = COALESCE(inserted_at, failed_at),
                         updated_at  = COALESCE(updated_at, failed_at)
         WHERE inserted_at IS NULL OR updated_at IS NULL",
        [],
    )
    .context("Failed to backfill jobs timestamps")?;
    conn.execute(
        "UPDATE settings SET inserted_at = COALESCE(inserted_at, datetime('now')),
                             updated_at  = COALESCE(updated_at, datetime('now'))
         WHERE inserted_at IS NULL OR updated_at IS NULL",
        [],
    )
    .context("Failed to backfill settings timestamps")?;
    Ok(())
}

/// Idempotently rename a column. Uses SQLite's `ALTER TABLE ... RENAME COLUMN`
/// (3.25+), but only when the old column still exists and the new one does not,
/// so it is safe to run on every startup and on fresh DBs that never had the
/// old column.
fn rename_column_if_exists(conn: &Connection, table: &str, old: &str, new: &str) -> Result<()> {
    let columns: Vec<String> = conn
        .prepare(&format!("PRAGMA table_info({})", table))?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<_>>()?;
    let has_old = columns.iter().any(|c| c == old);
    let has_new = columns.iter().any(|c| c == new);
    if has_old && !has_new {
        conn.execute_batch(&format!(
            "ALTER TABLE {} RENAME COLUMN {} TO {}",
            table, old, new
        ))
        .with_context(|| format!("Failed to rename {}.{} to {}", table, old, new))?;
    }
    Ok(())
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    col_type: &str,
) -> Result<()> {
    let has_column: bool = conn
        .prepare(&format!("PRAGMA table_info({})", table))?
        .query_map([], |row| row.get::<_, String>(1))?
        .any(|name| name.as_deref() == Ok(column));
    if !has_column {
        conn.execute_batch(&format!(
            "ALTER TABLE {} ADD COLUMN {} {}",
            table, column, col_type
        ))
        .with_context(|| format!("Failed to add column {}.{}", table, column))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::concerts::{get_concert, set_notes};
    use crate::db::lifecycle::{mark_download_succeeded, try_mark_download_started};
    use crate::db::tests::seed;
    use crate::events;
    use rusqlite::params;

    /// Read a single TEXT/NULL column for one concert.
    fn concert_ts(conn: &Connection, id: i64, column: &str) -> Option<String> {
        conn.query_row(
            &format!("SELECT {} FROM concerts WHERE id = ?1", column),
            params![id],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn backfill_sets_mp4_for_existing_downloads() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();
        // Simulate pre-migration state: clear the extension
        conn.execute(
            "UPDATE concerts SET downloaded_extension = NULL WHERE id = ?1",
            params![id],
        )
        .unwrap();
        // Re-run migrations to trigger backfill
        run_migrations(&conn).unwrap();
        let c = get_concert(&conn, id).unwrap();
        assert_eq!(c.downloaded_extension.as_deref(), Some("mp4"));
    }

    #[test]
    fn backfill_audit_timestamps_uses_latest_event() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        // Replace the auto-generated import event with a single event whose
        // timestamp is clearly distinct from inserted_at, then clear updated_at
        // to simulate a pre-migration row.
        conn.execute("DELETE FROM events WHERE concert_id = ?1", params![id])
            .unwrap();
        events::record(
            &conn,
            id,
            events::Event::Downloaded,
            "2030-05-06T07:08:09Z",
            None,
        );
        conn.execute(
            "UPDATE concerts SET updated_at = NULL WHERE id = ?1",
            params![id],
        )
        .unwrap();

        backfill_audit_timestamps(&conn).unwrap();

        // updated_at is the event's `at`, normalized to canonical space format
        // by datetime() (not the now-ish inserted_at). The WHEN guard on the
        // AFTER UPDATE trigger preserves this explicit value.
        assert_eq!(
            concert_ts(&conn, id, "updated_at"),
            Some("2030-05-06 07:08:09".to_string())
        );
        assert_ne!(
            concert_ts(&conn, id, "updated_at"),
            concert_ts(&conn, id, "inserted_at")
        );
    }

    #[test]
    fn backfill_audit_timestamps_handles_mixed_timestamp_formats() {
        // The event log mixes `datetime('now')` space format with chrono ISO
        // `...T...Z` format, which are not lexicographically comparable. Here the
        // truly-latest event is in space format but sorts BELOW an earlier
        // T-format event, so a raw string MAX(at) would pick the earlier one.
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        conn.execute("DELETE FROM events WHERE concert_id = ?1", params![id])
            .unwrap();
        // Same date; space (0x20) at index 10 sorts before 'T' (0x54), so the
        // 23:00 space value string-sorts below the 08:00 T value.
        events::record(
            &conn,
            id,
            events::Event::Downloaded,
            "2026-06-09 23:00:00",
            None,
        );
        events::record(
            &conn,
            id,
            events::Event::Split,
            "2026-06-09T08:00:00Z",
            None,
        );
        conn.execute(
            "UPDATE concerts SET updated_at = NULL WHERE id = ?1",
            params![id],
        )
        .unwrap();

        backfill_audit_timestamps(&conn).unwrap();

        // Must be the chronologically-latest event (23:00), normalized to the
        // canonical space format — not the earlier 08:00 T-format event.
        assert_eq!(
            concert_ts(&conn, id, "updated_at"),
            Some("2026-06-09 23:00:00".to_string())
        );
    }

    #[test]
    fn backfill_audit_timestamps_falls_back_to_inserted_at() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        // No events, updated_at cleared -> falls back to inserted_at.
        conn.execute("DELETE FROM events WHERE concert_id = ?1", params![id])
            .unwrap();
        conn.execute(
            "UPDATE concerts SET updated_at = NULL WHERE id = ?1",
            params![id],
        )
        .unwrap();
        backfill_audit_timestamps(&conn).unwrap();
        assert_eq!(
            concert_ts(&conn, id, "updated_at"),
            concert_ts(&conn, id, "inserted_at")
        );
    }

    #[test]
    fn backfill_audit_timestamps_is_idempotent() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        conn.execute(
            "UPDATE concerts SET updated_at = NULL WHERE id = ?1",
            params![id],
        )
        .unwrap();
        backfill_audit_timestamps(&conn).unwrap();
        let first = concert_ts(&conn, id, "updated_at");
        backfill_audit_timestamps(&conn).unwrap(); // must not touch populated rows
        assert_eq!(concert_ts(&conn, id, "updated_at"), first);
    }

    #[test]
    fn rename_column_if_exists_renames_and_preserves_value() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, first_seen_at TEXT);
             INSERT INTO t (id, first_seen_at) VALUES (1, '2024-01-01T00:00:00Z');",
        )
        .unwrap();

        rename_column_if_exists(&conn, "t", "first_seen_at", "inserted_at").unwrap();
        let value: String = conn
            .query_row("SELECT inserted_at FROM t WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(value, "2024-01-01T00:00:00Z");

        // Idempotent: a second run is a no-op (old column already gone).
        rename_column_if_exists(&conn, "t", "first_seen_at", "inserted_at").unwrap();
        let value2: String = conn
            .query_row("SELECT inserted_at FROM t WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(value2, "2024-01-01T00:00:00Z");
    }

    #[test]
    fn insert_sets_updated_at_via_trigger() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        assert!(
            concert_ts(&conn, id, "updated_at").is_some(),
            "AFTER INSERT trigger should populate updated_at"
        );
    }

    #[test]
    fn update_bumps_updated_at_via_trigger() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        // Pin updated_at to an old sentinel, then perform a normal update and
        // confirm the trigger advanced it (avoids 1-second now() flakiness).
        conn.execute(
            "UPDATE concerts SET updated_at = '2000-01-01T00:00:00Z' WHERE id = ?1",
            params![id],
        )
        .unwrap();
        set_notes(&conn, id, "changed").unwrap();
        let after = concert_ts(&conn, id, "updated_at").unwrap();
        assert_ne!(after, "2000-01-01T00:00:00Z", "updated_at must be bumped");
    }
}
