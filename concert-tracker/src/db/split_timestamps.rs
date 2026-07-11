use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use super::concerts::{concert_from_row, get_concert};
use crate::events::{self, Event};
use crate::model::Concert;

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

/// Persist the source-file duration in seconds. This is set at user-split time
/// and survives source-file deletion so the coverage gate stays functional.
/// Only stores a new value when it is a finite, positive number, and **never
/// overwrites an existing good value with NULL or a bad value** (fail-closed).
pub fn set_media_duration(conn: &Connection, id: i64, duration: f64) -> Result<()> {
    if !duration.is_finite() || duration <= 0.0 {
        return Err(anyhow::anyhow!(
            "set_media_duration: invalid duration {duration}"
        ));
    }
    conn.execute(
        "UPDATE concerts
         SET media_duration = ?1
         WHERE id = ?2
           AND (media_duration IS NULL OR media_duration <= 0)",
        params![duration, id],
    )
    .context("Failed to set media_duration")?;
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

/// Concerts split before the `media_duration` column existed (or whose value was
/// never persisted). Candidates for the `concert_db backfill-media-duration` CLI.
pub fn list_concerts_missing_media_duration(conn: &Connection) -> Result<Vec<Concert>> {
    let mut stmt = conn
        .prepare("SELECT * FROM concerts WHERE media_duration IS NULL")
        .context("Failed to prepare media_duration backfill query")?;
    let concerts = stmt
        .query_map([], concert_from_row)?
        .filter_map(|r| r.ok())
        .collect();
    Ok(concerts)
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::db::concerts::{update_metadata, MetadataUpdate};
    use crate::db::connection::open_in_memory;
    use crate::db::tests::{events_for, seed, seed_with_album};

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

    pub(crate) fn make_timestamps() -> Vec<concert_types::SongTimestamp> {
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
    fn set_media_duration_roundtrip() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);

        // Initial value is None.
        let c = get_concert(&conn, id).unwrap();
        assert!(c.media_duration.is_none());

        // Persist a valid duration.
        set_media_duration(&conn, id, 180.5).unwrap();
        let c = get_concert(&conn, id).unwrap();
        assert_eq!(c.media_duration, Some(180.5));
    }

    #[test]
    fn set_media_duration_rejects_invalid() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        assert!(set_media_duration(&conn, id, f64::NAN).is_err());
        assert!(set_media_duration(&conn, id, -1.0).is_err());
        assert!(set_media_duration(&conn, id, 0.0).is_err());
    }

    #[test]
    fn set_media_duration_never_overwrites_good_value_with_bad() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);

        // Write a good value first.
        set_media_duration(&conn, id, 200.0).unwrap();
        // A second write (simulating a best-effort GET-path persist) should not
        // overwrite the existing good value.
        set_media_duration(&conn, id, 150.0).unwrap(); // silently no-ops
        let c = get_concert(&conn, id).unwrap();
        assert_eq!(
            c.media_duration,
            Some(200.0),
            "original value must be preserved"
        );
    }

    // ── Event characterization (#64) ────────────────────────────────────────

    #[test]
    fn set_user_split_timestamps_emits_event_with_payload() {
        let conn = open_in_memory().unwrap();
        let id = seed(&conn);
        let ts = make_timestamps();
        set_user_split_timestamps(&conn, id, &ts).unwrap();
        let (event, json) = events_for(&conn, id).into_iter().next_back().unwrap();
        assert_eq!(event, "split_timestamps_user");
        let v: serde_json::Value = serde_json::from_str(&json.unwrap()).unwrap();
        let parsed: Vec<concert_types::SongTimestamp> = serde_json::from_value(v).unwrap();
        assert_eq!(parsed, ts);
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
    fn track_durations_prefers_user_then_auto() {
        let conn = open_in_memory().unwrap();
        let concert = seed_with_album(&conn); // 2-song set list: "Song A", "Song B"
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
}
