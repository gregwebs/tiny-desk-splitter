//! Database Seed API — test-only fixture creation for both Rust tests and the
//! Test Control API (`crate::test_control`). Compiled for `cfg(test)` builds
//! and `test-control` builds so plain non-test-control release builds never
//! see this module at all.
//!
//! [`SeedContext`] pairs a `&Connection` with a [`FixtureIds`] allocator and
//! exposes one method per fixture shape (`seed_listing`,
//! `seed_scraped_concert`, `seed_lifecycle_concert`). Each seed input struct
//! implements `Default` and deserializes with struct-level
//! `#[serde(default)]`, so a missing JSON field takes the Rust default and an
//! explicit JSON `null` deserializes to `None` — these differ exactly when a
//! field's `Default` is `Some(...)` (see `concert_date`/`teaser` below), which
//! is what lets a seed call explicitly request a NULL domain value instead of
//! a generated one. See `docs/adr/0003-test-control-seed-defaults.md` and
//! `docs/change/2026-07-13-db-seed-api-design.md`.
//!
//! Seed methods compose the same domain persistence functions the product
//! code uses (`db::concerts`, `db::lifecycle`, `db::split_timestamps`), so
//! seeded fixtures emit the same events a real user action would. Direct SQL
//! is used only for fixture *normalization* — resolving `NewListing`'s
//! COALESCE-on-conflict semantics, and clearing stale lifecycle/timestamp
//! state on a reused `source_url` — where the equivalent domain functions
//! (`lifecycle::clear_download_state`/`clear_split_state`) would record
//! misleading delete events for state that was never really downloaded or
//! split.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Deserialize;

use crate::db::concerts::{self, MetadataUpdate, NewListing};
use crate::db::lifecycle;
use crate::db::split_timestamps;
use crate::model::Concert;

const DEFAULT_CONCERT_DATE: &str = "2026-01-01";

fn fixture_source_url(n: u64) -> String {
    format!("https://example.test/tiny-desk/test-control-{n}")
}

fn fixture_set_list(n: u64) -> Vec<String> {
    vec![
        format!("Test Control Track {n}.1"),
        format!("Test Control Track {n}.2"),
        format!("Test Control Track {n}.3"),
    ]
}

/// Cloneable handle to a monotonic fixture-number allocator, starting at `1`.
/// A [`SeedContext`] built with [`SeedContext::new`] gets a fresh allocator;
/// callers that need one allocator shared across many contexts (Test Control
/// holds one for the lifetime of the server process, per ADR 0003 — not reset
/// by `test.reset`) construct a `FixtureIds` once (via `FixtureIds::default()`)
/// and pass clones to [`SeedContext::with_ids`].
#[derive(Clone)]
pub struct FixtureIds(Arc<AtomicU64>);

impl Default for FixtureIds {
    fn default() -> Self {
        Self(Arc::new(AtomicU64::new(1)))
    }
}

impl FixtureIds {
    fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}

/// A test-only handle carrying the SQLite connection and fixture-id allocator
/// that every Database Seed API call needs.
pub struct SeedContext<'a> {
    conn: &'a Connection,
    ids: FixtureIds,
}

impl<'a> SeedContext<'a> {
    /// A `SeedContext` with its own fresh fixture-number allocator, starting
    /// at 1. Use this from Rust tests, where each test gets an isolated
    /// in-memory database and does not need cross-test uniqueness.
    pub fn new(conn: &'a Connection) -> Self {
        Self::with_ids(conn, FixtureIds::default())
    }

    /// A `SeedContext` sharing an existing allocator — for callers (Test
    /// Control) that need fixture numbers unique across many seed calls
    /// against the same long-lived connection/process.
    pub fn with_ids(conn: &'a Connection, ids: FixtureIds) -> Self {
        Self { conn, ids }
    }

    pub fn seed_listing(&self, seed: SeedListing) -> Result<Concert> {
        let n = self.ids.next();
        let source_url = seed.source_url.unwrap_or_else(|| fixture_source_url(n));
        let title = seed.title.unwrap_or_else(|| format!("Test Listing {n}"));
        // `concert_date`/`teaser` are already resolved by `SeedListing::default()`
        // (whose `Default` is `Some(...)`): a missing field became `Some(...)` via
        // `#[serde(default)]`, and an explicit JSON `null` became `None` — no
        // further defaulting needed here.
        let concert_date = seed.concert_date;
        let teaser = seed.teaser;

        let concert = upsert_and_fetch(
            self.conn,
            &NewListing {
                source_url,
                title,
                concert_date: concert_date.clone(),
                teaser: teaser.clone(),
            },
        )?;
        normalize_listing_fields(self.conn, concert.id, &concert_date, &teaser)?;
        concerts::get_concert(self.conn, concert.id)
    }

    pub fn seed_scraped_concert(&self, seed: SeedScrapedConcert) -> Result<Concert> {
        let n = self.ids.next();
        let source_url = seed.source_url.unwrap_or_else(|| fixture_source_url(n));
        let title = seed
            .title
            .unwrap_or_else(|| format!("Test Scraped Concert {n}"));
        let concert_date = seed.concert_date;
        let artist = seed
            .artist
            .unwrap_or_else(|| format!("Test Scraped Artist {n}"));
        let album = seed
            .album
            .unwrap_or_else(|| format!("Test Scraped Album {n}"));
        let set_list = seed.set_list.unwrap_or_else(|| fixture_set_list(n));

        let concert = upsert_and_fetch(
            self.conn,
            &NewListing {
                source_url,
                title,
                concert_date: concert_date.clone(),
                teaser: None,
            },
        )?;
        normalize_listing_fields(self.conn, concert.id, &concert_date, &None)?;
        reset_fixture_lifecycle_state(self.conn, concert.id)?;
        concerts::update_metadata(
            self.conn,
            concert.id,
            &MetadataUpdate {
                artist,
                album,
                description: None,
                set_list,
                musicians: vec![],
            },
        )?;
        concerts::get_concert(self.conn, concert.id)
    }

    pub fn seed_lifecycle_concert(&self, seed: SeedLifecycleConcert) -> Result<Concert> {
        let n = self.ids.next();
        let source_url = seed.source_url.unwrap_or_else(|| fixture_source_url(n));
        let title = seed
            .title
            .unwrap_or_else(|| format!("Test Lifecycle Concert {n}"));
        let concert_date = seed.concert_date;
        let artist = seed
            .artist
            .unwrap_or_else(|| format!("Test Lifecycle Artist {n}"));
        let album = seed
            .album
            .unwrap_or_else(|| format!("Test Lifecycle Album {n}"));
        let set_list = seed.set_list.unwrap_or_else(|| fixture_set_list(n));

        let concert = upsert_and_fetch(
            self.conn,
            &NewListing {
                source_url,
                title,
                concert_date: concert_date.clone(),
                teaser: None,
            },
        )?;
        normalize_listing_fields(self.conn, concert.id, &concert_date, &None)?;
        reset_fixture_lifecycle_state(self.conn, concert.id)?;
        concerts::update_metadata(
            self.conn,
            concert.id,
            &MetadataUpdate {
                artist,
                album,
                description: None,
                set_list,
                musicians: vec![],
            },
        )?;

        if seed.downloaded || seed.split {
            lifecycle::try_mark_download_started(self.conn, concert.id)?;
            lifecycle::mark_download_succeeded(self.conn, concert.id, "mp4")?;
        }
        if seed.split {
            lifecycle::try_mark_split_started(self.conn, concert.id)?;
            lifecycle::mark_split_succeeded(self.conn, concert.id)?;
        }
        if let Some(timestamps) = &seed.auto_timestamps {
            split_timestamps::set_auto_split_timestamps(self.conn, concert.id, timestamps)?;
        }
        if let Some(timestamps) = &seed.user_timestamps {
            split_timestamps::set_user_split_timestamps(self.conn, concert.id, timestamps)?;
        }
        if let Some(duration) = seed.media_duration {
            split_timestamps::set_media_duration(self.conn, concert.id, duration)?;
        }

        concerts::get_concert(self.conn, concert.id)
    }
}

/// Inserts (or updates, on a `source_url` collision) a listing through the
/// same `db::concerts::upsert_listing` path the real scraper uses. Looks the
/// row back up by `source_url` rather than trusting
/// `Connection::last_insert_rowid` — `upsert_listing` is an `INSERT ... ON
/// CONFLICT DO UPDATE`, and SQLite only advances `last_insert_rowid` for the
/// `INSERT` branch, so it would silently return a stale id whenever a seed
/// call reseeds an already-used `source_url`.
fn upsert_and_fetch(conn: &Connection, listing: &NewListing) -> Result<Concert> {
    concerts::upsert_listing(conn, listing)?;
    concerts::get_concert_by_url(conn, &listing.source_url)?.ok_or_else(|| {
        anyhow::anyhow!(
            "upsert_listing succeeded but the row is not readable back by source_url: {}",
            listing.source_url
        )
    })
}

/// `upsert_listing`'s `ON CONFLICT` branch uses `COALESCE(excluded.x,
/// concerts.x)` for `concert_date`/`teaser`, so a seed's resolved `None`
/// value would silently preserve whatever a prior seed left in place on a
/// reused `source_url` instead of clearing it. This direct UPDATE writes the
/// already-resolved values so every seed call ends with exactly the fields it
/// resolved, regardless of prior state at that `source_url`.
///
/// Conditional on the values actually changing: `concerts` has an `AFTER
/// UPDATE` trigger (migration 0003) that bumps `updated_at` for any ordinary
/// update, and this fixture-normalization write must not move `updated_at`
/// when it is a no-op — `db::tests::seed` relies on producing the exact same
/// row byte-for-byte across repeated calls.
fn normalize_listing_fields(
    conn: &Connection,
    id: i64,
    concert_date: &Option<String>,
    teaser: &Option<String>,
) -> Result<()> {
    conn.execute(
        "UPDATE concerts SET concert_date = ?1, teaser = ?2
         WHERE id = ?3 AND (concert_date IS NOT ?1 OR teaser IS NOT ?2)",
        params![concert_date, teaser, id],
    )?;
    Ok(())
}

/// Resets download/split/archive/timestamp/media-duration state to inert
/// defaults so seeding a reused `source_url` never leaves state from a prior
/// seed call behind. Uses a direct `UPDATE`, not `lifecycle::clear_download_state`
/// / `clear_split_state` (which are for genuine user-initiated deletes and each
/// record a `DownloadDelete`/`SplitDelete` event) — that would be a false audit
/// trail here, since nothing was ever actually downloaded or split by this
/// fixture. Conditional on the row not already being fully inert, for the same
/// `updated_at`-stability reason as [`normalize_listing_fields`].
fn reset_fixture_lifecycle_state(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE concerts SET
             download_started_at = NULL, downloaded_at = NULL,
             downloaded_extension = NULL, download_errors_json = '[]',
             split_started_at = NULL, split_at = NULL, split_errors_json = '[]',
             tracks_present = NULL, tracks_liked = NULL,
             archive_started_at = NULL, archived_at = NULL, archive_errors_json = '[]',
             auto_split_timestamps_json = NULL, user_split_timestamps_json = NULL,
             media_duration = NULL
         WHERE id = ?1 AND (
             download_started_at IS NOT NULL OR downloaded_at IS NOT NULL OR
             downloaded_extension IS NOT NULL OR download_errors_json IS NOT '[]' OR
             split_started_at IS NOT NULL OR split_at IS NOT NULL OR
             split_errors_json IS NOT '[]' OR tracks_present IS NOT NULL OR
             tracks_liked IS NOT NULL OR
             archive_started_at IS NOT NULL OR archived_at IS NOT NULL OR
             archive_errors_json IS NOT '[]' OR auto_split_timestamps_json IS NOT NULL OR
             user_split_timestamps_json IS NOT NULL OR media_duration IS NOT NULL
         )",
        params![id],
    )?;
    Ok(())
}

/// Seed input for `SeedContext::seed_listing` / `test.seed_listing`. A
/// missing field takes the listed default; an explicit JSON `null` seeds
/// `None` — for `concert_date`/`teaser`, whose defaults are `Some(...)`,
/// that means an explicit `null` produces a real SQL NULL, distinct from
/// omitting the field.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SeedListing {
    pub source_url: Option<String>,
    pub title: Option<String>,
    pub concert_date: Option<String>,
    pub teaser: Option<String>,
}

impl Default for SeedListing {
    fn default() -> Self {
        Self {
            source_url: None,
            title: None,
            concert_date: Some(DEFAULT_CONCERT_DATE.to_string()),
            teaser: Some("Test listing teaser".to_string()),
        }
    }
}

/// Seed input for `SeedContext::seed_scraped_concert` /
/// `test.seed_scraped_concert`. `set_list: null` (or omitted) takes the
/// generated three-track default; pass `set_list: []` for an empty list.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SeedScrapedConcert {
    pub source_url: Option<String>,
    pub title: Option<String>,
    pub concert_date: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub set_list: Option<Vec<String>>,
}

impl Default for SeedScrapedConcert {
    fn default() -> Self {
        Self {
            source_url: None,
            title: None,
            concert_date: Some(DEFAULT_CONCERT_DATE.to_string()),
            artist: None,
            album: None,
            set_list: None,
        }
    }
}

/// Seed input for `SeedContext::seed_lifecycle_concert` /
/// `test.seed_lifecycle_concert`. `downloaded`/`split` default to `false`
/// (an inert fixture); `split: true` implies `downloaded: true` regardless of
/// the `downloaded` field, matching the real download-then-split lifecycle.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SeedLifecycleConcert {
    pub source_url: Option<String>,
    pub title: Option<String>,
    pub concert_date: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub set_list: Option<Vec<String>>,
    pub downloaded: bool,
    pub split: bool,
    pub auto_timestamps: Option<Vec<concert_types::SongTimestamp>>,
    pub user_timestamps: Option<Vec<concert_types::SongTimestamp>>,
    pub media_duration: Option<f64>,
}

impl Default for SeedLifecycleConcert {
    fn default() -> Self {
        Self {
            source_url: None,
            title: None,
            concert_date: Some(DEFAULT_CONCERT_DATE.to_string()),
            artist: None,
            album: None,
            set_list: None,
            downloaded: false,
            split: false,
            auto_timestamps: None,
            user_timestamps: None,
            media_duration: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::connection::open_in_memory;
    use crate::db::tests::events_for;

    fn ctx(conn: &Connection) -> SeedContext<'_> {
        SeedContext::new(conn)
    }

    #[test]
    fn seed_listing_defaults_are_unique_across_calls() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let a = seeds.seed_listing(SeedListing::default()).unwrap();
        let b = seeds.seed_listing(SeedListing::default()).unwrap();
        assert_ne!(a.source_url, b.source_url);
        assert_ne!(a.title, b.title);
        assert_eq!(a.concert_date.as_deref(), Some(DEFAULT_CONCERT_DATE));
        assert_eq!(a.teaser.as_deref(), Some("Test listing teaser"));
    }

    #[test]
    fn seed_listing_explicit_fields_override_defaults() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let concert = seeds
            .seed_listing(SeedListing {
                source_url: Some("https://npr.org/c/explicit".to_string()),
                title: Some("Explicit Title".to_string()),
                concert_date: Some("2024-01-01".to_string()),
                teaser: Some("Explicit teaser".to_string()),
            })
            .unwrap();
        assert_eq!(concert.source_url, "https://npr.org/c/explicit");
        assert_eq!(concert.title, "Explicit Title");
        assert_eq!(concert.concert_date.as_deref(), Some("2024-01-01"));
        assert_eq!(concert.teaser.as_deref(), Some("Explicit teaser"));
    }

    #[test]
    fn seed_listing_null_seeds_none_on_fresh_row() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let concert = seeds
            .seed_listing(SeedListing {
                concert_date: None,
                teaser: None,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(concert.concert_date, None);
        assert_eq!(concert.teaser, None);
    }

    #[test]
    fn seed_listing_reseed_keeps_id_stable() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let url = "https://npr.org/c/reseed".to_string();
        let first = seeds
            .seed_listing(SeedListing {
                source_url: Some(url.clone()),
                ..Default::default()
            })
            .unwrap();
        let second = seeds
            .seed_listing(SeedListing {
                source_url: Some(url),
                title: Some("Updated Title".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(second.title, "Updated Title");
    }

    #[test]
    fn seed_listing_reseed_with_null_clears_previously_set_fields() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let url = "https://npr.org/c/clear".to_string();
        seeds
            .seed_listing(SeedListing {
                source_url: Some(url.clone()),
                concert_date: Some("2024-05-01".to_string()),
                teaser: Some("Has a teaser".to_string()),
                ..Default::default()
            })
            .unwrap();
        let reseeded = seeds
            .seed_listing(SeedListing {
                source_url: Some(url),
                concert_date: None,
                teaser: None,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            reseeded.concert_date, None,
            "explicit null on reseed must clear the previously-set concert_date, \
             not preserve it via upsert_listing's COALESCE"
        );
        assert_eq!(
            reseeded.teaser, None,
            "explicit null on reseed must clear the previously-set teaser"
        );
    }

    #[test]
    fn seed_scraped_concert_defaults_set_list_and_resets_teaser() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let concert = seeds
            .seed_scraped_concert(SeedScrapedConcert::default())
            .unwrap();
        assert_eq!(concert.set_list.len(), 3);
        assert_eq!(concert.teaser, None);
        assert!(concert.artist.is_some());
        assert!(concert.album.is_some());
        assert!(concert.metadata_scraped_at.is_some());
    }

    #[test]
    fn seed_scraped_concert_empty_set_list_is_explicit() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let concert = seeds
            .seed_scraped_concert(SeedScrapedConcert {
                set_list: Some(vec![]),
                ..Default::default()
            })
            .unwrap();
        assert!(concert.set_list.is_empty());
    }

    #[test]
    fn seed_scraped_concert_resets_stale_lifecycle_state_without_delete_events() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let url = "https://npr.org/c/stale-scrape".to_string();
        let downloaded = seeds
            .seed_lifecycle_concert(SeedLifecycleConcert {
                source_url: Some(url.clone()),
                split: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(downloaded.split_status(), crate::model::SplitStatus::Split);

        let rescraped = seeds
            .seed_scraped_concert(SeedScrapedConcert {
                source_url: Some(url),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            rescraped.download_status(),
            crate::model::DownloadStatus::NotDownloaded
        );
        assert_eq!(
            rescraped.split_status(),
            crate::model::SplitStatus::NotSplit
        );

        let events = events_for(&conn, rescraped.id);
        assert!(
            !events.iter().any(|(event, _)| event == "download_delete"
                || event == "split_delete"),
            "reset must not emit delete events for state that was never a real user delete: {events:?}"
        );
    }

    #[test]
    fn seed_lifecycle_concert_downloaded_and_split_marks() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let concert = seeds
            .seed_lifecycle_concert(SeedLifecycleConcert {
                split: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            concert.download_status(),
            crate::model::DownloadStatus::Downloaded
        );
        assert_eq!(concert.split_status(), crate::model::SplitStatus::Split);
    }

    #[test]
    fn seed_lifecycle_concert_reuse_resets_to_inert_defaults() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let url = "https://npr.org/c/lifecycle-reuse".to_string();
        let first = seeds
            .seed_lifecycle_concert(SeedLifecycleConcert {
                source_url: Some(url.clone()),
                split: true,
                media_duration: Some(120.0),
                auto_timestamps: Some(vec![]),
                ..Default::default()
            })
            .unwrap();
        // Liked-track state comes from a real product action (not a seed
        // param), so it can go stale on a reused source_url the same way
        // download/split/timestamp state can.
        split_timestamps::set_tracks_liked(&conn, first.id, &[true, false]).unwrap();

        let reseeded = seeds
            .seed_lifecycle_concert(SeedLifecycleConcert {
                source_url: Some(url),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            reseeded.download_status(),
            crate::model::DownloadStatus::NotDownloaded,
            "reused source_url must reset to inert defaults, not keep prior downloaded state"
        );
        assert_eq!(reseeded.split_status(), crate::model::SplitStatus::NotSplit);
        assert_eq!(reseeded.media_duration, None);
        assert!(
            reseeded.tracks_liked.is_empty(),
            "reused source_url must not keep a prior seed's liked-track state"
        );
        let stored = split_timestamps::get_split_timestamps(&conn, reseeded.id).unwrap();
        assert_eq!(stored.auto, None);
        assert_eq!(stored.user, None);
    }

    #[test]
    fn seed_lifecycle_concert_timestamps_and_media_duration() {
        let conn = open_in_memory().unwrap();
        let seeds = ctx(&conn);
        let ts = concert_types::SongTimestamp {
            title: "Song One".to_string(),
            start_time: 0.0,
            end_time: 10.0,
            duration: 10.0,
        };
        let concert = seeds
            .seed_lifecycle_concert(SeedLifecycleConcert {
                auto_timestamps: Some(vec![ts.clone()]),
                user_timestamps: Some(vec![ts]),
                media_duration: Some(42.0),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(concert.media_duration, Some(42.0));
        let stored = split_timestamps::get_split_timestamps(&conn, concert.id).unwrap();
        assert_eq!(stored.auto.map(|v| v.len()), Some(1));
        assert_eq!(stored.user.map(|v| v.len()), Some(1));
    }

    #[test]
    fn json_omitted_field_uses_default_and_explicit_null_uses_none() {
        let listing: SeedListing = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(listing.concert_date.as_deref(), Some(DEFAULT_CONCERT_DATE));
        assert_eq!(listing.teaser.as_deref(), Some("Test listing teaser"));

        let listing: SeedListing =
            serde_json::from_value(serde_json::json!({"concert_date": null, "teaser": null}))
                .unwrap();
        assert_eq!(listing.concert_date, None);
        assert_eq!(listing.teaser, None);
    }

    #[test]
    fn json_rejects_unknown_fields() {
        let result: Result<SeedListing, _> =
            serde_json::from_value(serde_json::json!({"bogus_field": 1}));
        assert!(result.is_err());
    }

    #[test]
    fn fixture_ids_shared_across_contexts_stay_unique() {
        let conn = open_in_memory().unwrap();
        let ids = FixtureIds::default();
        let a = SeedContext::with_ids(&conn, ids.clone())
            .seed_listing(SeedListing::default())
            .unwrap();
        let b = SeedContext::with_ids(&conn, ids)
            .seed_listing(SeedListing::default())
            .unwrap();
        assert_ne!(a.source_url, b.source_url);
    }
}
