use std::collections::HashSet;

use anyhow::{Context, Result};
use chrono::{Datelike, Month, Utc};
use rusqlite::Connection;
use tiny_desk_scraper::{fetch_archive_month, ConcertListing};

use crate::db;
use crate::db::concerts::NewListing;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct YearMonth {
    pub year: i32,
    pub month: u32,
}

impl YearMonth {
    pub fn current() -> Self {
        let now = Utc::now();
        YearMonth {
            year: now.year(),
            month: now.month(),
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() != 2 {
            return Err(anyhow::anyhow!("Expected YYYY-MM format, got: {}", s));
        }
        Ok(YearMonth {
            year: parts[0].parse().context("Invalid year")?,
            month: parts[1].parse().context("Invalid month")?,
        })
    }

    pub fn from_date_str(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.get(..7)?.split('-').collect();
        if parts.len() != 2 {
            return None;
        }
        Some(YearMonth {
            year: parts[0].parse().ok()?,
            month: parts[1].parse().ok()?,
        })
    }

    pub fn display_label(&self) -> String {
        let month_name = Month::try_from(self.month as u8)
            .map(|m| m.name())
            .unwrap_or("Unknown");
        format!("{} {}", month_name, self.year)
    }

    pub fn previous(&self) -> Self {
        if self.month == 1 {
            YearMonth {
                year: self.year - 1,
                month: 12,
            }
        } else {
            YearMonth {
                year: self.year,
                month: self.month - 1,
            }
        }
    }

    pub fn next(&self) -> Self {
        if self.month == 12 {
            YearMonth {
                year: self.year + 1,
                month: 1,
            }
        } else {
            YearMonth {
                year: self.year,
                month: self.month + 1,
            }
        }
    }
}

/// Build the set of fully synced months from the database (months synced only
/// while still in progress are excluded; see `db::sync::list_fully_synced_months`).
pub fn synced_months_set(conn: &Connection) -> Result<HashSet<YearMonth>> {
    let pairs = db::sync::list_fully_synced_months(conn)?;
    Ok(pairs
        .into_iter()
        .map(|(y, m)| YearMonth { year: y, month: m })
        .collect())
}

/// A concert a sync wants scraped: either newly imported this run, or already
/// present but not yet successfully scraped (a retry). Carries the bits the sync
/// handler needs — DB id, source URL, and scrape state — so it can enqueue any
/// that still lack metadata without a second full-table query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncedConcert {
    pub id: i64,
    pub source_url: String,
    pub metadata_scraped_at: Option<String>,
}

/// Result of scoping a cumulative archive fetch to a single month.
#[derive(Debug, Default)]
pub struct MonthPartition {
    /// Listings dated to the requested month, plus any undated listings (kept so a
    /// possible NPR HTML format change doesn't silently drop concerts).
    pub kept: Vec<ConcertListing>,
    /// Count of kept listings with no parseable date. A missing `<time datetime>`
    /// signals NPR changed their archive HTML, so the caller logs an error.
    pub undated: usize,
}

/// Scope a cumulative archive fetch to `ym`: keep listings dated to that month,
/// drop listings from other months (NPR's `?date=` archive is cumulative — it
/// returns every concert up to that date), and keep + count undated listings.
/// Pure: no DB, no network.
pub fn listings_for_month(listings: &[ConcertListing], ym: &YearMonth) -> MonthPartition {
    let mut part = MonthPartition::default();
    for listing in listings {
        match YearMonth::from_date_str(&listing.date) {
            Some(m) if &m == ym => part.kept.push(listing.clone()),
            Some(_) => {} // other month: cumulative-archive bleed — drop it
            None => {
                part.undated += 1;
                part.kept.push(listing.clone());
            }
        }
    }
    part
}

/// Fetch the archive for `ym`, scope it to that month, and import idempotently:
/// brand-new concerts are inserted; existing-but-unscraped concerts are returned
/// for a scrape retry without touching their listing fields; existing+scraped
/// concerts are left completely alone. Returns the concerts that still need a
/// per-concert metadata scrape. The single archive fetch happens under whatever
/// lock the caller holds; the per-concert scrape must happen outside it.
pub fn sync_month(conn: &Connection, ym: &YearMonth) -> Result<Vec<SyncedConcert>> {
    let all = fetch_archive_month(ym.year, ym.month, None)
        .with_context(|| format!("Failed to fetch archive for {}/{:02}", ym.year, ym.month))?;
    let MonthPartition { kept, undated } = listings_for_month(&all, ym);
    if undated > 0 {
        let samples: Vec<&str> = kept
            .iter()
            .filter(|l| YearMonth::from_date_str(&l.date).is_none())
            .take(3)
            .map(|l| l.url.as_str())
            .collect();
        tracing::error!(
            "sync {}/{:02}: {} archive listing(s) had no parseable date — NPR archive HTML format may have changed (e.g. {:?})",
            ym.year, ym.month, undated, samples
        );
    }

    let synced = import_listings(conn, &kept)?;
    db::sync::mark_month_synced(conn, ym.year, ym.month)?;
    Ok(synced)
}

/// Import month-scoped listings idempotently, returning the concerts that still
/// need a metadata scrape (new imports + unscraped retries). Existing,
/// already-scraped concerts are left completely untouched.
///
/// NOTE: the per-listing check-then-act is NOT atomic at the DB level; it relies
/// on the caller holding the single global `Mutex<Connection>` (see
/// `web::handlers::sync_month_handler`), so all writers serialize and there is no
/// TOCTOU. If sync ever moves off that lock (or to a connection pool), switch to
/// `upsert_listing`'s `is_new` return instead.
fn import_listings(conn: &Connection, kept: &[ConcertListing]) -> Result<Vec<SyncedConcert>> {
    let mut synced = Vec::new();
    for listing in kept {
        match db::concerts::get_concert_by_url(conn, &listing.url)? {
            // Already fully scraped: leave it completely untouched.
            Some(c) if c.metadata_scraped_at.is_some() => continue,
            // Present but not (successfully) scraped: don't overwrite its listing
            // fields — just return it so the failed/half-done scrape is retried.
            Some(c) => synced.push(SyncedConcert {
                id: c.id,
                source_url: c.source_url,
                metadata_scraped_at: c.metadata_scraped_at,
            }),
            // Brand new: insert (records an `Import` event), then read back for its id.
            None => {
                db::concerts::upsert_listing(
                    conn,
                    &NewListing {
                        source_url: listing.url.clone(),
                        title: listing.title.clone(),
                        concert_date: (!listing.date.is_empty()).then(|| listing.date.clone()),
                        teaser: (!listing.teaser.is_empty()).then(|| listing.teaser.clone()),
                    },
                )?;
                if let Some(c) = db::concerts::get_concert_by_url(conn, &listing.url)? {
                    synced.push(SyncedConcert {
                        id: c.id,
                        source_url: c.source_url,
                        metadata_scraped_at: c.metadata_scraped_at,
                    });
                }
            }
        }
    }
    Ok(synced)
}

/// Sync a range of months (inclusive on both ends). Returns the total number of
/// concerts that needed a scrape across the range (newly imported + retried),
/// i.e. the sum of each month's `sync_month` result length.
pub fn sync_months(conn: &Connection, from: YearMonth, to: YearMonth) -> Result<usize> {
    let mut total = 0;
    let mut current = from;
    loop {
        let synced = sync_month(conn, &current)?;
        total += synced.len();
        if current.year == to.year && current.month == to.month {
            break;
        }
        current = current.next();
    }
    Ok(total)
}

/// `(id, url)` of just-synced concerts that still need a per-concert metadata
/// scrape (so their preview image + listing thumbnail get generated). Pure
/// filter over the sync result — no DB, no network.
pub fn concerts_needing_scrape(synced: &[SyncedConcert]) -> Vec<(i64, String)> {
    synced
        .iter()
        .filter(|c| c.metadata_scraped_at.is_none())
        .map(|c| (c.id, c.source_url.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn listing(url: &str, title: &str, date: &str) -> ConcertListing {
        ConcertListing {
            title: title.to_string(),
            url: url.to_string(),
            date: date.to_string(),
            teaser: "Some teaser".to_string(),
        }
    }

    #[test]
    fn year_month_parse_valid() {
        let ym = YearMonth::parse("2024-03").unwrap();
        assert_eq!(ym.year, 2024);
        assert_eq!(ym.month, 3);
    }

    #[test]
    fn year_month_parse_rejects_invalid_format() {
        assert!(YearMonth::parse("202403").is_err());
        assert!(YearMonth::parse("2024-3-1").is_err());
    }

    #[test]
    fn year_month_previous_wraps_january_to_december() {
        let ym = YearMonth {
            year: 2024,
            month: 1,
        };
        let prev = ym.previous();
        assert_eq!(prev.year, 2023);
        assert_eq!(prev.month, 12);
    }

    #[test]
    fn year_month_next_wraps_december_to_january() {
        let ym = YearMonth {
            year: 2024,
            month: 12,
        };
        let next = ym.next();
        assert_eq!(next.year, 2025);
        assert_eq!(next.month, 1);
    }

    #[test]
    fn year_month_previous_mid_year() {
        let ym = YearMonth {
            year: 2024,
            month: 6,
        };
        let prev = ym.previous();
        assert_eq!(prev.year, 2024);
        assert_eq!(prev.month, 5);
    }

    #[test]
    fn year_month_from_date_str_iso_timestamp() {
        let ym = YearMonth::from_date_str("2026-05-22T05:00:00-04:00").unwrap();
        assert_eq!(ym.year, 2026);
        assert_eq!(ym.month, 5);
    }

    #[test]
    fn year_month_from_date_str_date_only() {
        let ym = YearMonth::from_date_str("2025-11-13").unwrap();
        assert_eq!(ym.year, 2025);
        assert_eq!(ym.month, 11);
    }

    #[test]
    fn year_month_from_date_str_too_short() {
        assert!(YearMonth::from_date_str("2026").is_none());
    }

    #[test]
    fn year_month_display_label() {
        let ym = YearMonth {
            year: 2026,
            month: 5,
        };
        assert_eq!(ym.display_label(), "May 2026");
    }

    #[test]
    fn year_month_display_label_january() {
        let ym = YearMonth {
            year: 2025,
            month: 1,
        };
        assert_eq!(ym.display_label(), "January 2025");
    }

    #[test]
    fn year_month_equality_and_hash() {
        let a = YearMonth {
            year: 2026,
            month: 5,
        };
        let b = YearMonth {
            year: 2026,
            month: 5,
        };
        let c = YearMonth {
            year: 2026,
            month: 4,
        };
        assert_eq!(a, b);
        assert_ne!(a, c);

        let mut set = std::collections::HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
        assert!(!set.contains(&c));
    }

    fn synced(id: i64, url: &str, scraped: bool) -> SyncedConcert {
        SyncedConcert {
            id,
            source_url: url.to_string(),
            metadata_scraped_at: scraped.then(|| "2026-05-30T00:00:00Z".to_string()),
        }
    }

    #[test]
    fn concerts_needing_scrape_keeps_only_unscraped() {
        let synced = vec![
            synced(1, "https://npr.org/c/1", false),
            synced(2, "https://npr.org/c/2", true),
            synced(3, "https://npr.org/c/3", false),
        ];
        let needing = concerts_needing_scrape(&synced);
        assert_eq!(
            needing,
            vec![
                (1, "https://npr.org/c/1".to_string()),
                (3, "https://npr.org/c/3".to_string()),
            ]
        );
    }

    #[test]
    fn concerts_needing_scrape_empty_when_all_scraped() {
        let synced = vec![
            synced(1, "https://npr.org/c/1", true),
            synced(2, "https://npr.org/c/2", true),
        ];
        assert!(concerts_needing_scrape(&synced).is_empty());
    }

    #[test]
    fn concerts_needing_scrape_empty_input() {
        assert!(concerts_needing_scrape(&[]).is_empty());
    }

    // --- month scoping ------------------------------------------------------

    const MAY: YearMonth = YearMonth {
        year: 2026,
        month: 5,
    };

    #[test]
    fn listings_for_month_keeps_requested_month_drops_others() {
        let listings = vec![
            listing("https://npr.org/c/may", "May", "2026-05-20T05:00:00-04:00"),
            listing("https://npr.org/c/apr", "Apr", "2026-04-30T05:00:00-04:00"),
            listing("https://npr.org/c/jun", "Jun", "2026-06-01T05:00:00-04:00"),
        ];
        let part = listings_for_month(&listings, &MAY);
        let urls: Vec<&str> = part.kept.iter().map(|l| l.url.as_str()).collect();
        assert_eq!(urls, vec!["https://npr.org/c/may"]);
        assert_eq!(part.undated, 0);
    }

    #[test]
    fn listings_for_month_keeps_and_counts_undated() {
        let listings = vec![
            listing("https://npr.org/c/may", "May", "2026-05-20"),
            listing("https://npr.org/c/empty", "NoDate", ""),
            listing("https://npr.org/c/bad", "BadDate", "not-a-date"),
        ];
        let part = listings_for_month(&listings, &MAY);
        assert_eq!(part.undated, 2);
        // requested-month listing + both undated ones are kept; the undated signal
        // is a possible NPR HTML format change, not a reason to silently drop.
        assert_eq!(part.kept.len(), 3);
    }

    // --- idempotent import --------------------------------------------------

    fn scrape(conn: &Connection, id: i64) {
        db::concerts::update_metadata(
            conn,
            id,
            &db::concerts::MetadataUpdate {
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                description: None,
                set_list: vec![],
                musicians: vec![],
            },
        )
        .unwrap();
    }

    #[test]
    fn import_listings_inserts_new_and_returns_it_for_scrape() {
        let conn = db::connection::open_in_memory().unwrap();
        let kept = vec![listing("https://npr.org/c/new", "New", "2026-05-20")];
        let synced = import_listings(&conn, &kept).unwrap();
        assert_eq!(synced.len(), 1);
        assert!(synced[0].metadata_scraped_at.is_none());
        assert_eq!(db::concerts::list_concerts(&conn).unwrap().len(), 1);
    }

    #[test]
    fn import_listings_inserts_undated_new_with_null_date_and_teaser() {
        let conn = db::connection::open_in_memory().unwrap();
        // An undated listing is kept by `listings_for_month` and inserted here;
        // empty date/teaser map to NULL (not the empty string).
        let kept = vec![ConcertListing {
            title: "No Date Concert".to_string(),
            url: "https://npr.org/c/nodate".to_string(),
            date: "".to_string(),
            teaser: "".to_string(),
        }];
        let synced = import_listings(&conn, &kept).unwrap();
        assert_eq!(synced.len(), 1);
        let c = db::concerts::get_concert_by_url(&conn, "https://npr.org/c/nodate")
            .unwrap()
            .unwrap();
        assert!(c.concert_date.is_none());
        assert!(c.teaser.is_none());
    }

    #[test]
    fn import_listings_skips_existing_scraped_without_overwriting() {
        let conn = db::connection::open_in_memory().unwrap();
        let url = "https://npr.org/c/done";
        // Seed an existing, already-scraped concert with a clean title.
        let id = db::seeds::SeedContext::new(&conn)
            .seed_listing(db::seeds::SeedListing {
                source_url: Some(url.to_string()),
                title: Some("Clean Title".to_string()),
                concert_date: Some("2026-05-20".to_string()),
                teaser: None,
            })
            .unwrap()
            .id;
        scrape(&conn, id);

        // A re-sync whose archive listing carries the raw title must NOT touch it.
        let kept = vec![listing(url, "Raw Title: Tiny Desk Concert", "2026-05-20")];
        let synced = import_listings(&conn, &kept).unwrap();

        assert!(synced.is_empty(), "scraped concert is not re-queued");
        let c = db::concerts::get_concert_by_url(&conn, url)
            .unwrap()
            .unwrap();
        assert_eq!(c.title, "Clean Title", "listing fields preserved");
        assert!(c.metadata_scraped_at.is_some());
    }

    #[test]
    fn import_listings_requeues_existing_unscraped_without_overwriting() {
        let conn = db::connection::open_in_memory().unwrap();
        let url = "https://npr.org/c/halfdone";
        let original_id = db::seeds::SeedContext::new(&conn)
            .seed_listing(db::seeds::SeedListing {
                source_url: Some(url.to_string()),
                title: Some("Original Title".to_string()),
                concert_date: Some("2026-05-20".to_string()),
                teaser: None,
            })
            .unwrap()
            .id;

        // Existing but never scraped (e.g. a prior NAS-write failure) → retried.
        let kept = vec![listing(url, "Raw Title: Tiny Desk Concert", "2026-05-20")];
        let synced = import_listings(&conn, &kept).unwrap();

        assert_eq!(synced.len(), 1);
        assert_eq!(
            synced[0].id, original_id,
            "same row, not a duplicate import"
        );
        assert!(synced[0].metadata_scraped_at.is_none());
        let c = db::concerts::get_concert_by_url(&conn, url)
            .unwrap()
            .unwrap();
        assert_eq!(
            c.title, "Original Title",
            "listing fields untouched on retry"
        );
        assert_eq!(db::concerts::list_concerts(&conn).unwrap().len(), 1);
    }
}
