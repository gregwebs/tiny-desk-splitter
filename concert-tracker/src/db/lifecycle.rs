use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};

use super::concerts::{concert_from_row, get_concert};
use crate::events::{self, Event};
use crate::model::{Concert, ErrorEntry};

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
mod tests {
    use super::*;
    use crate::db::connection::open_in_memory;
    use crate::db::split_timestamps::tests::make_timestamps;
    use crate::db::split_timestamps::{
        get_split_timestamps, set_auto_split_timestamps, set_user_split_timestamps,
    };
    use crate::db::tests::{events_for, seed, seed_with_album};

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
        let id2 = seed_url(&conn, "https://npr.org/c/2", "B");

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
        let id2 = seed_url(&conn, "https://npr.org/c/2", "B");

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

    /// Same values as `db::tests::listing(url, title)` (date `2024-06-01`,
    /// teaser "Great show"), via `SeedContext::seed_listing`.
    fn seed_url(conn: &Connection, url: &str, title: &str) -> i64 {
        crate::db::seeds::SeedContext::new(conn)
            .seed_listing(crate::db::seeds::SeedListing {
                source_url: Some(url.to_string()),
                title: Some(title.to_string()),
                concert_date: Some("2024-06-01".to_string()),
                teaser: Some("Great show".to_string()),
            })
            .unwrap()
            .id
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

    fn seed_downloaded(conn: &Connection, url: &str) -> i64 {
        let id = seed_url(conn, url, "Concert");
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

    // ── Event characterization (#64) ────────────────────────────────────────
    //
    // Pin down exactly which events each lifecycle operation emits, including
    // that a guarded no-op emits none. These assert current behavior; a
    // mismatch found here is a finding to report, not a bug to fix in place.

    #[test]
    fn try_mark_download_started_emits_event_only_on_first_call() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        let before = events_for(&conn, id).len();
        assert!(try_mark_download_started(&conn, id).unwrap());
        let events = events_for(&conn, id);
        assert_eq!(events.len(), before + 1);
        assert_eq!(
            events.last().unwrap(),
            &("download_started".to_string(), None)
        );

        // The guarded second call must not record another event.
        assert!(!try_mark_download_started(&conn, id).unwrap());
        assert_eq!(events_for(&conn, id).len(), before + 1);
    }

    #[test]
    fn mark_download_succeeded_emits_downloaded_event() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();
        assert_eq!(
            events_for(&conn, id).last().unwrap(),
            &("downloaded".to_string(), None)
        );
    }

    #[test]
    fn mark_download_failed_emits_download_error_with_payload() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_failed(&conn, id, "boom").unwrap();
        let (event, json) = events_for(&conn, id).into_iter().next_back().unwrap();
        assert_eq!(event, "download_error");
        let v: serde_json::Value = serde_json::from_str(&json.unwrap()).unwrap();
        assert_eq!(v["error"], "boom");
    }

    #[test]
    fn clear_download_state_emits_download_delete_event() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();
        clear_download_state(&conn, id).unwrap();
        assert_eq!(
            events_for(&conn, id).last().unwrap(),
            &("download_delete".to_string(), None)
        );
    }

    #[test]
    fn mark_split_succeeded_emits_split_event_with_tracks_payload_when_set_list_present() {
        let conn = open_in_memory().unwrap();
        let id = seed_with_album(&conn); // 2-song set list
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();
        try_mark_split_started(&conn, id).unwrap();
        mark_split_succeeded(&conn, id).unwrap();
        let (event, json) = events_for(&conn, id).into_iter().next_back().unwrap();
        assert_eq!(event, "split");
        let v: serde_json::Value = serde_json::from_str(&json.unwrap()).unwrap();
        assert_eq!(v["track_count"], 2);
        assert_eq!(v["tracks"], serde_json::json!(["Song A", "Song B"]));
    }

    #[test]
    fn mark_split_succeeded_emits_split_event_with_null_payload_when_no_set_list() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn); // no set list
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();
        try_mark_split_started(&conn, id).unwrap();
        mark_split_succeeded(&conn, id).unwrap();
        let (event, json) = events_for(&conn, id).into_iter().next_back().unwrap();
        assert_eq!(event, "split");
        assert!(json.is_none(), "empty set_list must produce a NULL payload");
    }

    #[test]
    fn mark_split_failed_emits_split_error_with_payload() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();
        try_mark_split_started(&conn, id).unwrap();
        mark_split_failed(&conn, id, "ffmpeg error").unwrap();
        let (event, json) = events_for(&conn, id).into_iter().next_back().unwrap();
        assert_eq!(event, "split_error");
        let v: serde_json::Value = serde_json::from_str(&json.unwrap()).unwrap();
        assert_eq!(v["error"], "ffmpeg error");
    }

    #[test]
    fn try_mark_split_started_emits_nothing_without_downloaded_at() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        let before = events_for(&conn, id);
        assert!(!try_mark_split_started(&conn, id).unwrap());
        assert_eq!(events_for(&conn, id), before, "guarded no-op emits nothing");
    }

    #[test]
    fn clear_split_state_emits_split_delete_event() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();
        try_mark_split_started(&conn, id).unwrap();
        mark_split_succeeded(&conn, id).unwrap();
        clear_split_state(&conn, id).unwrap();
        assert_eq!(
            events_for(&conn, id).last().unwrap(),
            &("split_delete".to_string(), None)
        );
    }

    #[test]
    fn mark_archive_succeeded_emits_archived_event() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();
        try_mark_archive_started(&conn, id).unwrap();
        mark_archive_succeeded(&conn, id).unwrap();
        assert_eq!(
            events_for(&conn, id).last().unwrap(),
            &("archived".to_string(), None)
        );
    }

    #[test]
    fn mark_archive_failed_emits_archive_error_with_payload() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();
        try_mark_archive_started(&conn, id).unwrap();
        mark_archive_failed(&conn, id, "disk full").unwrap();
        let (event, json) = events_for(&conn, id).into_iter().next_back().unwrap();
        assert_eq!(event, "archive_error");
        let v: serde_json::Value = serde_json::from_str(&json.unwrap()).unwrap();
        assert_eq!(v["error"], "disk full");
    }

    #[test]
    fn try_mark_archive_started_emits_nothing_when_already_archived() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();
        try_mark_archive_started(&conn, id).unwrap();
        mark_archive_succeeded(&conn, id).unwrap();

        let before = events_for(&conn, id);
        assert!(!try_mark_archive_started(&conn, id).unwrap());
        assert_eq!(events_for(&conn, id), before, "guarded no-op emits nothing");
    }

    #[test]
    fn clear_archive_state_emits_nothing_when_archive_in_flight() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        try_mark_download_started(&conn, id).unwrap();
        mark_download_succeeded(&conn, id, "mp4").unwrap();
        try_mark_archive_started(&conn, id).unwrap();

        let before = events_for(&conn, id);
        assert!(!clear_archive_state(&conn, id).unwrap());
        assert_eq!(events_for(&conn, id), before, "guarded no-op emits nothing");
    }
}
