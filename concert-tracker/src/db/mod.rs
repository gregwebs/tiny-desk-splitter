//! Persistence layer entry point. Organized into domain modules — see
//! `docs/backend-persistence.md` for the full module map, type ownership,
//! dependency-direction rules, event-emission invariants, transaction
//! invariants, and lifecycle state diagram.
//!
//! One dependency-direction rule worth restating here because it constrains
//! `db::connection` directly: `db::connection::run_migrations` calls
//! `events::backfill`, which reads concerts via `db::concerts::list_concerts`.
//! So `events` may depend on concert read operations, and `db::connection`
//! may depend on `events` — but concert read operations must never depend
//! back on `db::connection` internals or on `events`, or the migration
//! startup path forms a cycle. Concert *write* operations (e.g.
//! `db::concerts::upsert_listing` recording an `Import` event) depending on
//! `events` is a separate, permitted relationship — the constraint above is
//! specifically about reads used during migration/backfill.

pub mod concerts;
pub mod connection;
pub mod failed_jobs;
pub mod lifecycle;
pub mod playlists;
pub mod settings;
pub mod split_timestamps;
pub mod sync;
pub mod time;

#[cfg(test)]
pub mod tests {
    use super::concerts::{get_concert_by_url, update_metadata, upsert_listing};
    use super::concerts::{MetadataUpdate, NewListing};
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
