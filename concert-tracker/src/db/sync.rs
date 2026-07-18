use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use super::time::now_string;

/// Records a sync at an explicit timestamp. Exists separately from
/// `mark_month_synced` so tests can inject a `synced_at` and deterministically
/// exercise the month-completeness predicate in `list_fully_synced_months`
/// without sleeping or mocking the clock.
pub fn mark_month_synced_at(
    conn: &Connection,
    year: i32,
    month: u32,
    synced_at: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO synced_months (year, month, synced_at) VALUES (?1, ?2, ?3)",
        params![year, month, synced_at],
    )
    .context("Failed to mark month synced")?;
    Ok(())
}

pub fn mark_month_synced(conn: &Connection, year: i32, month: u32) -> Result<()> {
    mark_month_synced_at(conn, year, month, &now_string())
}

/// Grace window added to a month's end before a sync of that month counts as
/// "complete". `synced_at` is stored in UTC but NPR's publish clock is US
/// Eastern, so a bare UTC month-end boundary would mark a month complete
/// several hours before its Eastern end and re-introduce a (smaller, sticky)
/// version of the mid-month-sync bug this predicate exists to fix. `+5 hours`
/// covers the EST offset (UTC-5); during EDT (UTC-4) this is up to an hour
/// over-generous, which is harmless — the invariant we must preserve is that
/// the boundary is never *early*, only ever slightly late.
const MONTH_END_SYNC_GRACE: &str = "+5 hours";

/// A month is "fully synced" only once a sync was recorded at or after that
/// month's end (see `MONTH_END_SYNC_GRACE`). A month synced only while still
/// in progress is deliberately excluded, so its Sync button keeps showing
/// until a later sync catches its final concerts.
pub fn list_fully_synced_months(conn: &Connection) -> Result<Vec<(i32, u32)>> {
    let mut stmt = conn.prepare(
        "SELECT year, month FROM synced_months \
         WHERE datetime(synced_at) >= \
               datetime(printf('%04d-%02d-01 00:00:00', year, month), '+1 month', ?1) \
         ORDER BY year, month",
    )?;
    let rows = stmt
        .query_map(params![MONTH_END_SYNC_GRACE], |row| {
            Ok((row.get::<_, i32>(0)?, row.get::<_, u32>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to list fully synced months")?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::connection::open_in_memory;
    use crate::db::seeds::{SeedContext, SeedListing};

    #[test]
    fn mark_month_synced_and_list() {
        let conn = open_in_memory().unwrap();
        mark_month_synced_at(&conn, 2026, 5, "2026-06-10 00:00:00").unwrap();
        mark_month_synced_at(&conn, 2026, 4, "2026-05-10 00:00:00").unwrap();
        let months = list_fully_synced_months(&conn).unwrap();
        assert_eq!(months, vec![(2026, 4), (2026, 5)]);
    }

    #[test]
    fn mark_month_synced_is_idempotent() {
        let conn = open_in_memory().unwrap();
        mark_month_synced_at(&conn, 2026, 5, "2026-06-10 00:00:00").unwrap();
        mark_month_synced_at(&conn, 2026, 5, "2026-06-11 00:00:00").unwrap();
        assert_eq!(list_fully_synced_months(&conn).unwrap().len(), 1);
    }

    // A month synced only while still in progress must not count as "fully
    // synced" -- otherwise its Sync button disappears before its final
    // concerts (published late in the month) are ever fetched.

    #[test]
    fn list_fully_synced_months_excludes_month_synced_mid_month() {
        let conn = open_in_memory().unwrap();
        mark_month_synced_at(&conn, 2026, 6, "2026-06-30 23:00:00").unwrap();
        assert_eq!(list_fully_synced_months(&conn).unwrap(), Vec::new());
    }

    #[test]
    fn list_fully_synced_months_includes_month_synced_after_it_ended() {
        let conn = open_in_memory().unwrap();
        mark_month_synced_at(&conn, 2026, 6, "2026-07-05 12:00:00").unwrap();
        assert_eq!(list_fully_synced_months(&conn).unwrap(), vec![(2026, 6)]);
    }

    #[test]
    fn list_fully_synced_months_handles_december_year_wrap() {
        let conn = open_in_memory().unwrap();
        mark_month_synced_at(&conn, 2025, 12, "2026-01-10 00:00:00").unwrap();
        assert_eq!(list_fully_synced_months(&conn).unwrap(), vec![(2025, 12)]);
    }

    #[test]
    fn list_fully_synced_months_reflects_latest_sync_after_resync() {
        let conn = open_in_memory().unwrap();
        mark_month_synced_at(&conn, 2026, 6, "2026-06-15 00:00:00").unwrap();
        assert_eq!(list_fully_synced_months(&conn).unwrap(), Vec::new());
        mark_month_synced_at(&conn, 2026, 6, "2026-07-02 00:00:00").unwrap();
        assert_eq!(list_fully_synced_months(&conn).unwrap(), vec![(2026, 6)]);
    }

    // The next four cases pin down the MONTH_END_SYNC_GRACE boundary. June's
    // end is midnight US Eastern, i.e. 2026-07-01 04:00 UTC (EDT); the grace
    // pushes the accepted boundary to 2026-07-01 05:00 UTC so the predicate
    // never fires early relative to Eastern, only up to ~1h late.

    #[test]
    fn list_fully_synced_months_excludes_before_grace_window() {
        let conn = open_in_memory().unwrap();
        mark_month_synced_at(&conn, 2026, 6, "2026-07-01 02:00:00").unwrap();
        assert_eq!(list_fully_synced_months(&conn).unwrap(), Vec::new());
    }

    #[test]
    fn list_fully_synced_months_includes_within_grace_window() {
        let conn = open_in_memory().unwrap();
        mark_month_synced_at(&conn, 2026, 6, "2026-07-01 06:00:00").unwrap();
        assert_eq!(list_fully_synced_months(&conn).unwrap(), vec![(2026, 6)]);
    }

    #[test]
    fn list_fully_synced_months_includes_at_grace_boundary() {
        let conn = open_in_memory().unwrap();
        mark_month_synced_at(&conn, 2026, 6, "2026-07-01 05:00:00").unwrap();
        assert_eq!(list_fully_synced_months(&conn).unwrap(), vec![(2026, 6)]);
    }

    #[test]
    fn list_fully_synced_months_excludes_just_before_grace_boundary() {
        let conn = open_in_memory().unwrap();
        // Eastern midnight has passed (EDT, UTC-4) but the +5h UTC grace has not.
        mark_month_synced_at(&conn, 2026, 6, "2026-07-01 04:30:00").unwrap();
        assert_eq!(list_fully_synced_months(&conn).unwrap(), Vec::new());
    }

    #[test]
    fn earliest_concert_date_returns_min() {
        let conn = open_in_memory().unwrap();
        let seeds = SeedContext::new(&conn);
        seeds
            .seed_listing(SeedListing {
                source_url: Some("https://npr.org/c/1".to_string()),
                title: Some("A".to_string()),
                concert_date: Some("2024-06-01".to_string()),
                teaser: Some("Great show".to_string()),
            })
            .unwrap();
        seeds
            .seed_listing(SeedListing {
                source_url: Some("https://npr.org/c/2".to_string()),
                title: Some("B".to_string()),
                concert_date: Some("2020-01-15".to_string()),
                teaser: None,
            })
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
}
