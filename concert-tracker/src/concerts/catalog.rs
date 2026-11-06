//! Browsing the collection of concerts.
//!
//! `ConcertCatalog` is the single interface through which adapters browse the
//! collection. It returns canonical [`ConcertState`]s, so a listing card and a
//! detail card describe the same facts and cannot disagree about status or
//! available actions; grouping into months, terminal formatting, and HTML stay
//! adapter concerns.
//!
//! It is a separate module from [`Concerts`] deliberately: compound filters,
//! search, stable ordering, and infinite-scrolling cursor pagination are
//! planned for browsing the collection, and none of them exist yet. None of
//! those capabilities are modeled here — when they are specified they land
//! behind this interface without changing individual-concert commands.

use super::{archive_configured, ConcertQueryError, ConcertState, Concerts};
use crate::db;
use crate::model::{ArchiveStatus, Concert, DownloadStatus, SplitStatus};

/// Which concerts of the collection to browse. These are the existing filter
/// meanings, preserved as they are: a filter answers a browsing question; it is
/// not a compound query language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConcertFilter {
    /// Everything the user has not ignored, which is what the web listing shows
    /// by default.
    #[default]
    All,
    Wanted,
    Available,
    Ignored,
    Downloaded,
    Split,
    Archived,
    /// Every concert, ignored ones included. Only the CLI asks for this: its
    /// `all` has always meant "list everything", while the web listing's
    /// default deliberately hides what the user ignored.
    Everything,
}

impl ConcertFilter {
    /// Parse the filter slug used by the web listing's query parameter. An
    /// unrecognised slug browses everything the user has not ignored, which is
    /// what the listing has always done with one.
    pub fn from_slug(slug: &str) -> Self {
        match slug {
            "wanted" => Self::Wanted,
            "available" => Self::Available,
            "ignored" => Self::Ignored,
            "downloaded" => Self::Downloaded,
            "split" => Self::Split,
            "archived" => Self::Archived,
            _ => Self::All,
        }
    }

    /// Whether a concert belongs in this filter. Archived degrades to All when
    /// archiving is not configured, which is how the listing has always gated
    /// that filter: without an archive location nothing can be archived.
    pub fn matches(&self, state: &ConcertState) -> bool {
        let concert: &Concert = &state.concert;
        match self {
            Self::Wanted => !concert.ignored && concert.wanted,
            Self::Ignored => concert.ignored,
            Self::Available => !concert.ignored && !concert.wanted,
            Self::Downloaded => matches!(concert.download_status(), DownloadStatus::Downloaded),
            Self::Split => matches!(concert.split_status(), SplitStatus::Split),
            Self::Archived if state.archive_configured => {
                matches!(concert.archive_status(), ArchiveStatus::Archived)
            }
            Self::Archived | Self::All => !concert.ignored,
            Self::Everything => true,
        }
    }
}

/// The collection-browsing interface. Long-lived and shared by adapters.
#[derive(Clone)]
pub struct ConcertCatalog {
    concerts: Concerts,
}

impl ConcertCatalog {
    pub fn new(concerts: Concerts) -> Self {
        Self { concerts }
    }

    /// Browse the collection, applying one filter. Concerts come back in the
    /// collection's existing order (newest concert date first); ordering is not
    /// a catalog capability yet.
    ///
    /// The database is read once for the whole collection — including user
    /// split timestamps, which a per-concert query would repeat — and each
    /// concert is then observed exactly as `Concerts::get` observes it.
    pub fn browse(&self, filter: ConcertFilter) -> Result<Vec<ConcertState>, ConcertQueryError> {
        let (concerts, user_split_timestamps, archive_configured) = {
            let conn = self.concerts.lock_db()?;
            let concerts =
                db::concerts::list_concerts(&conn).map_err(ConcertQueryError::Operational)?;
            let user_split_timestamps = db::split_timestamps::list_user_split_timestamps(&conn)
                .map_err(ConcertQueryError::Operational)?;
            let archive_configured = archive_configured(&conn)?;
            (concerts, user_split_timestamps, archive_configured)
        };

        let loaded: Vec<super::LoadedConcert> = concerts
            .into_iter()
            .map(|concert| super::LoadedConcert {
                user_split_timestamps: user_split_timestamps.get(&concert.id).cloned(),
                concert,
            })
            .collect();

        let states: Vec<ConcertState> = loaded
            .into_iter()
            .map(|loaded| self.concerts.observe(loaded, archive_configured))
            .filter(|state| filter.matches(state))
            .collect();

        tracing::debug!(
            ?filter,
            count = states.len(),
            archive_configured,
            "browsed concert catalog"
        );
        Ok(states)
    }

    /// Whether archiving is configured at all. Adapters use this to decide
    /// whether to offer the archived filter; the filter itself also degrades to
    /// All without an archive location.
    pub fn archive_configured(&self) -> Result<bool, ConcertQueryError> {
        let conn = self.concerts.lock_db()?;
        archive_configured(&conn)
    }

    /// The collection facts a listing page needs beside the concerts themselves:
    /// which months have been fully synced, and the earliest concert date (the
    /// listing's lower scroll bound). Reading them here keeps the adapter out of
    /// the collection's tables.
    pub fn listing_context(&self) -> Result<ListingContext, ConcertQueryError> {
        let conn = self.concerts.lock_db()?;
        Ok(ListingContext {
            synced_months: crate::sync::synced_months_set(&conn)
                .map_err(ConcertQueryError::Operational)?,
            earliest_date: db::sync::earliest_concert_date(&conn)
                .map_err(ConcertQueryError::Operational)?,
        })
    }
}

/// The listing page's collection-level context. See
/// [`ConcertCatalog::listing_context`].
#[derive(Debug, Clone, Default)]
pub struct ListingContext {
    /// Months whose archive page has been fully imported.
    pub synced_months: std::collections::HashSet<crate::sync::YearMonth>,
    /// The earliest concert date in the collection, if any.
    pub earliest_date: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::seeds::{SeedContext, SeedLifecycleConcert, SeedMediaConcert};
    use std::sync::{Arc, Mutex};

    fn catalog_for(conn: rusqlite::Connection, working_dir: &std::path::Path) -> ConcertCatalog {
        Concerts::without_workers(Arc::new(Mutex::new(conn)), working_dir.to_path_buf()).catalog()
    }

    fn browsing_fixture(
        working_dir: &std::path::Path,
    ) -> (rusqlite::Connection, Vec<(String, i64)>) {
        let conn = db::connection::open_in_memory().unwrap();
        let seeds = SeedContext::new(&conn);
        let available = seeds
            .seed_listing(db::seeds::SeedListing::default())
            .unwrap();
        let wanted = seeds
            .seed_listing(db::seeds::SeedListing::default())
            .unwrap();
        let ignored = seeds
            .seed_listing(db::seeds::SeedListing::default())
            .unwrap();
        let downloaded = seeds
            .seed_media_concert(
                working_dir,
                SeedMediaConcert {
                    lifecycle: SeedLifecycleConcert {
                        downloaded: true,
                        ..SeedLifecycleConcert::default()
                    },
                    source_file: true,
                    ..SeedMediaConcert::default()
                },
            )
            .unwrap();
        let split = seeds
            .seed_media_concert(
                working_dir,
                SeedMediaConcert {
                    lifecycle: SeedLifecycleConcert {
                        downloaded: true,
                        split: true,
                        ..SeedLifecycleConcert::default()
                    },
                    source_file: true,
                    ..SeedMediaConcert::default()
                },
            )
            .unwrap();

        db::concerts::set_concert_status(&conn, wanted.id, crate::model::ConcertStatus::Wanted)
            .unwrap();
        db::concerts::set_concert_status(&conn, ignored.id, crate::model::ConcertStatus::Ignored)
            .unwrap();
        db::settings::update_archive_location(&conn, "/archive").unwrap();

        (
            conn,
            vec![
                ("available".to_string(), available.id),
                ("wanted".to_string(), wanted.id),
                ("ignored".to_string(), ignored.id),
                ("downloaded".to_string(), downloaded.id),
                ("split".to_string(), split.id),
            ],
        )
    }

    /// Concerts sharing a concert date have no defined order, so browsing
    /// assertions compare identity, not sequence.
    fn ids(states: &[ConcertState]) -> Vec<i64> {
        let mut ids: Vec<i64> = states.iter().map(|state| state.concert.id).collect();
        ids.sort_unstable();
        ids
    }

    fn sorted(mut ids: Vec<i64>) -> Vec<i64> {
        ids.sort_unstable();
        ids
    }

    #[test]
    fn browse_returns_canonical_state_for_every_matching_concert() {
        let working_dir = tempfile::tempdir().unwrap();
        let (conn, fixtures) = browsing_fixture(working_dir.path());
        let catalog = catalog_for(conn, working_dir.path());

        let all = catalog.browse(ConcertFilter::All).unwrap();

        // All means everything the user has not ignored.
        let expected = sorted(
            fixtures
                .iter()
                .filter(|(status, _)| status != "ignored")
                .map(|(_, id)| *id)
                .collect(),
        );
        assert_eq!(ids(&all), expected);

        // Each entry is a full Concert State, so a listing card carries the same
        // action policy the detail card does.
        let downloaded = all
            .iter()
            .find(|state| state.concert.id == fixtures[3].1)
            .unwrap();
        assert!(downloaded.permitted_actions.can_delete_download);
        assert!(downloaded.permitted_actions.can_archive);
        assert!(matches!(
            downloaded.media.source,
            super::super::Observation::Known(super::super::SourceMediaState { path: Some(_) })
        ));
    }

    #[test]
    fn browse_applies_each_existing_filter_meaning() {
        let working_dir = tempfile::tempdir().unwrap();
        let (conn, fixtures) = browsing_fixture(working_dir.path());
        let id_of = |status: &str| {
            fixtures
                .iter()
                .find(|(name, _)| name == status)
                .map(|(_, id)| *id)
                .unwrap()
        };
        let catalog = catalog_for(conn, working_dir.path());

        assert_eq!(
            ids(&catalog.browse(ConcertFilter::Wanted).unwrap()),
            vec![id_of("wanted")]
        );
        // Available is the absence of both Wanted and Ignored: a downloaded or
        // split concert is still Available until the user says otherwise.
        assert_eq!(
            ids(&catalog.browse(ConcertFilter::Available).unwrap()),
            sorted(vec![
                id_of("available"),
                id_of("downloaded"),
                id_of("split")
            ])
        );
        assert_eq!(
            ids(&catalog.browse(ConcertFilter::Ignored).unwrap()),
            vec![id_of("ignored")]
        );
        assert_eq!(
            ids(&catalog.browse(ConcertFilter::Downloaded).unwrap()),
            sorted(vec![id_of("downloaded"), id_of("split")])
        );
        assert_eq!(
            ids(&catalog.browse(ConcertFilter::Split).unwrap()),
            vec![id_of("split")]
        );
    }

    #[test]
    fn archived_filter_degrades_to_all_without_an_archive_location() {
        let working_dir = tempfile::tempdir().unwrap();
        let (conn, fixtures) = browsing_fixture(working_dir.path());
        conn.execute("UPDATE settings SET archive_location = NULL", [])
            .unwrap();
        let catalog = catalog_for(conn, working_dir.path());

        assert!(!catalog.archive_configured().unwrap());
        let archived = catalog.browse(ConcertFilter::Archived).unwrap();
        assert_eq!(
            ids(&archived),
            sorted(
                fixtures
                    .iter()
                    .filter(|(status, _)| status != "ignored")
                    .map(|(_, id)| *id)
                    .collect()
            )
        );
    }

    #[test]
    fn archived_filter_selects_only_archived_concerts_when_configured() {
        let working_dir = tempfile::tempdir().unwrap();
        let (conn, _) = browsing_fixture(working_dir.path());
        let archived = SeedContext::new(&conn)
            .seed_media_concert(
                working_dir.path(),
                SeedMediaConcert {
                    lifecycle: SeedLifecycleConcert {
                        downloaded: true,
                        ..SeedLifecycleConcert::default()
                    },
                    source_file: true,
                    ..SeedMediaConcert::default()
                },
            )
            .unwrap();
        db::lifecycle::try_mark_archive_started(&conn, archived.id).unwrap();
        db::lifecycle::mark_archive_succeeded(&conn, archived.id).unwrap();
        let catalog = catalog_for(conn, working_dir.path());

        assert!(catalog.archive_configured().unwrap());
        assert_eq!(
            ids(&catalog.browse(ConcertFilter::Archived).unwrap()),
            vec![archived.id]
        );
    }

    #[test]
    fn everything_includes_ignored_concerts() {
        let working_dir = tempfile::tempdir().unwrap();
        let (conn, fixtures) = browsing_fixture(working_dir.path());
        let catalog = catalog_for(conn, working_dir.path());

        assert_eq!(
            ids(&catalog.browse(ConcertFilter::Everything).unwrap()),
            sorted(fixtures.iter().map(|(_, id)| *id).collect())
        );
    }

    #[test]
    fn filter_slugs_match_the_listing_and_cli_vocabulary() {
        assert_eq!(ConcertFilter::from_slug(""), ConcertFilter::All);
        assert_eq!(ConcertFilter::from_slug("all"), ConcertFilter::All);
        assert_eq!(ConcertFilter::from_slug("unknown"), ConcertFilter::All);
        assert_eq!(ConcertFilter::from_slug("wanted"), ConcertFilter::Wanted);
        assert_eq!(
            ConcertFilter::from_slug("available"),
            ConcertFilter::Available
        );
        assert_eq!(ConcertFilter::from_slug("ignored"), ConcertFilter::Ignored);
        assert_eq!(
            ConcertFilter::from_slug("downloaded"),
            ConcertFilter::Downloaded
        );
        assert_eq!(ConcertFilter::from_slug("split"), ConcertFilter::Split);
        assert_eq!(
            ConcertFilter::from_slug("archived"),
            ConcertFilter::Archived
        );
    }

    #[test]
    fn browsing_reports_missing_or_broken_persistence_as_operational_errors() {
        let working_dir = tempfile::tempdir().unwrap();
        let (conn, _) = browsing_fixture(working_dir.path());
        conn.execute_batch("DROP TABLE concerts").unwrap();
        let catalog = catalog_for(conn, working_dir.path());

        assert!(matches!(
            catalog.browse(ConcertFilter::All),
            Err(ConcertQueryError::Operational(_))
        ));
    }
}
