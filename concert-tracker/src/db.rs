use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::collections::HashSet;
use std::path::Path;

use crate::events::{self, Event};
use crate::model::{Concert, ErrorEntry, Musician, Playlist, PlaylistItem, PlaylistItemKind};

const MIGRATION: &str = include_str!("../migrations/0001_init.sql");
const MIGRATION_002: &str = include_str!("../migrations/0002_archive.sql");
const MIGRATION_003: &str = include_str!("../migrations/0003_audit_timestamps.sql");
const MIGRATION_004: &str = include_str!("../migrations/0004_playlists.sql");

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    System,
    Light,
    Dark,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    pub fn parse(s: &str) -> Result<Theme> {
        match s {
            "system" => Ok(Theme::System),
            "light" => Ok(Theme::Light),
            "dark" => Ok(Theme::Dark),
            other => Err(anyhow!("unknown theme: {other}")),
        }
    }

    /// True for an explicit user choice — used by templates to decide
    /// whether to render the `data-theme` attribute on `<html>`.
    /// `System` produces no attribute so `prefers-color-scheme` wins.
    pub fn is_explicit(self) -> bool {
        !matches!(self, Theme::System)
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub archive_location: Option<String>,
    pub theme: Theme,
}

pub fn get_settings(conn: &Connection) -> Result<Settings> {
    conn.query_row(
        "SELECT archive_location, theme FROM settings WHERE id = 1",
        [],
        |row| {
            let archive_location: Option<String> = row.get(0)?;
            let theme_str: String = row.get(1)?;
            Ok((archive_location, theme_str))
        },
    )
    .context("Failed to read settings")
    .map(|(archive_location, theme_str)| Settings {
        archive_location,
        theme: Theme::parse(&theme_str).unwrap_or(Theme::System),
    })
}

pub fn update_archive_location(conn: &Connection, location: &str) -> Result<()> {
    let value = if location.trim().is_empty() {
        None
    } else {
        Some(location.trim())
    };
    conn.execute(
        "UPDATE settings SET archive_location = ?1 WHERE id = 1",
        params![value],
    )
    .context("Failed to update archive location")?;
    Ok(())
}

pub fn update_theme(conn: &Connection, theme: Theme) -> Result<()> {
    tracing::debug!("update_theme: {}", theme.as_str());
    conn.execute(
        "UPDATE settings SET theme = ?1 WHERE id = 1",
        params![theme.as_str()],
    )
    .context("Failed to update theme")?;
    Ok(())
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

/// Returns false if download is already in progress (started_at IS NOT NULL).
pub fn try_mark_download_started(conn: &Connection, id: i64) -> Result<bool> {
    let rows = conn
        .execute(
            "UPDATE concerts SET download_started_at = datetime('now')
             WHERE id = ?1 AND download_started_at IS NULL",
            params![id],
        )
        .context("Failed to mark download started")?;
    if rows > 0 {
        events::record_now(conn, id, Event::DownloadStarted, None);
    }
    Ok(rows > 0)
}

pub fn mark_download_succeeded(conn: &Connection, id: i64, extension: &str) -> Result<()> {
    tracing::debug!(
        concert_id = id,
        ext = extension,
        "storing download extension"
    );
    conn.execute(
        "UPDATE concerts SET downloaded_at = datetime('now'), download_started_at = NULL,
         downloaded_extension = ?2
         WHERE id = ?1",
        params![id, extension],
    )
    .context("Failed to mark download succeeded")?;
    events::record_now(conn, id, Event::Downloaded, None);
    Ok(())
}

pub fn mark_download_failed(conn: &Connection, id: i64, error: &str) -> Result<()> {
    append_error(conn, id, "download_errors_json", error)?;
    conn.execute(
        "UPDATE concerts SET download_started_at = NULL WHERE id = ?1",
        params![id],
    )
    .context("Failed to clear download_started_at")?;
    let json = serde_json::json!({"error": error}).to_string();
    events::record_now(conn, id, Event::DownloadError, Some(&json));
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
    if rows > 0 {
        events::record_now(conn, id, Event::SplitStarted, None);
    }
    Ok(rows > 0)
}

pub fn mark_split_succeeded(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE concerts SET split_at = datetime('now'), split_started_at = NULL
         WHERE id = ?1",
        params![id],
    )
    .context("Failed to mark split succeeded")?;
    let json = split_tracks_json(conn, id);
    events::record_now(conn, id, Event::Split, json.as_deref());
    Ok(())
}

fn split_tracks_json(conn: &Connection, concert_id: i64) -> Option<String> {
    let concert = get_concert(conn, concert_id).ok()?;
    if concert.set_list.is_empty() {
        return None;
    }
    Some(
        serde_json::json!({
            "track_count": concert.set_list.len(),
            "tracks": concert.set_list,
        })
        .to_string(),
    )
}

pub fn mark_split_failed(conn: &Connection, id: i64, error: &str) -> Result<()> {
    append_error(conn, id, "split_errors_json", error)?;
    conn.execute(
        "UPDATE concerts SET split_started_at = NULL WHERE id = ?1",
        params![id],
    )
    .context("Failed to clear split_started_at")?;
    let json = serde_json::json!({"error": error}).to_string();
    events::record_now(conn, id, Event::SplitError, Some(&json));
    Ok(())
}

/// Clear download-related state, including download_errors_json. Without
/// resetting the error history, a prior failed attempt would resurrect the
/// download-error badge once downloaded_at is nulled (see
/// DownloadStatus::from_concert). The DownloadDelete event (recorded here) and
/// the failed_jobs table preserve the audit trail. Split state is intentionally
/// preserved — tracks may still exist on disk.
pub fn clear_download_state(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE concerts SET downloaded_at = NULL, download_started_at = NULL,
                downloaded_extension = NULL, download_errors_json = '[]'
         WHERE id = ?1",
        params![id],
    )
    .context("Failed to clear download state")?;
    events::record_now(conn, id, Event::DownloadDelete, None);
    Ok(())
}

/// One-time cleanup: reset download_errors_json for concerts whose latest
/// download_delete event is newer than their latest download_error event — the
/// download was deleted after the error, so the leftover error is stale (it
/// predates the fix that clears errors on delete) and is wrongly resurrecting
/// the download-error badge. Returns the number of concerts fixed.
///
/// Done as a direct UPDATE (not via clear_download_state) so it records no
/// spurious download_delete event and touches only download_errors_json. The
/// MAX(..) > MAX(..) comparison is NULL — and so does not match — when either
/// event type is absent, leaving concerts with no delete (or no error) alone.
/// Idempotent: a fixed row becomes '[]' and no longer matches.
pub fn clear_stale_download_errors(conn: &Connection) -> Result<usize> {
    let n = conn
        .execute(
            "UPDATE concerts SET download_errors_json = '[]'
             WHERE COALESCE(download_errors_json, '[]') != '[]'
               AND (SELECT MAX(at) FROM events e
                      WHERE e.concert_id = concerts.id AND e.event = 'download_delete')
                 > (SELECT MAX(at) FROM events e
                      WHERE e.concert_id = concerts.id AND e.event = 'download_error')",
            [],
        )
        .context("Failed to clear stale download errors")?;
    Ok(n)
}

/// Clear split-related state, including split_errors_json. Without resetting
/// the error history, a prior failed attempt would resurrect the split-error
/// badge once split_at is nulled (see SplitStatus::from_concert). The
/// SplitError events in the events table preserve the audit trail.
pub fn clear_split_state(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE concerts SET split_at = NULL, split_started_at = NULL,
                tracks_present = NULL, split_errors_json = '[]'
         WHERE id = ?1",
        params![id],
    )
    .context("Failed to clear split state")?;
    events::record_now(conn, id, Event::SplitDelete, None);
    Ok(())
}

/// Stored automated and user-supplied split timestamps for a concert.
pub struct StoredSplitTimestamps {
    pub auto: Option<Vec<concert_types::SongTimestamp>>,
    pub user: Option<Vec<concert_types::SongTimestamp>>,
}

pub fn get_split_timestamps(conn: &Connection, id: i64) -> Result<StoredSplitTimestamps> {
    let (auto_json, user_json): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT auto_split_timestamps_json, user_split_timestamps_json
             FROM concerts WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .context("Failed to read split timestamps")?;

    let auto = auto_json
        .as_deref()
        .and_then(|j| serde_json::from_str(j).ok());
    let user = user_json
        .as_deref()
        .and_then(|j| serde_json::from_str(j).ok());
    Ok(StoredSplitTimestamps { auto, user })
}

pub fn set_auto_split_timestamps(
    conn: &Connection,
    id: i64,
    ts: &[concert_types::SongTimestamp],
) -> Result<()> {
    let json = serde_json::to_string(ts).context("Failed to serialize auto timestamps")?;
    conn.execute(
        "UPDATE concerts SET auto_split_timestamps_json = ?1 WHERE id = ?2",
        params![json, id],
    )
    .context("Failed to set auto_split_timestamps_json")?;
    Ok(())
}

pub fn set_user_split_timestamps(
    conn: &Connection,
    id: i64,
    ts: &[concert_types::SongTimestamp],
) -> Result<()> {
    let json = serde_json::to_string(ts).context("Failed to serialize user timestamps")?;
    conn.execute(
        "UPDATE concerts SET user_split_timestamps_json = ?1 WHERE id = ?2",
        params![json, id],
    )
    .context("Failed to set user_split_timestamps_json")?;
    events::record_now(conn, id, Event::SplitTimestampsUser, Some(&json));
    Ok(())
}

/// Clear the user-supplied timestamps (reset to automated boundaries).
/// Records a `SplitTimestampsReset` event only when the column was non-NULL.
pub fn clear_user_split_timestamps(conn: &Connection, id: i64) -> Result<()> {
    let was_set: bool = conn
        .query_row(
            "SELECT user_split_timestamps_json IS NOT NULL FROM concerts WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .context("Failed to check user_split_timestamps_json")?;
    conn.execute(
        "UPDATE concerts SET user_split_timestamps_json = NULL WHERE id = ?1",
        params![id],
    )
    .context("Failed to clear user_split_timestamps_json")?;
    if was_set {
        events::record_now(conn, id, Event::SplitTimestampsReset, None);
    }
    Ok(())
}

pub fn set_tracks_present(conn: &Connection, id: i64, tracks: &[bool]) -> Result<()> {
    let json = serde_json::to_string(tracks).unwrap();
    conn.execute(
        "UPDATE concerts SET tracks_present = ?1 WHERE id = ?2",
        params![json, id],
    )
    .context("Failed to set tracks_present")?;
    Ok(())
}

pub fn set_tracks_liked(conn: &Connection, id: i64, tracks: &[bool]) -> Result<()> {
    let json = serde_json::to_string(tracks).unwrap();
    conn.execute(
        "UPDATE concerts SET tracks_liked = ?1 WHERE id = ?2",
        params![json, id],
    )
    .context("Failed to set tracks_liked")?;
    Ok(())
}

/// Flip the like bit for one track. Pads `tracks_liked` to `set_list.len()`
/// with `false` so previously-unsaved indices become writeable. Caller must
/// validate that `idx < set_list.len()`.
pub fn toggle_track_liked(conn: &Connection, id: i64, idx: usize) -> Result<bool> {
    let concert = get_concert(conn, id)?;
    if idx >= concert.set_list.len() {
        anyhow::bail!(
            "track index {} out of range for set_list of length {}",
            idx,
            concert.set_list.len()
        );
    }
    let mut liked = concert.tracks_liked.clone();
    if liked.len() < concert.set_list.len() {
        liked.resize(concert.set_list.len(), false);
    }
    liked[idx] = !liked[idx];
    let new_state = liked[idx];
    set_tracks_liked(conn, id, &liked)?;

    let title = &concert.set_list[idx];
    let json = serde_json::json!({"track_index": idx, "track_title": title}).to_string();
    let event = if new_state {
        Event::TrackLiked
    } else {
        Event::TrackLikedDelete
    };
    events::record_now(conn, id, event, Some(&json));

    Ok(new_state)
}

pub fn try_mark_archive_started(conn: &Connection, id: i64) -> Result<bool> {
    let rows = conn
        .execute(
            "UPDATE concerts SET archive_started_at = datetime('now')
             WHERE id = ?1 AND archive_started_at IS NULL AND archived_at IS NULL",
            params![id],
        )
        .context("Failed to mark archive started")?;
    if rows > 0 {
        events::record_now(conn, id, Event::ArchiveStarted, None);
    }
    Ok(rows > 0)
}

pub fn mark_archive_succeeded(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE concerts SET archived_at = datetime('now'), archive_started_at = NULL
         WHERE id = ?1",
        params![id],
    )
    .context("Failed to mark archive succeeded")?;
    events::record_now(conn, id, Event::Archived, None);
    Ok(())
}

pub fn mark_archive_failed(conn: &Connection, id: i64, error: &str) -> Result<()> {
    append_error(conn, id, "archive_errors_json", error)?;
    conn.execute(
        "UPDATE concerts SET archive_started_at = NULL WHERE id = ?1",
        params![id],
    )
    .context("Failed to clear archive_started_at")?;
    let json = serde_json::json!({"error": error}).to_string();
    events::record_now(conn, id, Event::ArchiveError, Some(&json));
    Ok(())
}

/// Clear archive state. Guards on `archived_at IS NOT NULL` so a stale
/// page can't stomp an in-flight archive (which has `archive_started_at`
/// set but `archived_at` still NULL). Resets `archive_errors_json` for the
/// same reason `clear_split_state` resets `split_errors_json`: otherwise
/// a prior failed archive would resurrect the `archive-error` badge once
/// `archived_at` is nulled (see `ArchiveStatus::from_concert`). Returns
/// true iff a row was actually cleared.
pub fn clear_archive_state(conn: &Connection, id: i64) -> Result<bool> {
    let rows = conn
        .execute(
            "UPDATE concerts SET archived_at = NULL, archive_started_at = NULL,
                    archive_errors_json = '[]'
             WHERE id = ?1 AND archived_at IS NOT NULL",
            params![id],
        )
        .context("Failed to clear archive state")?;
    if rows > 0 {
        events::record_now(conn, id, Event::ArchiveDelete, None);
    }
    Ok(rows > 0)
}

/// Current UTC time in the `datetime('now')` space format
/// (`2026-06-09 20:33:05`) that the concerts-table timestamp columns use.
/// Prefer this over ad-hoc chrono formatting: the codebase already suffers a
/// two-format hazard (see `backfill_audit_timestamps`), so Rust-side writers
/// of concerts columns should match the SQL default format.
pub fn now_string() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
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

pub fn set_downloaded_extension_if_missing(conn: &Connection, id: i64, ext: &str) -> Result<()> {
    tracing::debug!(
        concert_id = id,
        ext,
        "setting downloaded_extension if missing"
    );
    conn.execute(
        "UPDATE concerts SET downloaded_extension = ?1 WHERE id = ?2 AND downloaded_extension IS NULL",
        params![ext, id],
    )
    .context("Failed to set downloaded_extension")?;
    Ok(())
}

pub fn list_concerts_needing_tracks_backfill(conn: &Connection) -> Result<Vec<Concert>> {
    let mut stmt = conn
        .prepare("SELECT * FROM concerts WHERE split_at IS NOT NULL AND tracks_present IS NULL")
        .context("Failed to prepare tracks backfill query")?;
    let concerts = stmt
        .query_map([], concert_from_row)?
        .filter_map(|r| r.ok())
        .collect();
    Ok(concerts)
}

/// Concerts eligible for automated re-splitting: successfully split or
/// previously split-errored, with no user-edited timestamps, and not
/// currently mid-split. Includes concerts whose download may no longer be
/// present on disk (those will be reported as skipped at run time).
pub fn list_resplit_candidates(conn: &Connection) -> Result<Vec<Concert>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM concerts
         WHERE user_split_timestamps_json IS NULL
           AND split_started_at IS NULL
           AND (split_at IS NOT NULL OR COALESCE(split_errors_json, '[]') != '[]')
         ORDER BY id",
    )?;
    let concerts = stmt
        .query_map([], concert_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to list resplit candidates")?;
    Ok(concerts)
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
/// Returns `(download_count, split_count, archive_count)` of rows touched.
pub fn fail_in_progress_jobs(conn: &Connection, error: &str) -> Result<(usize, usize, usize)> {
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

    let ar_ids: Vec<i64> = conn
        .prepare("SELECT id FROM concerts WHERE archive_started_at IS NOT NULL")?
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<_>>()
        .context("Failed to read in-progress archives")?;
    for id in &ar_ids {
        mark_archive_failed(conn, *id, error)?;
    }

    Ok((dl_ids.len(), sp_ids.len(), ar_ids.len()))
}

/// Clear all stale in-progress flags (e.g. after an unclean shutdown).
pub fn reset_in_progress(conn: &Connection) -> Result<usize> {
    let rows = conn
        .execute(
            "UPDATE concerts SET download_started_at = NULL, split_started_at = NULL, archive_started_at = NULL
             WHERE download_started_at IS NOT NULL OR split_started_at IS NOT NULL OR archive_started_at IS NOT NULL",
            [],
        )
        .context("Failed to reset in-progress")?;
    Ok(rows)
}

pub fn list_in_progress(conn: &Connection) -> Result<Vec<Concert>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM concerts
         WHERE download_started_at IS NOT NULL OR split_started_at IS NOT NULL OR archive_started_at IS NOT NULL
         ORDER BY download_started_at, split_started_at, archive_started_at",
    )?;
    let concerts = stmt
        .query_map([], concert_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to list in-progress concerts")?;
    Ok(concerts)
}

pub fn count_active_jobs(conn: &Connection) -> Result<usize> {
    let count: usize = conn.query_row(
        "SELECT
           (SELECT COUNT(*) FROM concerts WHERE download_started_at IS NOT NULL AND downloaded_at IS NULL)
         + (SELECT COUNT(*) FROM concerts WHERE split_started_at IS NOT NULL AND split_at IS NULL)
         + (SELECT COUNT(*) FROM concerts WHERE archive_started_at IS NOT NULL AND archived_at IS NULL)",
        [],
        |row| row.get(0),
    )?;
    Ok(count)
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

pub fn mark_month_synced(conn: &Connection, year: i32, month: u32) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO synced_months (year, month) VALUES (?1, ?2)",
        params![year, month],
    )
    .context("Failed to mark month synced")?;
    Ok(())
}

pub fn list_synced_months(conn: &Connection) -> Result<Vec<(i32, u32)>> {
    let mut stmt = conn.prepare("SELECT year, month FROM synced_months ORDER BY year, month")?;
    let rows = stmt
        .query_map([], |row| Ok((row.get::<_, i32>(0)?, row.get::<_, u32>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to list synced months")?;
    Ok(rows)
}

pub fn earliest_concert_date(conn: &Connection) -> Result<Option<String>> {
    let result = conn
        .query_row(
            "SELECT MIN(concert_date) FROM concerts WHERE concert_date IS NOT NULL",
            [],
            |row| row.get::<_, Option<String>>(0),
        )
        .context("Failed to get earliest concert date")?;
    Ok(result)
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

// ── Playlists ────────────────────────────────────────────────────────────────

/// Outcome of a playlist mutation that can fail validation. Kept distinct from
/// the catch-all `anyhow::Result` so the web layer can map each case to the right
/// HTTP status: `NotFound` → 404, `Invalid` → 422, `Db` → 500.
#[derive(Debug)]
pub enum PlaylistError {
    /// The target playlist (or item) does not exist.
    NotFound,
    /// The request is well-formed but references something invalid: an empty
    /// name, a missing/out-of-range concert-track reference, a self/cyclic nest,
    /// or a reorder set that doesn't match the playlist's items.
    Invalid(String),
    /// An unexpected database or serialization error.
    Db(anyhow::Error),
}

impl std::fmt::Display for PlaylistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlaylistError::NotFound => write!(f, "playlist not found"),
            PlaylistError::Invalid(msg) => write!(f, "{msg}"),
            PlaylistError::Db(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PlaylistError {}

impl From<rusqlite::Error> for PlaylistError {
    fn from(e: rusqlite::Error) -> Self {
        PlaylistError::Db(e.into())
    }
}

impl From<anyhow::Error> for PlaylistError {
    fn from(e: anyhow::Error) -> Self {
        PlaylistError::Db(e)
    }
}

fn playlist_from_row(row: &Row) -> rusqlite::Result<Playlist> {
    Ok(Playlist {
        id: row.get("id")?,
        name: row.get("name")?,
        description: row.get("description")?,
        inserted_at: row.get("inserted_at")?,
        updated_at: row.get("updated_at")?,
    })
}

/// Raw `playlist_items` row before the nullable columns are validated into a
/// `PlaylistItemKind`. Mapping is split in two so the column read stays a plain
/// `rusqlite` closure and the shape validation can use `anyhow` errors.
struct RawPlaylistItem {
    id: i64,
    playlist_id: i64,
    position: i64,
    item_type: String,
    concert_id: Option<i64>,
    track_index: Option<i64>,
    child_playlist_id: Option<i64>,
}

fn raw_playlist_item_from_row(row: &Row) -> rusqlite::Result<RawPlaylistItem> {
    Ok(RawPlaylistItem {
        id: row.get("id")?,
        playlist_id: row.get("playlist_id")?,
        position: row.get("position")?,
        item_type: row.get("item_type")?,
        concert_id: row.get("concert_id")?,
        track_index: row.get("track_index")?,
        child_playlist_id: row.get("child_playlist_id")?,
    })
}

fn raw_to_playlist_item(raw: RawPlaylistItem) -> Result<PlaylistItem> {
    let kind = match raw.item_type.as_str() {
        "track" => PlaylistItemKind::Track {
            concert_id: raw.concert_id.context("track item missing concert_id")?,
            track_index: usize::try_from(
                raw.track_index.context("track item missing track_index")?,
            )
            .context("negative track_index")?,
        },
        "concert" => PlaylistItemKind::Concert {
            concert_id: raw.concert_id.context("concert item missing concert_id")?,
        },
        "playlist" => PlaylistItemKind::Playlist {
            child_playlist_id: raw
                .child_playlist_id
                .context("playlist item missing child_playlist_id")?,
        },
        other => bail!("unknown playlist item_type: {other}"),
    };
    Ok(PlaylistItem {
        id: raw.id,
        playlist_id: raw.playlist_id,
        position: raw.position,
        kind,
    })
}

pub fn create_playlist(
    conn: &Connection,
    name: &str,
    description: Option<&str>,
) -> std::result::Result<i64, PlaylistError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(PlaylistError::Invalid(
            "playlist name must not be empty".into(),
        ));
    }
    conn.execute(
        "INSERT INTO playlists (name, description) VALUES (?1, ?2)",
        params![name, description],
    )
    .context("Failed to create playlist")?;
    Ok(conn.last_insert_rowid())
}

pub fn get_playlist(conn: &Connection, id: i64) -> Result<Option<Playlist>> {
    let mut stmt = conn.prepare("SELECT * FROM playlists WHERE id = ?1")?;
    let mut iter = stmt.query_map(params![id], playlist_from_row)?;
    match iter.next() {
        Some(row) => Ok(Some(row.context("Failed to read playlist")?)),
        None => Ok(None),
    }
}

pub fn list_playlists(conn: &Connection) -> Result<Vec<Playlist>> {
    let mut stmt = conn.prepare("SELECT * FROM playlists ORDER BY name COLLATE NOCASE, id")?;
    let playlists = stmt
        .query_map([], playlist_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to list playlists")?;
    Ok(playlists)
}

pub fn find_playlist_by_name(conn: &Connection, name: &str) -> Result<Option<Playlist>> {
    let mut stmt =
        conn.prepare("SELECT * FROM playlists WHERE name = ?1 COLLATE NOCASE LIMIT 1")?;
    let mut iter = stmt.query_map(params![name.trim()], playlist_from_row)?;
    match iter.next() {
        Some(row) => Ok(Some(row.context("Failed to read playlist")?)),
        None => Ok(None),
    }
}

pub fn update_playlist(
    conn: &Connection,
    id: i64,
    name: &str,
    description: Option<&str>,
) -> std::result::Result<(), PlaylistError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(PlaylistError::Invalid(
            "playlist name must not be empty".into(),
        ));
    }
    let rows = conn
        .execute(
            "UPDATE playlists SET name = ?1, description = ?2 WHERE id = ?3",
            params![name, description, id],
        )
        .context("Failed to update playlist")?;
    if rows == 0 {
        return Err(PlaylistError::NotFound);
    }
    Ok(())
}

/// Delete a playlist. Cascades remove its items and any items nesting it.
/// Returns true if a row was deleted.
pub fn delete_playlist(conn: &Connection, id: i64) -> Result<bool> {
    let rows = conn
        .execute("DELETE FROM playlists WHERE id = ?1", params![id])
        .context("Failed to delete playlist")?;
    Ok(rows > 0)
}

pub fn list_playlist_items(conn: &Connection, playlist_id: i64) -> Result<Vec<PlaylistItem>> {
    let mut stmt =
        conn.prepare("SELECT * FROM playlist_items WHERE playlist_id = ?1 ORDER BY position, id")?;
    let raws = stmt
        .query_map(params![playlist_id], raw_playlist_item_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to list playlist items")?;
    raws.into_iter().map(raw_to_playlist_item).collect()
}

fn playlist_exists(conn: &Connection, id: i64) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM playlists WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Number of tracks in a concert's set list, or None if the concert is missing.
fn concert_set_list_len(conn: &Connection, concert_id: i64) -> Result<Option<usize>> {
    let json: Option<Option<String>> = conn
        .query_row(
            "SELECT set_list_json FROM concerts WHERE id = ?1",
            params![concert_id],
            |r| r.get(0),
        )
        .optional()
        .context("Failed to read set_list_json")?;
    match json {
        None => Ok(None), // concert row absent
        Some(j) => {
            let list: Vec<String> = j
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            Ok(Some(list.len()))
        }
    }
}

/// Would adding `child_id` as a nested item inside `parent_id` create a cycle?
/// True if they are the same playlist, or if `parent_id` is reachable from
/// `child_id` by following nested-playlist edges. A `HashSet` bounds the walk so
/// a pre-existing malformed cycle can't loop forever.
pub fn would_create_cycle(conn: &Connection, parent_id: i64, child_id: i64) -> Result<bool> {
    if parent_id == child_id {
        return Ok(true);
    }
    let mut stack = vec![child_id];
    let mut visited = HashSet::new();
    while let Some(pid) = stack.pop() {
        if pid == parent_id {
            return Ok(true);
        }
        if !visited.insert(pid) {
            continue;
        }
        let mut stmt = conn.prepare(
            "SELECT child_playlist_id FROM playlist_items
             WHERE playlist_id = ?1 AND item_type = 'playlist' AND child_playlist_id IS NOT NULL",
        )?;
        let kids = stmt
            .query_map(params![pid], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        stack.extend(kids);
    }
    Ok(false)
}

/// Append an item to a playlist at the next position. Validates the reference
/// (concert/track exists and in range; nested playlist exists and does not create
/// a cycle) before inserting. Returns the new item id.
pub fn add_playlist_item(
    conn: &Connection,
    playlist_id: i64,
    kind: &PlaylistItemKind,
) -> std::result::Result<i64, PlaylistError> {
    // Validate-then-insert runs in one transaction so the reference/cycle checks
    // and the INSERT are atomic — the invariant travels with the function rather
    // than depending on the caller holding the connection mutex across calls.
    // `unchecked_transaction` lets us do this through a shared `&Connection`
    // (the connection lives behind an Arc<Mutex>, so no real `&mut` is available).
    let tx = conn
        .unchecked_transaction()
        .context("Failed to begin add-item transaction")?;

    if !playlist_exists(&tx, playlist_id)? {
        return Err(PlaylistError::NotFound);
    }
    match kind {
        PlaylistItemKind::Track {
            concert_id,
            track_index,
        } => {
            let len = concert_set_list_len(&tx, *concert_id)?
                .ok_or_else(|| PlaylistError::Invalid(format!("concert {concert_id} not found")))?;
            if *track_index >= len {
                return Err(PlaylistError::Invalid(format!(
                    "track_index {track_index} out of range (concert has {len} tracks)"
                )));
            }
        }
        PlaylistItemKind::Concert { concert_id } => {
            if concert_set_list_len(&tx, *concert_id)?.is_none() {
                return Err(PlaylistError::Invalid(format!(
                    "concert {concert_id} not found"
                )));
            }
        }
        PlaylistItemKind::Playlist { child_playlist_id } => {
            if !playlist_exists(&tx, *child_playlist_id)? {
                return Err(PlaylistError::Invalid(format!(
                    "playlist {child_playlist_id} not found"
                )));
            }
            if would_create_cycle(&tx, playlist_id, *child_playlist_id)? {
                return Err(PlaylistError::Invalid(
                    "adding this playlist would create a cycle".into(),
                ));
            }
        }
    }

    let next_pos: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM playlist_items WHERE playlist_id = ?1",
            params![playlist_id],
            |r| r.get(0),
        )
        .context("Failed to compute next position")?;

    let (concert_id, track_index, child_playlist_id): (Option<i64>, Option<i64>, Option<i64>) =
        match kind {
            PlaylistItemKind::Track {
                concert_id,
                track_index,
            } => (Some(*concert_id), Some(*track_index as i64), None),
            PlaylistItemKind::Concert { concert_id } => (Some(*concert_id), None, None),
            PlaylistItemKind::Playlist { child_playlist_id } => {
                (None, None, Some(*child_playlist_id))
            }
        };

    tx.execute(
        "INSERT INTO playlist_items
            (playlist_id, position, item_type, concert_id, track_index, child_playlist_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            playlist_id,
            next_pos,
            kind.type_str(),
            concert_id,
            track_index,
            child_playlist_id
        ],
    )
    .context("Failed to insert playlist item")?;
    let id = tx.last_insert_rowid();
    tx.commit().context("Failed to commit add-item")?;
    Ok(id)
}

/// Remove one item from a playlist. Sibling positions are left as-is (gaps are
/// harmless). Returns true if a row was deleted.
pub fn remove_playlist_item(conn: &Connection, playlist_id: i64, item_id: i64) -> Result<bool> {
    let rows = conn
        .execute(
            "DELETE FROM playlist_items WHERE id = ?1 AND playlist_id = ?2",
            params![item_id, playlist_id],
        )
        .context("Failed to remove playlist item")?;
    Ok(rows > 0)
}

/// Renumber a playlist's items to the given order. `item_ids` must be exactly the
/// playlist's current item ids (same set, any order) or this is `Invalid`. Runs
/// in a transaction so a partial failure can't leave inconsistent positions.
pub fn reorder_playlist_items(
    conn: &mut Connection,
    playlist_id: i64,
    item_ids: &[i64],
) -> std::result::Result<(), PlaylistError> {
    // Duplicate detection is pure (no DB), so check it up front.
    let requested: HashSet<i64> = item_ids.iter().copied().collect();
    if requested.len() != item_ids.len() {
        return Err(PlaylistError::Invalid(
            "reorder contains duplicate item ids".into(),
        ));
    }

    // Read the current item set and renumber inside one transaction so the
    // set-equality check and the position UPDATEs are atomic.
    let tx = conn
        .transaction()
        .context("Failed to begin reorder transaction")?;

    if !playlist_exists(&tx, playlist_id)? {
        return Err(PlaylistError::NotFound);
    }
    let current: HashSet<i64> = {
        let mut stmt = tx.prepare("SELECT id FROM playlist_items WHERE playlist_id = ?1")?;
        let set = stmt
            .query_map(params![playlist_id], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        set
    };
    if requested != current {
        return Err(PlaylistError::Invalid(
            "reorder item ids must exactly match the playlist's items".into(),
        ));
    }
    for (pos, item_id) in item_ids.iter().enumerate() {
        tx.execute(
            "UPDATE playlist_items SET position = ?1 WHERE id = ?2 AND playlist_id = ?3",
            params![pos as i64, item_id, playlist_id],
        )
        .context("Failed to update item position")?;
    }
    tx.commit().context("Failed to commit reorder")?;
    Ok(())
}

/// Per-track durations for a concert, preferring user timestamps over auto.
/// Index i is `Some(secs)` when that track has a known duration, else `None`.
/// An empty vec means no timestamps at all (every track unknown).
pub fn track_durations(conn: &Connection, concert_id: i64) -> Result<Vec<Option<f64>>> {
    let stored = get_split_timestamps(conn, concert_id)?;
    let chosen = stored.user.or(stored.auto);
    Ok(match chosen {
        Some(v) => v.into_iter().map(|s| Some(s.duration)).collect(),
        None => Vec::new(),
    })
}

/// A playlist that contains the queried target, together with a representative
/// `playlist_items.id` used by the sidebar to issue a `DELETE` without a
/// separate lookup.
pub struct PlaylistMembership {
    pub playlist: Playlist,
    /// `MIN(i.id)` over all items matching the target in this playlist.
    /// If the target appears more than once, this selects the oldest entry;
    /// each remove re-fetches membership so successive removes peel off copies.
    pub item_id: i64,
}

/// Deserialize a `PlaylistMembership` from a row that has all `playlists`
/// columns plus an `item_id` aggregate column appended.
fn membership_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlaylistMembership> {
    let playlist = playlist_from_row(row)?;
    let item_id: i64 = row.get("item_id")?;
    Ok(PlaylistMembership { playlist, item_id })
}

/// Playlists that directly contain a given track item.
///
/// Uses `GROUP BY p.id` + bare `p.*`; safe because `p.id` is the primary key
/// so all `p.*` columns are functionally dependent on it (SQLite extension).
pub fn playlists_containing_track(
    conn: &Connection,
    concert_id: i64,
    track_index: usize,
) -> Result<Vec<PlaylistMembership>> {
    let mut stmt = conn.prepare(
        "SELECT p.*, MIN(i.id) AS item_id FROM playlists p
         JOIN playlist_items i ON i.playlist_id = p.id
         WHERE i.item_type = 'track' AND i.concert_id = ?1 AND i.track_index = ?2
         GROUP BY p.id
         ORDER BY p.name COLLATE NOCASE, p.id",
    )?;
    let out = stmt
        .query_map(params![concert_id, track_index as i64], membership_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to query playlists containing track")?;
    Ok(out)
}

/// Playlists that directly contain a given concert item.
///
/// Uses `GROUP BY p.id` + bare `p.*`; safe because `p.id` is the primary key
/// so all `p.*` columns are functionally dependent on it (SQLite extension).
pub fn playlists_containing_concert(
    conn: &Connection,
    concert_id: i64,
) -> Result<Vec<PlaylistMembership>> {
    let mut stmt = conn.prepare(
        "SELECT p.*, MIN(i.id) AS item_id FROM playlists p
         JOIN playlist_items i ON i.playlist_id = p.id
         WHERE i.item_type = 'concert' AND i.concert_id = ?1
         GROUP BY p.id
         ORDER BY p.name COLLATE NOCASE, p.id",
    )?;
    let out = stmt
        .query_map(params![concert_id], membership_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to query playlists containing concert")?;
    Ok(out)
}

/// Playlists that directly nest a given playlist as an item.
///
/// Uses `GROUP BY p.id` + bare `p.*`; safe because `p.id` is the primary key
/// so all `p.*` columns are functionally dependent on it (SQLite extension).
pub fn playlists_nesting_playlist(
    conn: &Connection,
    child_id: i64,
) -> Result<Vec<PlaylistMembership>> {
    let mut stmt = conn.prepare(
        "SELECT p.*, MIN(i.id) AS item_id FROM playlists p
         JOIN playlist_items i ON i.playlist_id = p.id
         WHERE i.item_type = 'playlist' AND i.child_playlist_id = ?1
         GROUP BY p.id
         ORDER BY p.name COLLATE NOCASE, p.id",
    )?;
    let out = stmt
        .query_map(params![child_id], membership_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to query playlists nesting playlist")?;
    Ok(out)
}

// ── Failed jobs ─────────────────────────────────────────────────────────────

pub struct FailedJob {
    pub id: i64,
    pub concert_id: i64,
    pub name: String,
    pub failed_at: String,
    pub failure_message: String,
    pub title: String,
    pub artist: String,
}

pub fn insert_failed_job(
    conn: &Connection,
    concert_id: i64,
    name: &str,
    failure_message: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO jobs (concert_id, name, failed_at, failure_message)
         VALUES (?1, ?2, datetime('now'), ?3)",
        params![concert_id, name, failure_message],
    )
    .context("Failed to insert failed job")?;
    Ok(conn.last_insert_rowid())
}

pub fn list_failed_jobs(conn: &Connection, limit: usize) -> Result<Vec<FailedJob>> {
    let mut stmt = conn.prepare(
        "SELECT j.id, j.concert_id, j.name, j.failed_at, j.failure_message,
                COALESCE(c.title, 'Unknown'), COALESCE(c.artist, '')
         FROM jobs j
         LEFT JOIN concerts c ON j.concert_id = c.id
         ORDER BY j.failed_at DESC, j.id DESC
         LIMIT ?1",
    )?;
    let jobs = stmt
        .query_map(params![limit as i64], |row| {
            Ok(FailedJob {
                id: row.get(0)?,
                concert_id: row.get(1)?,
                name: row.get(2)?,
                failed_at: row.get(3)?,
                failure_message: row.get(4)?,
                title: row.get(5)?,
                artist: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to list failed jobs")?;
    Ok(jobs)
}

pub fn get_failed_job(conn: &Connection, id: i64) -> Result<FailedJob> {
    conn.query_row(
        "SELECT j.id, j.concert_id, j.name, j.failed_at, j.failure_message,
                COALESCE(c.title, 'Unknown'), COALESCE(c.artist, '')
         FROM jobs j
         LEFT JOIN concerts c ON j.concert_id = c.id
         WHERE j.id = ?1",
        params![id],
        |row| {
            Ok(FailedJob {
                id: row.get(0)?,
                concert_id: row.get(1)?,
                name: row.get(2)?,
                failed_at: row.get(3)?,
                failure_message: row.get(4)?,
                title: row.get(5)?,
                artist: row.get(6)?,
            })
        },
    )
    .context("Failed to get failed job")
}

fn append_error(conn: &Connection, id: i64, column: &str, error: &str) -> Result<()> {
    assert!(
        column == "download_errors_json"
            || column == "split_errors_json"
            || column == "archive_errors_json",
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
        mark_download_succeeded(&conn, id, "mp4").unwrap();
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

        mark_download_succeeded(&conn, id, "mp4").unwrap();
        // Now it should succeed
        assert!(try_mark_split_started(&conn, id).unwrap());
        // Double start blocked
        assert!(!try_mark_split_started(&conn, id).unwrap());
    }

    #[test]
    fn mark_split_succeeded_and_failed() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        mark_download_succeeded(&conn, id, "mp4").unwrap();

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
        mark_download_succeeded(&conn, id1, "mp4").unwrap();
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
        mark_download_succeeded(&conn, id1, "mp4").unwrap();
        try_mark_split_started(&conn, id1).unwrap();
        try_mark_download_started(&conn, id2).unwrap();

        let (dl, sp, ar) = fail_in_progress_jobs(&conn, "server restarted").unwrap();
        assert_eq!(dl, 1);
        assert_eq!(sp, 1);
        assert_eq!(ar, 0);

        let c1 = get_concert(&conn, id1).unwrap();
        assert!(c1.split_started_at.is_none());
        assert_eq!(c1.split_errors.last().unwrap().error, "server restarted");

        let c2 = get_concert(&conn, id2).unwrap();
        assert!(c2.download_started_at.is_none());
        assert_eq!(c2.download_errors.last().unwrap().error, "server restarted");

        // Idempotent: a second call on the now-clean state touches nothing.
        let (dl2, sp2, ar2) = fail_in_progress_jobs(&conn, "server restarted").unwrap();
        assert_eq!((dl2, sp2, ar2), (0, 0, 0));
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
    fn clear_download_state_resets_download_columns_and_errors() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_failed(&conn, id, "earlier 403").unwrap();
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();
        try_mark_split_started(&conn, id).unwrap();
        mark_split_failed(&conn, id, "ffmpeg blew up").unwrap();
        try_mark_split_started(&conn, id).unwrap();
        mark_split_succeeded(&conn, id).unwrap();

        clear_download_state(&conn, id).unwrap();

        let c = get_concert(&conn, id).unwrap();
        assert!(c.downloaded_at.is_none());
        assert!(c.download_started_at.is_none());
        // download_errors cleared so the download-error badge doesn't resurface.
        assert!(c.download_errors.is_empty(), "download errors cleared");
        // split state must be preserved — tracks still exist on disk.
        assert!(c.split_at.is_some(), "split_at must be untouched");
        assert_eq!(c.split_errors.len(), 1);
        // downloaded_extension must be cleared
        assert!(c.downloaded_extension.is_none());
    }

    #[test]
    fn clear_download_state_does_not_resurrect_download_error_badge() {
        use crate::model::DownloadStatus;
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_failed(&conn, id, "first try").unwrap();
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();

        let before = get_concert(&conn, id).unwrap();
        assert_eq!(
            DownloadStatus::from_concert(&before),
            DownloadStatus::Downloaded
        );

        clear_download_state(&conn, id).unwrap();

        let after = get_concert(&conn, id).unwrap();
        assert!(after.download_errors.is_empty());
        assert_eq!(
            DownloadStatus::from_concert(&after),
            DownloadStatus::NotDownloaded
        );
    }

    #[test]
    fn clear_stale_download_errors_clears_when_delete_after_error() {
        use crate::events::{record, Event};
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        // An error, then (pre-fix) a delete recorded later that left the error.
        try_mark_download_started(&conn, id).unwrap();
        mark_download_failed(&conn, id, "earlier 403").unwrap();
        record(
            &conn,
            id,
            Event::DownloadDelete,
            "2999-01-01T00:00:00Z",
            None,
        );
        assert!(!get_concert(&conn, id).unwrap().download_errors.is_empty());

        let n = clear_stale_download_errors(&conn).unwrap();
        assert_eq!(n, 1);
        assert!(get_concert(&conn, id).unwrap().download_errors.is_empty());

        // Idempotent: a second run fixes nothing.
        assert_eq!(clear_stale_download_errors(&conn).unwrap(), 0);
    }

    #[test]
    fn clear_stale_download_errors_keeps_current_error_after_delete() {
        use crate::events::{record, Event};
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        // An old delete, then a more recent error → the error is current.
        record(
            &conn,
            id,
            Event::DownloadDelete,
            "2000-01-01T00:00:00Z",
            None,
        );
        try_mark_download_started(&conn, id).unwrap();
        mark_download_failed(&conn, id, "recent error").unwrap();

        let n = clear_stale_download_errors(&conn).unwrap();
        assert_eq!(n, 0);
        assert_eq!(
            get_concert(&conn, id).unwrap().download_errors.len(),
            1,
            "a current error must be preserved"
        );
    }

    #[test]
    fn clear_stale_download_errors_keeps_errors_without_delete_event() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_failed(&conn, id, "boom").unwrap();

        let n = clear_stale_download_errors(&conn).unwrap();
        assert_eq!(n, 0);
        assert!(!get_concert(&conn, id).unwrap().download_errors.is_empty());
    }

    #[test]
    fn mark_download_succeeded_stores_extension() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "webm").unwrap();
        let c = get_concert(&conn, id).unwrap();
        assert_eq!(c.downloaded_extension.as_deref(), Some("webm"));
    }

    #[test]
    fn set_downloaded_extension_if_missing_is_idempotent() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        set_downloaded_extension_if_missing(&conn, id, "mp4").unwrap();
        set_downloaded_extension_if_missing(&conn, id, "webm").unwrap();
        let c = get_concert(&conn, id).unwrap();
        assert_eq!(c.downloaded_extension.as_deref(), Some("mp4"));
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
    fn clear_split_state_resets_split_columns_and_errors() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();
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
        assert!(c.split_errors.is_empty(), "split errors cleared");
        assert!(c.tracks_present.is_empty(), "tracks_present cleared");
    }

    #[test]
    fn clear_split_state_does_not_resurrect_split_error_badge() {
        use crate::model::SplitStatus;
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();
        try_mark_split_started(&conn, id).unwrap();
        mark_split_failed(&conn, id, "first try").unwrap();
        try_mark_split_started(&conn, id).unwrap();
        mark_split_succeeded(&conn, id).unwrap();

        let before = get_concert(&conn, id).unwrap();
        assert_eq!(SplitStatus::from_concert(&before), SplitStatus::Split);

        clear_split_state(&conn, id).unwrap();

        let after = get_concert(&conn, id).unwrap();
        assert!(after.split_errors.is_empty());
        assert_eq!(SplitStatus::from_concert(&after), SplitStatus::NotSplit);
    }

    #[test]
    fn set_tracks_present_roundtrip() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        set_tracks_present(&conn, id, &[true, false, true]).unwrap();
        let c = get_concert(&conn, id).unwrap();
        assert_eq!(c.tracks_present, vec![true, false, true]);
    }

    #[test]
    fn tracks_present_defaults_to_empty_when_null() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        let c = get_concert(&conn, id).unwrap();
        assert!(c.tracks_present.is_empty());
    }

    #[test]
    fn tracks_liked_defaults_to_empty_when_null() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        let c = get_concert(&conn, id).unwrap();
        assert!(c.tracks_liked.is_empty());
    }

    #[test]
    fn set_tracks_liked_roundtrip() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        set_tracks_liked(&conn, id, &[false, true, false]).unwrap();
        let c = get_concert(&conn, id).unwrap();
        assert_eq!(c.tracks_liked, vec![false, true, false]);
    }

    #[test]
    fn toggle_track_liked_roundtrip() {
        let conn = open_in_memory().unwrap();
        let id = seed_with_album(&conn); // 2-song set list: "Song A", "Song B"
        assert!(toggle_track_liked(&conn, id, 1).unwrap());
        assert_eq!(
            get_concert(&conn, id).unwrap().tracks_liked,
            vec![false, true]
        );
        assert!(!toggle_track_liked(&conn, id, 1).unwrap());
        assert_eq!(
            get_concert(&conn, id).unwrap().tracks_liked,
            vec![false, false]
        );
    }

    #[test]
    fn toggle_track_liked_from_null_column() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        update_metadata(
            &conn,
            id,
            &MetadataUpdate {
                artist: "X".to_string(),
                album: "Y".to_string(),
                description: None,
                set_list: vec!["A".to_string(), "B".to_string(), "C".to_string()],
                musicians: vec![],
            },
        )
        .unwrap();
        // tracks_liked is NULL/empty at this point
        assert!(get_concert(&conn, id).unwrap().tracks_liked.is_empty());
        assert!(toggle_track_liked(&conn, id, 1).unwrap());
        assert_eq!(
            get_concert(&conn, id).unwrap().tracks_liked,
            vec![false, true, false]
        );
    }

    #[test]
    fn toggle_track_liked_records_event() {
        let conn = open_in_memory().unwrap();
        let id = seed_with_album(&conn);
        toggle_track_liked(&conn, id, 0).unwrap();
        let (event, json): (String, Option<String>) = conn
            .query_row(
                "SELECT event, json FROM events WHERE concert_id = ?1 ORDER BY id DESC LIMIT 1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(event, "track_liked");
        let v: serde_json::Value = serde_json::from_str(&json.unwrap()).unwrap();
        assert_eq!(v["track_index"], 0);
        assert_eq!(v["track_title"], "Song A");
    }

    #[test]
    fn toggle_track_liked_records_unlike_event() {
        let conn = open_in_memory().unwrap();
        let id = seed_with_album(&conn);
        toggle_track_liked(&conn, id, 0).unwrap();
        toggle_track_liked(&conn, id, 0).unwrap();
        let event: String = conn
            .query_row(
                "SELECT event FROM events WHERE concert_id = ?1 ORDER BY id DESC LIMIT 1",
                params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(event, "track_liked_delete");
    }

    #[test]
    fn toggle_track_liked_rejects_out_of_range_idx() {
        let conn = open_in_memory().unwrap();
        let id = seed_with_album(&conn); // 2-song set list
        let result = toggle_track_liked(&conn, id, 5);
        assert!(result.is_err());
        let c = get_concert(&conn, id).unwrap();
        assert!(
            c.tracks_liked.is_empty(),
            "no write should occur on rejection"
        );
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

    fn seed_url(conn: &Connection, url: &str, title: &str) -> i64 {
        upsert_listing(conn, &listing(url, title)).unwrap();
        get_concert_by_url(conn, url).unwrap().unwrap().id
    }

    #[test]
    fn mark_month_synced_and_list() {
        let conn = open_in_memory().unwrap();
        mark_month_synced(&conn, 2026, 5).unwrap();
        mark_month_synced(&conn, 2026, 4).unwrap();
        let months = list_synced_months(&conn).unwrap();
        assert_eq!(months, vec![(2026, 4), (2026, 5)]);
    }

    #[test]
    fn mark_month_synced_is_idempotent() {
        let conn = open_in_memory().unwrap();
        mark_month_synced(&conn, 2026, 5).unwrap();
        mark_month_synced(&conn, 2026, 5).unwrap();
        assert_eq!(list_synced_months(&conn).unwrap().len(), 1);
    }

    #[test]
    fn earliest_concert_date_returns_min() {
        let conn = open_in_memory().unwrap();
        upsert_listing(&conn, &listing("https://npr.org/c/1", "A")).unwrap();
        upsert_listing(
            &conn,
            &NewListing {
                source_url: "https://npr.org/c/2".to_string(),
                title: "B".to_string(),
                concert_date: Some("2020-01-15".to_string()),
                teaser: None,
            },
        )
        .unwrap();
        let earliest = earliest_concert_date(&conn).unwrap();
        assert_eq!(earliest, Some("2020-01-15".to_string()));
    }

    #[test]
    fn earliest_concert_date_returns_none_when_empty() {
        let conn = open_in_memory().unwrap();
        let earliest = earliest_concert_date(&conn).unwrap();
        assert!(earliest.is_none());
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

    #[test]
    fn list_in_progress_returns_only_active_jobs() {
        let conn = open_in_memory().unwrap();
        let id1 = seed_url(&conn, "https://npr.org/c/1", "Concert A");
        let id2 = seed_url(&conn, "https://npr.org/c/2", "Concert B");
        let _id3 = seed_url(&conn, "https://npr.org/c/3", "Concert C");

        try_mark_download_started(&conn, id1).unwrap();
        try_mark_download_started(&conn, id2).unwrap();
        mark_download_succeeded(&conn, id2, "mp4").unwrap();
        try_mark_split_started(&conn, id2).unwrap();

        let in_progress = list_in_progress(&conn).unwrap();
        assert_eq!(in_progress.len(), 2);
        let ids: Vec<i64> = in_progress.iter().map(|c| c.id).collect();
        assert!(ids.contains(&id1));
        assert!(ids.contains(&id2));
    }

    #[test]
    fn count_active_jobs_counts_downloading_and_splitting() {
        let conn = open_in_memory().unwrap();
        assert_eq!(count_active_jobs(&conn).unwrap(), 0);

        let id1 = seed_url(&conn, "https://npr.org/c/1", "Concert A");
        let id2 = seed_url(&conn, "https://npr.org/c/2", "Concert B");
        let id3 = seed_url(&conn, "https://npr.org/c/3", "Concert C");

        try_mark_download_started(&conn, id1).unwrap();
        assert_eq!(count_active_jobs(&conn).unwrap(), 1);

        try_mark_download_started(&conn, id2).unwrap();
        assert_eq!(count_active_jobs(&conn).unwrap(), 2);

        mark_download_succeeded(&conn, id2, "mp4").unwrap();
        assert_eq!(count_active_jobs(&conn).unwrap(), 1);

        try_mark_split_started(&conn, id2).unwrap();
        assert_eq!(count_active_jobs(&conn).unwrap(), 2);

        // Completed jobs should not be counted
        mark_download_succeeded(&conn, id1, "mp4").unwrap();
        mark_split_succeeded(&conn, id2).unwrap();
        assert_eq!(count_active_jobs(&conn).unwrap(), 0);

        // id3 remains idle throughout
        let _ = id3;
    }

    #[test]
    fn insert_failed_job_returns_id() {
        let conn = open_in_memory().unwrap();
        let concert_id = seed(&conn);
        let job_id = insert_failed_job(&conn, concert_id, "download", "exit 1: boom").unwrap();
        assert!(job_id > 0);
    }

    #[test]
    fn list_failed_jobs_returns_in_descending_order() {
        let conn = open_in_memory().unwrap();
        let cid = seed(&conn);
        insert_failed_job(&conn, cid, "download", "error 1").unwrap();
        insert_failed_job(&conn, cid, "split", "error 2").unwrap();
        let jobs = list_failed_jobs(&conn, 100).unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].name, "split");
        assert_eq!(jobs[1].name, "download");
    }

    #[test]
    fn list_failed_jobs_respects_limit() {
        let conn = open_in_memory().unwrap();
        let cid = seed(&conn);
        for i in 0..5 {
            insert_failed_job(&conn, cid, "download", &format!("error {}", i)).unwrap();
        }
        let jobs = list_failed_jobs(&conn, 3).unwrap();
        assert_eq!(jobs.len(), 3);
    }

    #[test]
    fn list_failed_jobs_includes_concert_info() {
        let conn = open_in_memory().unwrap();
        let cid = seed_with_album(&conn);
        insert_failed_job(&conn, cid, "download", "boom").unwrap();
        let jobs = list_failed_jobs(&conn, 10).unwrap();
        assert_eq!(jobs[0].title, "Test Concert");
        assert_eq!(jobs[0].artist, "Test Artist");
    }

    #[test]
    fn list_failed_jobs_handles_deleted_concert() {
        let conn = open_in_memory().unwrap();
        insert_failed_job(&conn, 9999, "split", "orphaned").unwrap();
        let jobs = list_failed_jobs(&conn, 10).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].title, "Unknown");
        assert_eq!(jobs[0].artist, "");
    }

    #[test]
    fn get_failed_job_returns_matching_row() {
        let conn = open_in_memory().unwrap();
        let cid = seed(&conn);
        let job_id = insert_failed_job(&conn, cid, "download", "some error").unwrap();
        let job = get_failed_job(&conn, job_id).unwrap();
        assert_eq!(job.id, job_id);
        assert_eq!(job.concert_id, cid);
        assert_eq!(job.name, "download");
        assert_eq!(job.failure_message, "some error");
    }

    #[test]
    fn get_failed_job_returns_error_for_missing_id() {
        let conn = open_in_memory().unwrap();
        assert!(get_failed_job(&conn, 9999).is_err());
    }

    #[test]
    fn settings_roundtrip() {
        let conn = open_in_memory().unwrap();
        let s = get_settings(&conn).unwrap();
        assert!(s.archive_location.is_none());
        assert_eq!(s.theme, Theme::System);

        update_archive_location(&conn, "/nas/media/music").unwrap();
        let s = get_settings(&conn).unwrap();
        assert_eq!(s.archive_location.as_deref(), Some("/nas/media/music"));

        update_archive_location(&conn, "").unwrap();
        let s = get_settings(&conn).unwrap();
        assert!(s.archive_location.is_none());

        update_theme(&conn, Theme::Dark).unwrap();
        assert_eq!(get_settings(&conn).unwrap().theme, Theme::Dark);
        update_theme(&conn, Theme::Light).unwrap();
        assert_eq!(get_settings(&conn).unwrap().theme, Theme::Light);
        update_theme(&conn, Theme::System).unwrap();
        assert_eq!(get_settings(&conn).unwrap().theme, Theme::System);
    }

    #[test]
    fn settings_trims_whitespace() {
        let conn = open_in_memory().unwrap();
        update_archive_location(&conn, "  /nas/media  ").unwrap();
        assert_eq!(
            get_settings(&conn).unwrap().archive_location.as_deref(),
            Some("/nas/media")
        );
    }

    #[test]
    fn theme_parse_rejects_garbage() {
        assert!(Theme::parse("blue").is_err());
        assert!(Theme::parse("").is_err());
        assert!(Theme::parse("Dark").is_err()); // case-sensitive
    }

    #[test]
    fn theme_parse_accepts_all_variants() {
        assert_eq!(Theme::parse("system").unwrap(), Theme::System);
        assert_eq!(Theme::parse("light").unwrap(), Theme::Light);
        assert_eq!(Theme::parse("dark").unwrap(), Theme::Dark);
    }

    #[test]
    fn get_settings_falls_back_to_system_on_unknown_theme() {
        // Defensive: if the DB somehow holds a value the enum doesn't know
        // (e.g. a future variant on an older binary), don't fail the page render.
        // The CHECK constraint normally prevents this, so drop it for this test.
        let conn = open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE settings_tmp (id INTEGER PRIMARY KEY CHECK (id = 1), \
             archive_location TEXT, theme TEXT NOT NULL DEFAULT 'system'); \
             INSERT INTO settings_tmp (id, theme) VALUES (1, 'solarized'); \
             DROP TABLE settings; ALTER TABLE settings_tmp RENAME TO settings;",
        )
        .unwrap();
        assert_eq!(get_settings(&conn).unwrap().theme, Theme::System);
    }

    #[test]
    fn try_mark_archive_started_blocks_double_start() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();

        assert!(try_mark_archive_started(&conn, id).unwrap());
        assert!(!try_mark_archive_started(&conn, id).unwrap());
    }

    #[test]
    fn try_mark_archive_started_blocks_if_already_archived() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();

        assert!(try_mark_archive_started(&conn, id).unwrap());
        mark_archive_succeeded(&conn, id).unwrap();
        assert!(!try_mark_archive_started(&conn, id).unwrap());
    }

    #[test]
    fn mark_archive_succeeded_clears_started_and_sets_archived() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();
        try_mark_archive_started(&conn, id).unwrap();

        mark_archive_succeeded(&conn, id).unwrap();
        let c = get_concert(&conn, id).unwrap();
        assert!(c.archive_started_at.is_none());
        assert!(c.archived_at.is_some());
        assert!(c.archive_errors.is_empty());
    }

    #[test]
    fn mark_archive_failed_appends_error_and_clears_started() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();
        try_mark_archive_started(&conn, id).unwrap();

        mark_archive_failed(&conn, id, "disk full").unwrap();
        let c = get_concert(&conn, id).unwrap();
        assert!(c.archive_started_at.is_none());
        assert!(c.archived_at.is_none());
        assert_eq!(c.archive_errors.len(), 1);
        assert_eq!(c.archive_errors[0].error, "disk full");
    }

    #[test]
    fn clear_archive_state_clears_when_archived() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();
        try_mark_archive_started(&conn, id).unwrap();
        mark_archive_failed(&conn, id, "disk full").unwrap();
        try_mark_archive_started(&conn, id).unwrap();
        mark_archive_succeeded(&conn, id).unwrap();

        let cleared = clear_archive_state(&conn, id).unwrap();
        assert!(cleared);
        let c = get_concert(&conn, id).unwrap();
        assert!(c.archived_at.is_none());
        assert!(c.archive_started_at.is_none());
        assert!(
            c.archive_errors.is_empty(),
            "errors_json should be reset so the archive-error badge does not resurface"
        );
    }

    #[test]
    fn clear_archive_state_noop_when_archive_in_flight() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();
        try_mark_archive_started(&conn, id).unwrap();

        let cleared = clear_archive_state(&conn, id).unwrap();
        assert!(!cleared, "must not clear while archive is in flight");
        let c = get_concert(&conn, id).unwrap();
        assert!(
            c.archive_started_at.is_some(),
            "in-flight archive's started_at must be preserved"
        );
        assert!(c.archived_at.is_none());
    }

    #[test]
    fn fail_in_progress_catches_archive_jobs() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();
        try_mark_archive_started(&conn, id).unwrap();

        let (dl, sp, ar) = fail_in_progress_jobs(&conn, "restart").unwrap();
        assert_eq!((dl, sp, ar), (0, 0, 1));

        let c = get_concert(&conn, id).unwrap();
        assert!(c.archive_started_at.is_none());
        assert_eq!(c.archive_errors[0].error, "restart");
    }

    #[test]
    fn count_active_jobs_includes_archiving() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();

        assert_eq!(count_active_jobs(&conn).unwrap(), 0);
        try_mark_archive_started(&conn, id).unwrap();
        assert_eq!(count_active_jobs(&conn).unwrap(), 1);
        mark_archive_succeeded(&conn, id).unwrap();
        assert_eq!(count_active_jobs(&conn).unwrap(), 0);
    }

    // ── Audit timestamps ────────────────────────────────────────────────────

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
    fn insert_failed_job_sets_timestamps() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        let job_id = insert_failed_job(&conn, id, "download", "boom").unwrap();
        let (inserted, updated): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT inserted_at, updated_at FROM jobs WHERE id = ?1",
                params![job_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(inserted.is_some(), "jobs.inserted_at should be set");
        assert!(updated.is_some(), "jobs.updated_at should be set");
    }

    #[test]
    fn update_theme_bumps_settings_updated_at() {
        let conn = open_in_memory().unwrap();
        conn.execute(
            "UPDATE settings SET updated_at = '2000-01-01T00:00:00Z' WHERE id = 1",
            [],
        )
        .unwrap();
        update_theme(&conn, Theme::Dark).unwrap();
        let updated: String = conn
            .query_row("SELECT updated_at FROM settings WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_ne!(updated, "2000-01-01T00:00:00Z");
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

    fn make_timestamps() -> Vec<concert_types::SongTimestamp> {
        vec![
            concert_types::SongTimestamp {
                title: "Song A".to_string(),
                start_time: 0.0,
                end_time: 120.0,
                duration: 120.0,
            },
            concert_types::SongTimestamp {
                title: "Song B".to_string(),
                start_time: 125.0,
                end_time: 250.0,
                duration: 125.0,
            },
        ]
    }

    #[test]
    fn set_and_get_auto_split_timestamps() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        let ts = make_timestamps();

        set_auto_split_timestamps(&conn, id, &ts).unwrap();

        let stored = get_split_timestamps(&conn, id).unwrap();
        assert_eq!(stored.auto, Some(ts));
        assert!(stored.user.is_none());
    }

    #[test]
    fn set_and_get_user_split_timestamps() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        let ts = make_timestamps();

        set_user_split_timestamps(&conn, id, &ts).unwrap();

        let stored = get_split_timestamps(&conn, id).unwrap();
        assert!(stored.auto.is_none());
        assert_eq!(stored.user, Some(ts));
    }

    #[test]
    fn clear_user_split_timestamps_when_set() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        let ts = make_timestamps();

        set_user_split_timestamps(&conn, id, &ts).unwrap();
        clear_user_split_timestamps(&conn, id).unwrap();

        let stored = get_split_timestamps(&conn, id).unwrap();
        assert!(stored.user.is_none());

        // Event should be recorded
        let events: Vec<String> = conn
            .prepare("SELECT event FROM events WHERE concert_id = ?1 ORDER BY id")
            .unwrap()
            .query_map(params![id], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(events.contains(&"split_timestamps_user".to_string()));
        assert!(events.contains(&"split_timestamps_reset".to_string()));
    }

    #[test]
    fn clear_user_split_timestamps_no_event_when_already_null() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);

        // Should not record a reset event when nothing was set
        conn.execute("DELETE FROM events", []).unwrap();
        clear_user_split_timestamps(&conn, id).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM events WHERE concert_id = ?1 AND event = 'split_timestamps_reset'", params![id], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn get_split_timestamps_returns_both_null_initially() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        let stored = get_split_timestamps(&conn, id).unwrap();
        assert!(stored.auto.is_none());
        assert!(stored.user.is_none());
    }

    #[test]
    fn clear_split_state_preserves_timestamp_columns() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        let ts = make_timestamps();

        set_auto_split_timestamps(&conn, id, &ts).unwrap();
        set_user_split_timestamps(&conn, id, &ts).unwrap();

        // Simulate a split (so clear_split_state won't error)
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();
        try_mark_split_started(&conn, id).unwrap();
        mark_split_succeeded(&conn, id).unwrap();
        clear_split_state(&conn, id).unwrap();

        // Both timestamp columns survive delete-split
        let stored = get_split_timestamps(&conn, id).unwrap();
        assert_eq!(stored.auto, Some(ts.clone()));
        assert_eq!(stored.user, Some(ts));
    }

    // ── Playlists ────────────────────────────────────────────────────────────

    /// Seed a concert with the given url/title and a set list, returning its id.
    fn seed_concert(conn: &Connection, url: &str, title: &str, set_list: &[&str]) -> i64 {
        upsert_listing(conn, &listing(url, title)).unwrap();
        let id = get_concert_by_url(conn, url).unwrap().unwrap().id;
        update_metadata(
            conn,
            id,
            &MetadataUpdate {
                artist: "Artist".to_string(),
                album: title.to_string(),
                description: None,
                set_list: set_list.iter().map(|s| s.to_string()).collect(),
                musicians: vec![],
            },
        )
        .unwrap();
        id
    }

    fn ts(title: &str, start: f64, end: f64) -> concert_types::SongTimestamp {
        concert_types::SongTimestamp {
            title: title.to_string(),
            start_time: start,
            end_time: end,
            duration: end - start,
        }
    }

    #[test]
    fn create_and_get_playlist() {
        let conn = open_in_memory().unwrap();
        let id = create_playlist(&conn, "  My Mix  ", Some("desc")).unwrap();
        let p = get_playlist(&conn, id).unwrap().unwrap();
        assert_eq!(p.name, "My Mix"); // trimmed
        assert_eq!(p.description.as_deref(), Some("desc"));
        assert!(p.updated_at.is_some(), "insert trigger sets updated_at");
        assert!(get_playlist(&conn, 9999).unwrap().is_none());
    }

    #[test]
    fn create_playlist_rejects_empty_name() {
        let conn = open_in_memory().unwrap();
        match create_playlist(&conn, "   ", None) {
            Err(PlaylistError::Invalid(_)) => {}
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn list_playlists_sorted_by_name() {
        let conn = open_in_memory().unwrap();
        create_playlist(&conn, "Zeta", None).unwrap();
        create_playlist(&conn, "alpha", None).unwrap();
        let names: Vec<_> = list_playlists(&conn)
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, vec!["alpha", "Zeta"]); // NOCASE order
    }

    #[test]
    fn find_playlist_by_name_is_case_insensitive() {
        let conn = open_in_memory().unwrap();
        create_playlist(&conn, "Road Trip", None).unwrap();
        assert!(find_playlist_by_name(&conn, "road trip").unwrap().is_some());
        assert!(find_playlist_by_name(&conn, "nope").unwrap().is_none());
    }

    #[test]
    fn add_items_of_each_kind_and_list_in_order() {
        let conn = open_in_memory().unwrap();
        let concert = seed_concert(&conn, "https://npr.org/a", "A", &["t0", "t1"]);
        let child = create_playlist(&conn, "Child", None).unwrap();
        let pl = create_playlist(&conn, "Parent", None).unwrap();

        add_playlist_item(
            &conn,
            pl,
            &PlaylistItemKind::Track {
                concert_id: concert,
                track_index: 1,
            },
        )
        .unwrap();
        add_playlist_item(
            &conn,
            pl,
            &PlaylistItemKind::Concert {
                concert_id: concert,
            },
        )
        .unwrap();
        add_playlist_item(
            &conn,
            pl,
            &PlaylistItemKind::Playlist {
                child_playlist_id: child,
            },
        )
        .unwrap();

        let items = list_playlist_items(&conn, pl).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].position, 0);
        assert_eq!(items[1].position, 1);
        assert_eq!(items[2].position, 2);
        assert_eq!(
            items[0].kind,
            PlaylistItemKind::Track {
                concert_id: concert,
                track_index: 1
            }
        );
        assert_eq!(
            items[1].kind,
            PlaylistItemKind::Concert {
                concert_id: concert
            }
        );
        assert_eq!(
            items[2].kind,
            PlaylistItemKind::Playlist {
                child_playlist_id: child
            }
        );
    }

    #[test]
    fn add_item_validates_references() {
        let conn = open_in_memory().unwrap();
        let concert = seed_concert(&conn, "https://npr.org/a", "A", &["t0", "t1"]);
        let pl = create_playlist(&conn, "P", None).unwrap();

        // Missing playlist.
        assert!(matches!(
            add_playlist_item(
                &conn,
                4242,
                &PlaylistItemKind::Concert {
                    concert_id: concert
                }
            ),
            Err(PlaylistError::NotFound)
        ));
        // Missing concert.
        assert!(matches!(
            add_playlist_item(&conn, pl, &PlaylistItemKind::Concert { concert_id: 999 }),
            Err(PlaylistError::Invalid(_))
        ));
        // Out-of-range track index (concert has 2 tracks: 0,1).
        assert!(matches!(
            add_playlist_item(
                &conn,
                pl,
                &PlaylistItemKind::Track {
                    concert_id: concert,
                    track_index: 2
                }
            ),
            Err(PlaylistError::Invalid(_))
        ));
        // Missing nested playlist.
        assert!(matches!(
            add_playlist_item(
                &conn,
                pl,
                &PlaylistItemKind::Playlist {
                    child_playlist_id: 999
                }
            ),
            Err(PlaylistError::Invalid(_))
        ));
    }

    #[test]
    fn nesting_rejects_self_and_cycles() {
        let conn = open_in_memory().unwrap();
        let a = create_playlist(&conn, "A", None).unwrap();
        let b = create_playlist(&conn, "B", None).unwrap();

        // Self-nest A→A.
        assert!(matches!(
            add_playlist_item(
                &conn,
                a,
                &PlaylistItemKind::Playlist {
                    child_playlist_id: a
                }
            ),
            Err(PlaylistError::Invalid(_))
        ));
        // A nests B (ok), then B→A would close a cycle.
        add_playlist_item(
            &conn,
            a,
            &PlaylistItemKind::Playlist {
                child_playlist_id: b,
            },
        )
        .unwrap();
        assert!(matches!(
            add_playlist_item(
                &conn,
                b,
                &PlaylistItemKind::Playlist {
                    child_playlist_id: a
                }
            ),
            Err(PlaylistError::Invalid(_))
        ));
    }

    #[test]
    fn remove_item_and_gaps_are_harmless() {
        let conn = open_in_memory().unwrap();
        let concert = seed_concert(&conn, "https://npr.org/a", "A", &["t0", "t1", "t2"]);
        let pl = create_playlist(&conn, "P", None).unwrap();
        let i0 = add_playlist_item(
            &conn,
            pl,
            &PlaylistItemKind::Track {
                concert_id: concert,
                track_index: 0,
            },
        )
        .unwrap();
        add_playlist_item(
            &conn,
            pl,
            &PlaylistItemKind::Track {
                concert_id: concert,
                track_index: 1,
            },
        )
        .unwrap();

        assert!(remove_playlist_item(&conn, pl, i0).unwrap());
        assert!(
            !remove_playlist_item(&conn, pl, i0).unwrap(),
            "second remove is a no-op"
        );
        let items = list_playlist_items(&conn, pl).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].kind,
            PlaylistItemKind::Track {
                concert_id: concert,
                track_index: 1
            }
        );
    }

    #[test]
    fn reorder_renumbers_and_validates_set() {
        let mut conn = open_in_memory().unwrap();
        let concert = seed_concert(&conn, "https://npr.org/a", "A", &["t0", "t1", "t2"]);
        let pl = create_playlist(&conn, "P", None).unwrap();
        let a = add_playlist_item(
            &conn,
            pl,
            &PlaylistItemKind::Track {
                concert_id: concert,
                track_index: 0,
            },
        )
        .unwrap();
        let b = add_playlist_item(
            &conn,
            pl,
            &PlaylistItemKind::Track {
                concert_id: concert,
                track_index: 1,
            },
        )
        .unwrap();
        let c = add_playlist_item(
            &conn,
            pl,
            &PlaylistItemKind::Track {
                concert_id: concert,
                track_index: 2,
            },
        )
        .unwrap();

        reorder_playlist_items(&mut conn, pl, &[c, a, b]).unwrap();
        let ids: Vec<_> = list_playlist_items(&conn, pl)
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(ids, vec![c, a, b]);

        // A set that doesn't match the playlist's items is rejected.
        assert!(matches!(
            reorder_playlist_items(&mut conn, pl, &[a, b]),
            Err(PlaylistError::Invalid(_))
        ));
        assert!(matches!(
            reorder_playlist_items(&mut conn, pl, &[a, b, c, 999]),
            Err(PlaylistError::Invalid(_))
        ));
    }

    #[test]
    fn cascade_delete_removes_items() {
        let conn = open_in_memory().unwrap();
        let concert = seed_concert(&conn, "https://npr.org/a", "A", &["t0"]);
        let child = create_playlist(&conn, "Child", None).unwrap();
        let pl = create_playlist(&conn, "P", None).unwrap();
        add_playlist_item(
            &conn,
            pl,
            &PlaylistItemKind::Concert {
                concert_id: concert,
            },
        )
        .unwrap();
        add_playlist_item(
            &conn,
            pl,
            &PlaylistItemKind::Playlist {
                child_playlist_id: child,
            },
        )
        .unwrap();
        assert_eq!(list_playlist_items(&conn, pl).unwrap().len(), 2);

        // Deleting the concert drops its item (ON DELETE CASCADE). The app has no
        // delete-concert path today, so exercise the FK directly.
        conn.execute("DELETE FROM concerts WHERE id = ?1", params![concert])
            .unwrap();
        assert_eq!(list_playlist_items(&conn, pl).unwrap().len(), 1);

        // Deleting the child playlist drops the nesting item.
        assert!(delete_playlist(&conn, child).unwrap());
        assert_eq!(list_playlist_items(&conn, pl).unwrap().len(), 0);
    }

    #[test]
    fn track_durations_prefers_user_then_auto() {
        let conn = open_in_memory().unwrap();
        let concert = seed_concert(&conn, "https://npr.org/a", "A", &["t0", "t1"]);
        assert!(
            track_durations(&conn, concert).unwrap().is_empty(),
            "no timestamps yet"
        );
        set_auto_split_timestamps(&conn, concert, &[ts("t0", 0.0, 10.0), ts("t1", 10.0, 25.0)])
            .unwrap();
        assert_eq!(
            track_durations(&conn, concert).unwrap(),
            vec![Some(10.0), Some(15.0)]
        );
        set_user_split_timestamps(&conn, concert, &[ts("t0", 0.0, 5.0), ts("t1", 5.0, 25.0)])
            .unwrap();
        assert_eq!(
            track_durations(&conn, concert).unwrap(),
            vec![Some(5.0), Some(20.0)],
            "user wins"
        );
    }

    #[test]
    fn membership_queries() {
        let conn = open_in_memory().unwrap();
        let concert = seed_concert(&conn, "https://npr.org/a", "A", &["t0", "t1"]);
        let child = create_playlist(&conn, "Child", None).unwrap();
        let p1 = create_playlist(&conn, "P1", None).unwrap();
        let p2 = create_playlist(&conn, "P2", None).unwrap();
        let i_track_p1 = add_playlist_item(
            &conn,
            p1,
            &PlaylistItemKind::Track {
                concert_id: concert,
                track_index: 0,
            },
        )
        .unwrap();
        let i_track_p2 = add_playlist_item(
            &conn,
            p2,
            &PlaylistItemKind::Track {
                concert_id: concert,
                track_index: 0,
            },
        )
        .unwrap();
        let i_concert_p1 = add_playlist_item(
            &conn,
            p1,
            &PlaylistItemKind::Concert {
                concert_id: concert,
            },
        )
        .unwrap();
        let i_nested_p1 = add_playlist_item(
            &conn,
            p1,
            &PlaylistItemKind::Playlist {
                child_playlist_id: child,
            },
        )
        .unwrap();

        let track_in = playlists_containing_track(&conn, concert, 0).unwrap();
        assert_eq!(
            track_in.iter().map(|m| m.playlist.id).collect::<Vec<_>>(),
            vec![p1, p2]
        );
        assert_eq!(track_in[0].item_id, i_track_p1);
        assert_eq!(track_in[1].item_id, i_track_p2);
        assert!(playlists_containing_track(&conn, concert, 1)
            .unwrap()
            .is_empty());

        let concert_in = playlists_containing_concert(&conn, concert).unwrap();
        assert_eq!(
            concert_in.iter().map(|m| m.playlist.id).collect::<Vec<_>>(),
            vec![p1]
        );
        assert_eq!(concert_in[0].item_id, i_concert_p1);

        let nested_in = playlists_nesting_playlist(&conn, child).unwrap();
        assert_eq!(
            nested_in.iter().map(|m| m.playlist.id).collect::<Vec<_>>(),
            vec![p1]
        );
        assert_eq!(nested_in[0].item_id, i_nested_p1);
    }

    #[test]
    fn membership_queries_duplicate_item_returns_min_item_id() {
        // A target added to the same playlist twice: MIN(item_id) is returned.
        // The sidebar removes one copy per trash click via re-fetch, so this
        // behaviour must stay stable.
        let conn = open_in_memory().unwrap();
        let concert = seed_concert(&conn, "https://npr.org/b", "B", &["t0"]);
        let p1 = create_playlist(&conn, "P1", None).unwrap();
        let first_id = add_playlist_item(
            &conn,
            p1,
            &PlaylistItemKind::Track {
                concert_id: concert,
                track_index: 0,
            },
        )
        .unwrap();
        let _second_id = add_playlist_item(
            &conn,
            p1,
            &PlaylistItemKind::Track {
                concert_id: concert,
                track_index: 0,
            },
        )
        .unwrap();

        let memberships = playlists_containing_track(&conn, concert, 0).unwrap();
        assert_eq!(memberships.len(), 1, "deduplicated to one row per playlist");
        assert_eq!(
            memberships[0].item_id, first_id,
            "MIN(item_id) selects the oldest copy"
        );
    }

    // ── list_resplit_candidates ──────────────────────────────────────────────

    fn seed_downloaded(conn: &Connection, url: &str) -> i64 {
        upsert_listing(conn, &listing(url, "Concert")).unwrap();
        let id = get_concert_by_url(conn, url).unwrap().unwrap().id;
        try_mark_download_started(conn, id).unwrap();
        mark_download_succeeded(conn, id, "mp4").unwrap();
        id
    }

    #[test]
    fn list_resplit_candidates_includes_split_with_null_user_ts() {
        let conn = open_in_memory().unwrap();
        let id = seed_downloaded(&conn, "https://npr.org/c/1");
        try_mark_split_started(&conn, id).unwrap();
        mark_split_succeeded(&conn, id).unwrap();
        // user_split_timestamps_json is NULL (default) — should be included
        let candidates = list_resplit_candidates(&conn).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].id, id);
    }

    #[test]
    fn list_resplit_candidates_excludes_split_with_user_ts() {
        let conn = open_in_memory().unwrap();
        let id = seed_downloaded(&conn, "https://npr.org/c/1");
        try_mark_split_started(&conn, id).unwrap();
        mark_split_succeeded(&conn, id).unwrap();
        // Set user timestamps — this concert should be excluded
        set_user_split_timestamps(&conn, id, &[]).unwrap();
        let candidates = list_resplit_candidates(&conn).unwrap();
        assert!(candidates.is_empty());
    }

    #[test]
    fn list_resplit_candidates_includes_split_error_concert() {
        let conn = open_in_memory().unwrap();
        let id = seed_downloaded(&conn, "https://npr.org/c/1");
        try_mark_split_started(&conn, id).unwrap();
        mark_split_failed(&conn, id, "ocr died").unwrap();
        // split_at IS NULL but split_errors non-empty — should be included
        let candidates = list_resplit_candidates(&conn).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].id, id);
    }

    #[test]
    fn list_resplit_candidates_excludes_mid_split_concert() {
        let conn = open_in_memory().unwrap();
        let id = seed_downloaded(&conn, "https://npr.org/c/1");
        // Mark as previously split, then start a new split (split_started_at set)
        try_mark_split_started(&conn, id).unwrap();
        mark_split_succeeded(&conn, id).unwrap();
        try_mark_split_started(&conn, id).unwrap(); // leaves split_started_at set
        let candidates = list_resplit_candidates(&conn).unwrap();
        assert!(candidates.is_empty());
    }

    #[test]
    fn list_resplit_candidates_excludes_never_split_concert() {
        let conn = open_in_memory().unwrap();
        seed_downloaded(&conn, "https://npr.org/c/1");
        // No split state at all
        let candidates = list_resplit_candidates(&conn).unwrap();
        assert!(candidates.is_empty());
    }

    #[test]
    fn list_resplit_candidates_ordered_by_id() {
        let conn = open_in_memory().unwrap();
        // Insert two split concerts; IDs are assigned in insertion order
        let id1 = seed_downloaded(&conn, "https://npr.org/c/1");
        try_mark_split_started(&conn, id1).unwrap();
        mark_split_succeeded(&conn, id1).unwrap();
        let id2 = seed_downloaded(&conn, "https://npr.org/c/2");
        try_mark_split_started(&conn, id2).unwrap();
        mark_split_succeeded(&conn, id2).unwrap();
        let candidates = list_resplit_candidates(&conn).unwrap();
        assert_eq!(candidates.len(), 2);
        assert!(candidates[0].id < candidates[1].id);
    }
}
