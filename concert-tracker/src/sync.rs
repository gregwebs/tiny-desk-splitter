use std::collections::HashSet;

use anyhow::{Context, Result};
use chrono::{Datelike, Month, Utc};
use rusqlite::Connection;
use tiny_desk_scraper::{fetch_archive_month, ConcertListing};

use crate::db::{self, NewListing};

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

/// Build the set of synced months from the database.
pub fn synced_months_set(conn: &Connection) -> Result<HashSet<YearMonth>> {
    let pairs = db::list_synced_months(conn)?;
    Ok(pairs
        .into_iter()
        .map(|(y, m)| YearMonth { year: y, month: m })
        .collect())
}

/// A concert that was just upserted by a sync, carrying the bits the sync
/// handler needs to decide whether to scrape it: its DB id, its source URL, and
/// whether its per-concert metadata has already been scraped. Returning this
/// avoids a second full-table query (and `HashSet` round-trip) in the handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncedConcert {
    pub id: i64,
    pub source_url: String,
    pub metadata_scraped_at: Option<String>,
}

/// Fetch and upsert all concert listings for a given month, returning the
/// upserted concerts (id + url + scrape state) so the caller can scrape any that
/// still lack metadata. The single archive page fetch happens under whatever
/// lock the caller holds; the per-concert metadata scrape must happen outside it.
pub fn sync_month(conn: &Connection, ym: &YearMonth) -> Result<Vec<SyncedConcert>> {
    let listings = fetch_archive_month(ym.year, ym.month, None)
        .with_context(|| format!("Failed to fetch archive for {}/{:02}", ym.year, ym.month))?;
    upsert_listings(conn, &listings)?;
    db::mark_month_synced(conn, ym.year, ym.month)?;

    let mut synced = Vec::with_capacity(listings.len());
    for listing in &listings {
        if let Some(c) = db::get_concert_by_url(conn, &listing.url)? {
            synced.push(SyncedConcert {
                id: c.id,
                source_url: c.source_url,
                metadata_scraped_at: c.metadata_scraped_at,
            });
        }
    }
    Ok(synced)
}

/// Sync a range of months (inclusive on both ends). Returns the total number of
/// listings upserted across the range.
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

pub fn upsert_listings(conn: &Connection, listings: &[ConcertListing]) -> Result<usize> {
    for listing in listings {
        db::upsert_listing(
            conn,
            &NewListing {
                source_url: listing.url.clone(),
                title: listing.title.clone(),
                concert_date: if listing.date.is_empty() {
                    None
                } else {
                    Some(listing.date.clone())
                },
                teaser: if listing.teaser.is_empty() {
                    None
                } else {
                    Some(listing.teaser.clone())
                },
            },
        )?;
    }
    Ok(listings.len())
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

    #[test]
    fn upsert_listings_inserts_all() {
        let conn = db::open_in_memory().unwrap();
        let listings = vec![
            listing("https://npr.org/c/1", "Concert A", "2024-01-01"),
            listing("https://npr.org/c/2", "Concert B", "2024-02-01"),
        ];
        let count = upsert_listings(&conn, &listings).unwrap();
        assert_eq!(count, 2);
        assert_eq!(db::list_concerts(&conn).unwrap().len(), 2);
    }

    #[test]
    fn upsert_listings_treats_empty_date_as_none() {
        let conn = db::open_in_memory().unwrap();
        let listings = vec![ConcertListing {
            title: "No Date Concert".to_string(),
            url: "https://npr.org/c/nodate".to_string(),
            date: "".to_string(),
            teaser: "".to_string(),
        }];
        upsert_listings(&conn, &listings).unwrap();
        let c = db::get_concert_by_url(&conn, "https://npr.org/c/nodate")
            .unwrap()
            .unwrap();
        assert!(c.concert_date.is_none());
        assert!(c.teaser.is_none());
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

    #[test]
    fn upsert_listings_is_idempotent() {
        let conn = db::open_in_memory().unwrap();
        let listings = vec![listing("https://npr.org/c/1", "Concert A", "2024-01-01")];
        upsert_listings(&conn, &listings).unwrap();
        upsert_listings(&conn, &listings).unwrap();
        assert_eq!(db::list_concerts(&conn).unwrap().len(), 1);
    }
}
