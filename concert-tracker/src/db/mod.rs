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

pub mod concerts;
pub mod connection;
pub mod failed_jobs;
pub mod lifecycle;
pub mod playlists;
pub mod settings;
pub mod split_timestamps;
pub mod sync;
pub mod time;

// TEMPORARY compatibility facade (#63 expand step of the db-domain-split
// refactor, issue #69; extended in #64, extended again in #65 now that
// playlists and track_durations have moved out): re-exports so existing
// `db::...` call sites keep compiling unchanged while callers migrate to
// domain paths in #66/#67. Removed by #68.
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
pub use playlists::{
    add_playlist_item, create_playlist, delete_playlist, find_playlist_by_name, get_playlist,
    list_playlist_items, list_playlists, playlists_containing_concert, playlists_containing_track,
    playlists_nesting_playlist, remove_playlist_item, reorder_playlist_items, update_playlist,
    would_create_cycle, PlaylistError, PlaylistMembership,
};
pub use settings::{get_settings, update_archive_location, update_theme, Settings, Theme};
pub use split_timestamps::{
    clear_user_split_timestamps, get_split_timestamps, list_concerts_missing_media_duration,
    list_concerts_needing_tracks_backfill, set_auto_split_timestamps, set_media_duration,
    set_tracks_liked, set_tracks_present, set_user_split_timestamps, toggle_track_liked,
    track_durations, StoredSplitTimestamps,
};
pub use sync::{
    earliest_concert_date, list_fully_synced_months, mark_month_synced, mark_month_synced_at,
};
pub use time::now_string;

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::model::Musician;
    use rusqlite::{params, Connection};

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
}
