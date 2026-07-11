//! Dependency direction between `db::connection`, `db::concerts`, and `events`
//! (pinned down here per #63, updated per #64 now that concert reads live in
//! their own module): `db::connection::run_migrations` calls `events::backfill`,
//! which reads concerts via `db::concerts::list_concerts`. So `events` may
//! depend on concert read operations, and `db::connection` may depend on
//! `events` — but concert read operations must never depend back on
//! `db::connection` internals or on `events`, or the migration startup path
//! forms a cycle. Concert *write* operations (e.g. `db::concerts::upsert_listing`
//! recording an `Import` event) depending on `events` is a separate, permitted
//! relationship — the constraint above is specifically about reads used during
//! migration/backfill.

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::collections::HashSet;

use crate::model::{Playlist, PlaylistItem, PlaylistItemKind};

pub mod concerts;
pub mod connection;
pub mod failed_jobs;
pub mod lifecycle;
pub mod settings;
pub mod split_timestamps;
pub mod sync;
pub mod time;

// TEMPORARY compatibility facade (#63 expand step of the db-domain-split
// refactor, issue #69; extended in #64): re-exports so existing `db::...`
// call sites keep compiling unchanged while callers migrate to domain paths
// in #66/#67. Removed by #68.
//
// This block is also load-bearing for code still living directly in this
// module: `track_durations` below calls `get_split_timestamps` unqualified,
// resolving it through this `pub use` rather than a direct import. #68 must
// add a direct `use split_timestamps::get_split_timestamps;` when the facade
// is removed.
pub use concerts::{
    get_concert, get_concert_by_album, get_concert_by_url, get_concert_opt, list_concerts,
    list_concerts_missing_teaser, set_notes, set_teaser, toggle_ignored, toggle_wanted,
    update_metadata, upsert_listing, MetadataUpdate, NewListing,
};
pub use connection::{open, open_in_memory};
pub use failed_jobs::{get_failed_job, insert_failed_job, list_failed_jobs, FailedJob};
pub use lifecycle::{
    clear_archive_state, clear_download_state, clear_split_state, clear_stale_download_errors,
    count_active_jobs, fail_in_progress_jobs, list_in_progress, list_resplit_candidates,
    mark_archive_failed, mark_archive_succeeded, mark_download_failed, mark_download_succeeded,
    mark_split_failed, mark_split_succeeded, reset_in_progress, set_downloaded_at_if_missing,
    set_downloaded_extension_if_missing, set_split_at_if_missing, try_mark_archive_started,
    try_mark_download_started, try_mark_split_started,
};
pub use settings::{get_settings, update_archive_location, update_theme, Settings, Theme};
pub use split_timestamps::{
    clear_user_split_timestamps, get_split_timestamps, list_concerts_missing_media_duration,
    list_concerts_needing_tracks_backfill, set_auto_split_timestamps, set_media_duration,
    set_tracks_liked, set_tracks_present, set_user_split_timestamps, toggle_track_liked,
    StoredSplitTimestamps,
};
pub use sync::{
    earliest_concert_date, list_fully_synced_months, mark_month_synced, mark_month_synced_at,
};
pub use time::now_string;

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

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::model::Musician;

    /// All `(event, json)` rows for a concert, oldest first. `pub(crate)`:
    /// shared by every domain's event-characterization tests (#64), which pin
    /// down exactly which events an operation emits — including that a
    /// guarded no-op operation emits none.
    pub(crate) fn events_for(conn: &Connection, id: i64) -> Vec<(String, Option<String>)> {
        conn.prepare("SELECT event, json FROM events WHERE concert_id = ?1 ORDER BY id")
            .unwrap()
            .query_map(params![id], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    /// `pub(crate)`: shared with every domain test module's `seed`/`upsert_listing`
    /// call sites.
    pub(crate) fn listing(url: &str, title: &str) -> NewListing {
        NewListing {
            source_url: url.to_string(),
            title: title.to_string(),
            concert_date: Some("2024-06-01".to_string()),
            teaser: Some("Great show".to_string()),
        }
    }

    /// `pub(crate)`: shared with every domain test module (`db::concerts`,
    /// `db::lifecycle`, `db::split_timestamps`, `db::sync`, `db::failed_jobs`,
    /// `db::connection`) that needs a seeded concert row.
    pub(crate) fn seed(conn: &Connection) -> i64 {
        upsert_listing(conn, &listing("https://npr.org/c/1", "Test Concert")).unwrap();
        let c = get_concert_by_url(conn, "https://npr.org/c/1")
            .unwrap()
            .unwrap();
        c.id
    }

    /// `pub(crate)`: shared with `db::concerts`, `db::split_timestamps`, and
    /// `db::failed_jobs` tests that need a concert with metadata/set list.
    pub(crate) fn seed_with_album(conn: &Connection) -> i64 {
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
}
