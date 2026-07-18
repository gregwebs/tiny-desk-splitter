//! Playlist expansion: turn a playlist's stored items (tracks, whole concerts,
//! nested playlists) into a flat, ordered list of concrete tracks resolved
//! against the *current* database state ("live reference"). This is where the
//! tree of `playlist_items` becomes the sequence the player and the list page
//! consume.
//!
//! Two defensive properties matter here:
//! - A `track` item whose `track_index` is now out of range (the concert's
//!   `set_list` shrank on re-scrape) is **skipped**, never an error — one stale
//!   reference must not poison the whole playlist.
//! - Nested playlists are expanded recursively with a path-set cycle guard, so a
//!   cycle that slipped past the add-time check can't loop forever.

use std::collections::HashSet;

use anyhow::Result;
use rusqlite::Connection;

use crate::db;
use crate::model::{self, PlaylistItemKind, PlaylistSummary, ResolvedTrack};

/// Flatten a playlist into its ordered, resolved tracks.
pub fn expand_playlist(conn: &Connection, playlist_id: i64) -> Result<Vec<ResolvedTrack>> {
    let mut path = HashSet::new();
    expand_inner(conn, playlist_id, &mut path)
}

fn expand_inner(
    conn: &Connection,
    playlist_id: i64,
    path: &mut HashSet<i64>,
) -> Result<Vec<ResolvedTrack>> {
    // `path` holds the playlist ids on the current recursion stack. If this id is
    // already present we are in a cycle: skip rather than recurse forever.
    if !path.insert(playlist_id) {
        tracing::warn!(
            playlist_id,
            "cycle detected while expanding playlist; skipping"
        );
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for item in db::playlists::list_playlist_items(conn, playlist_id)? {
        match item.kind {
            PlaylistItemKind::Track {
                concert_id,
                track_index,
            } => match resolve_track(conn, concert_id, track_index)? {
                Some(rt) => out.push(rt),
                None => tracing::warn!(
                    concert_id,
                    track_index,
                    "playlist track item is out of range or its concert is missing; skipping"
                ),
            },
            PlaylistItemKind::Concert { concert_id } => {
                out.extend(resolve_concert(conn, concert_id)?);
            }
            PlaylistItemKind::Playlist { child_playlist_id } => {
                out.extend(expand_inner(conn, child_playlist_id, path)?);
            }
        }
    }

    // Leave the stack so the same playlist nested twice in non-cyclic ways still
    // expands each time (only true ancestor cycles are blocked above).
    path.remove(&playlist_id);
    Ok(out)
}

/// Resolve a single track item, or `None` if the concert is gone or the index is
/// now out of range.
fn resolve_track(
    conn: &Connection,
    concert_id: i64,
    track_index: usize,
) -> Result<Option<ResolvedTrack>> {
    // Genuinely-missing concert → skip (None); a real DB error propagates.
    let concert = match db::concerts::get_concert_opt(conn, concert_id)? {
        Some(c) => c,
        None => return Ok(None),
    };
    if track_index >= concert.set_list.len() {
        return Ok(None);
    }
    let durations = db::split_timestamps::track_durations(conn, concert_id)?;
    Ok(Some(ResolvedTrack {
        concert_id,
        track_index,
        title: concert.set_list[track_index].clone(),
        duration: durations.get(track_index).copied().flatten(),
        available: concert
            .tracks_present
            .get(track_index)
            .copied()
            .unwrap_or(false),
    }))
}

/// Resolve a whole-concert item into all of its tracks (one DB read for the
/// concert + one for its durations, shared across the tracks).
fn resolve_concert(conn: &Connection, concert_id: i64) -> Result<Vec<ResolvedTrack>> {
    // Genuinely-missing concert → no tracks; a real DB error propagates.
    let concert = match db::concerts::get_concert_opt(conn, concert_id)? {
        Some(c) => c,
        None => return Ok(Vec::new()),
    };
    let durations = db::split_timestamps::track_durations(conn, concert_id)?;
    let tracks = model::list_all_tracks_from_db(
        &concert.set_list,
        &concert.tracks_present,
        &concert.tracks_liked,
    );
    Ok(tracks
        .into_iter()
        .map(|t| ResolvedTrack {
            concert_id,
            track_index: t.index,
            duration: durations.get(t.index).copied().flatten(),
            available: t.available,
            title: t.title,
        })
        .collect())
}

/// Aggregate a playlist for the list page: track count, summed known duration,
/// count of tracks with unknown duration, and the first track that would play.
pub fn summarize_playlist(conn: &Connection, playlist_id: i64) -> Result<PlaylistSummary> {
    let tracks = expand_playlist(conn, playlist_id)?;
    let track_count = tracks.len();
    let mut known_duration_secs = 0.0;
    let mut unknown_count = 0;
    for t in &tracks {
        match t.duration {
            Some(d) => known_duration_secs += d,
            None => unknown_count += 1,
        }
    }
    let first_track = tracks.into_iter().next();
    Ok(PlaylistSummary {
        track_count,
        known_duration_secs,
        unknown_count,
        first_track,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::db::concerts::MetadataUpdate;
    use crate::model::PlaylistItemKind;

    fn seed_concert(conn: &Connection, url: &str, title: &str, set_list: &[&str]) -> i64 {
        db::seeds::SeedContext::new(conn)
            .seed_scraped_concert(db::seeds::SeedScrapedConcert {
                source_url: Some(url.to_string()),
                title: Some(title.to_string()),
                concert_date: None,
                artist: Some("Artist".to_string()),
                album: Some(title.to_string()),
                set_list: Some(set_list.iter().map(|s| s.to_string()).collect()),
            })
            .unwrap()
            .id
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
    fn flattens_track_concert_and_nested_playlist_in_order() {
        let conn = db::connection::open_in_memory().unwrap();
        let a = seed_concert(&conn, "https://npr.org/a", "A", &["a0", "a1"]);
        let b = seed_concert(&conn, "https://npr.org/b", "B", &["b0", "b1", "b2"]);
        db::split_timestamps::set_tracks_present(&conn, a, &[true, true]).unwrap();

        let child = db::playlists::create_playlist(&conn, "Child", None).unwrap();
        db::playlists::add_playlist_item(
            &conn,
            child,
            &PlaylistItemKind::Track {
                concert_id: b,
                track_index: 2,
            },
        )
        .unwrap();

        let pl = db::playlists::create_playlist(&conn, "Parent", None).unwrap();
        db::playlists::add_playlist_item(
            &conn,
            pl,
            &PlaylistItemKind::Track {
                concert_id: a,
                track_index: 1,
            },
        )
        .unwrap();
        db::playlists::add_playlist_item(&conn, pl, &PlaylistItemKind::Concert { concert_id: b })
            .unwrap();
        db::playlists::add_playlist_item(
            &conn,
            pl,
            &PlaylistItemKind::Playlist {
                child_playlist_id: child,
            },
        )
        .unwrap();

        let tracks = expand_playlist(&conn, pl).unwrap();
        let got: Vec<(i64, usize, &str)> = tracks
            .iter()
            .map(|t| (t.concert_id, t.track_index, t.title.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (a, 1, "a1"), // track item
                (b, 0, "b0"), // concert item expands to all of B
                (b, 1, "b1"),
                (b, 2, "b2"),
                (b, 2, "b2"), // nested child playlist -> B track 2
            ]
        );
        // availability comes from tracks_present (only A was marked present).
        assert!(tracks[0].available);
        assert!(!tracks[1].available);
    }

    #[test]
    fn track_item_out_of_range_after_shrink_is_skipped_not_errored() {
        let conn = db::connection::open_in_memory().unwrap();
        let c = seed_concert(&conn, "https://npr.org/a", "A", &["t0", "t1", "t2"]);
        let pl = db::playlists::create_playlist(&conn, "P", None).unwrap();
        // Valid at add-time (index 2 of 3).
        db::playlists::add_playlist_item(
            &conn,
            pl,
            &PlaylistItemKind::Track {
                concert_id: c,
                track_index: 2,
            },
        )
        .unwrap();
        db::playlists::add_playlist_item(
            &conn,
            pl,
            &PlaylistItemKind::Track {
                concert_id: c,
                track_index: 0,
            },
        )
        .unwrap();

        // Re-scrape shrinks the set list to one track, orphaning index 2.
        db::concerts::update_metadata(
            &conn,
            c,
            &MetadataUpdate {
                artist: "Artist".to_string(),
                album: "A".to_string(),
                description: None,
                set_list: vec!["t0".to_string()],
                musicians: vec![],
            },
        )
        .unwrap();

        let tracks = expand_playlist(&conn, pl).unwrap();
        // The orphaned index-2 item is skipped; index-0 still resolves.
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].track_index, 0);
        assert_eq!(tracks[0].title, "t0");
    }

    #[test]
    fn summary_sums_known_and_counts_unknown() {
        let conn = db::connection::open_in_memory().unwrap();
        // A has durations; B has none.
        let a = seed_concert(&conn, "https://npr.org/a", "A", &["a0", "a1"]);
        let b = seed_concert(&conn, "https://npr.org/b", "B", &["b0"]);
        db::split_timestamps::set_auto_split_timestamps(
            &conn,
            a,
            &[ts("a0", 0.0, 10.0), ts("a1", 10.0, 30.0)],
        )
        .unwrap();

        let pl = db::playlists::create_playlist(&conn, "P", None).unwrap();
        db::playlists::add_playlist_item(&conn, pl, &PlaylistItemKind::Concert { concert_id: a })
            .unwrap();
        db::playlists::add_playlist_item(&conn, pl, &PlaylistItemKind::Concert { concert_id: b })
            .unwrap();

        let s = summarize_playlist(&conn, pl).unwrap();
        assert_eq!(s.track_count, 3);
        assert_eq!(s.known_duration_secs, 30.0);
        assert_eq!(s.unknown_count, 1);
        assert_eq!(s.first_track.unwrap().title, "a0");
    }

    #[test]
    fn empty_playlist_summarizes_to_zero() {
        let conn = db::connection::open_in_memory().unwrap();
        let pl = db::playlists::create_playlist(&conn, "Empty", None).unwrap();
        let s = summarize_playlist(&conn, pl).unwrap();
        assert_eq!(s.track_count, 0);
        assert_eq!(s.known_duration_secs, 0.0);
        assert_eq!(s.unknown_count, 0);
        assert!(s.first_track.is_none());
    }
}
