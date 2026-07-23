//! Application interface for observing and interacting with individual concerts.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Error;
use rusqlite::Connection;

use crate::concert_media::ConcertMediaInventory;
use crate::db;
use crate::jobs::scrape_queue::ScrapeQueue;
use crate::jobs::{JobKey, JobKind, JobRegistry};
use crate::model::{ArchiveStatus, Concert, DownloadStatus, PlaybackItem};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation<T> {
    Known(T),
    Unknown(ObservationFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationFailure {
    pub operation: MediaObservationOperation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaObservationOperation {
    ReadSourceDirectory,
    ReadPublishedSplit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConcertTrackState {
    pub index: usize,
    pub title: String,
    pub persisted_available: bool,
    pub liked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMediaState {
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct PublishedSplitState {
    pub tracks_present_on_disk: Vec<bool>,
    pub reconstruction_items: Vec<PlaybackItem>,
    pub source_redundant: bool,
}

#[derive(Debug, Clone)]
pub struct MediaAvailability {
    pub source: Observation<SourceMediaState>,
    pub published_split: Observation<PublishedSplitState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobActivity {
    pub persisted_started: bool,
    pub registry_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveWork {
    pub scrape_pending: bool,
    pub download: JobActivity,
    pub split: JobActivity,
    pub archive: JobActivity,
    pub split_queued_after_download: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermittedActions {
    pub can_download: bool,
    pub can_delete_download: bool,
    pub can_archive: bool,
    pub can_unarchive: bool,
    pub can_play_concert: bool,
    pub can_delete_redundant_source: bool,
    pub tracks_busy: bool,
}

#[derive(Debug, Clone)]
pub struct ConcertState {
    pub concert: Concert,
    pub tracks: Vec<ConcertTrackState>,
    pub media: MediaAvailability,
    pub active_work: ActiveWork,
    pub archive_configured: bool,
    pub permitted_actions: PermittedActions,
}

#[derive(Debug)]
pub enum ConcertQueryError {
    NotFound { concert_id: i64 },
    Operational(Error),
}

impl std::fmt::Display for ConcertQueryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { concert_id } => write!(formatter, "concert {concert_id} not found"),
            Self::Operational(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ConcertQueryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotFound { .. } => None,
            Self::Operational(error) => Some(error.as_ref()),
        }
    }
}

#[derive(Clone)]
pub struct Concerts {
    db: Arc<Mutex<Connection>>,
    working_dir: PathBuf,
    registry: Arc<JobRegistry>,
    scrape_queue: ScrapeQueue,
}

impl Concerts {
    pub fn new(
        db: Arc<Mutex<Connection>>,
        working_dir: PathBuf,
        registry: Arc<JobRegistry>,
        scrape_queue: ScrapeQueue,
    ) -> Self {
        Self {
            db,
            working_dir,
            registry,
            scrape_queue,
        }
    }

    pub fn get(&self, concert_id: i64) -> Result<ConcertState, ConcertQueryError> {
        let (concert, user_split_timestamps, archive_configured) = {
            let conn = self.lock_db()?;
            let concert = db::concerts::get_concert(&conn, concert_id).map_err(|error| {
                if matches!(
                    error.downcast_ref::<rusqlite::Error>(),
                    Some(rusqlite::Error::QueryReturnedNoRows)
                ) {
                    ConcertQueryError::NotFound { concert_id }
                } else {
                    ConcertQueryError::Operational(error)
                }
            })?;
            let user_split_timestamps =
                db::split_timestamps::get_split_timestamps(&conn, concert_id)
                    .map_err(ConcertQueryError::Operational)?
                    .user;
            let archive_configured = db::settings::get_settings(&conn)
                .map_err(ConcertQueryError::Operational)?
                .archive_location
                .is_some();
            (concert, user_split_timestamps, archive_configured)
        };

        let tracks = concert
            .set_list
            .iter()
            .enumerate()
            .map(|(index, title)| ConcertTrackState {
                index,
                title: title.clone(),
                persisted_available: concert.tracks_present.get(index).copied().unwrap_or(false),
                liked: concert.tracks_liked.get(index).copied().unwrap_or(false),
            })
            .collect();
        let inventory = ConcertMediaInventory::for_concert(
            &self.working_dir,
            &concert,
            user_split_timestamps.as_deref(),
        );
        let source = match inventory.try_find_downloaded_file() {
            Ok(path) => Observation::Known(SourceMediaState { path }),
            Err(error) => {
                tracing::warn!(concert_id, %error, "could not observe concert source media");
                Observation::Unknown(ObservationFailure {
                    operation: MediaObservationOperation::ReadSourceDirectory,
                })
            }
        };
        let published_split = match inventory.try_published_snapshot() {
            Ok(snapshot) => Observation::Known(PublishedSplitState {
                tracks_present_on_disk: snapshot.tracks_present_on_disk,
                reconstruction_items: snapshot.reconstruction_items,
                source_redundant: snapshot.source_redundant,
            }),
            Err(error) => {
                tracing::warn!(concert_id, %error, "could not observe Published Concert Split");
                Observation::Unknown(ObservationFailure {
                    operation: MediaObservationOperation::ReadPublishedSplit,
                })
            }
        };
        let active_work = self.active_work(&concert);
        let media = MediaAvailability {
            source,
            published_split,
        };
        let permitted_actions =
            permitted_actions(&concert, &media, &active_work, archive_configured);

        tracing::debug!(
            concert_id,
            archive_configured,
            "observed canonical Concert State"
        );
        Ok(ConcertState {
            concert,
            tracks,
            media,
            active_work,
            archive_configured,
            permitted_actions,
        })
    }

    pub fn event_history(
        &self,
        concert_id: i64,
    ) -> Result<Vec<crate::events::EventRow>, ConcertQueryError> {
        let conn = self.lock_db()?;
        self.ensure_exists(&conn, concert_id)?;
        crate::events::try_list_for_concert(&conn, concert_id)
            .map_err(|error| ConcertQueryError::Operational(error.into()))
    }

    pub fn failed_job_history(
        &self,
        concert_id: i64,
    ) -> Result<Vec<db::failed_jobs::FailedJob>, ConcertQueryError> {
        let conn = self.lock_db()?;
        self.ensure_exists(&conn, concert_id)?;
        db::failed_jobs::list_for_concert(&conn, concert_id).map_err(ConcertQueryError::Operational)
    }

    fn lock_db(&self) -> Result<std::sync::MutexGuard<'_, Connection>, ConcertQueryError> {
        self.db.lock().map_err(|_| {
            ConcertQueryError::Operational(anyhow::anyhow!("concert database mutex is poisoned"))
        })
    }

    fn ensure_exists(&self, conn: &Connection, concert_id: i64) -> Result<(), ConcertQueryError> {
        db::concerts::get_concert(conn, concert_id)
            .map(|_| ())
            .map_err(|error| {
                if matches!(
                    error.downcast_ref::<rusqlite::Error>(),
                    Some(rusqlite::Error::QueryReturnedNoRows)
                ) {
                    ConcertQueryError::NotFound { concert_id }
                } else {
                    ConcertQueryError::Operational(error)
                }
            })
    }

    fn active_work(&self, concert: &Concert) -> ActiveWork {
        let key = |kind| JobKey {
            concert_id: concert.id,
            kind,
        };
        let download_key = key(JobKind::Download);
        let split_key = key(JobKind::Split);
        ActiveWork {
            scrape_pending: self.scrape_queue.is_pending(concert.id),
            download: JobActivity {
                persisted_started: concert.download_started_at.is_some(),
                registry_active: self.registry.is_running(&download_key),
            },
            split: JobActivity {
                persisted_started: concert.split_started_at.is_some(),
                registry_active: self.registry.is_running(&split_key),
            },
            archive: JobActivity {
                persisted_started: concert.archive_started_at.is_some(),
                registry_active: self.registry.is_running(&key(JobKind::Archive)),
            },
            split_queued_after_download: self.registry.has_dependent(&download_key, &split_key),
        }
    }
}

fn permitted_actions(
    concert: &Concert,
    media: &MediaAvailability,
    active_work: &ActiveWork,
    archive_configured: bool,
) -> PermittedActions {
    let download_active =
        active_work.download.persisted_started || active_work.download.registry_active;
    let split_active = active_work.split.persisted_started
        || active_work.split.registry_active
        || active_work.split_queued_after_download;
    let archive_active =
        active_work.archive.persisted_started || active_work.archive.registry_active;
    let any_active = download_active || split_active || archive_active;
    let reconstruction_available = match &media.published_split {
        Observation::Known(split) => !split.reconstruction_items.is_empty(),
        Observation::Unknown(_) => false,
    };
    let source_available = matches!(
        &media.source,
        Observation::Known(SourceMediaState { path: Some(_) })
    );
    let source_redundant = match &media.published_split {
        Observation::Known(split) => split.source_redundant,
        Observation::Unknown(_) => false,
    };
    let download_status = concert.download_status();
    let archive_status = concert.archive_status();

    PermittedActions {
        can_download: !any_active
            && matches!(
                download_status,
                DownloadStatus::NotDownloaded | DownloadStatus::DownloadError
            ),
        can_delete_download: !any_active && matches!(download_status, DownloadStatus::Downloaded),
        can_archive: !any_active
            && archive_configured
            && (concert.downloaded_at.is_some() || concert.split_at.is_some())
            && matches!(
                archive_status,
                ArchiveStatus::NotArchived | ArchiveStatus::ArchiveError
            ),
        can_unarchive: !any_active && matches!(archive_status, ArchiveStatus::Archived),
        can_play_concert: source_available || (!split_active && reconstruction_available),
        can_delete_redundant_source: !any_active && source_redundant,
        tracks_busy: split_active,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::db::seeds::{SeedContext, SeedLifecycleConcert, SeedMediaConcert};
    use crate::jobs::scrape_queue::ScrapeQueue;

    #[tokio::test]
    async fn get_returns_complete_state_for_a_concert_with_media() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_media_concert(
                working_dir.path(),
                SeedMediaConcert {
                    lifecycle: SeedLifecycleConcert {
                        set_list: Some(vec!["First".to_string(), "Second".to_string()]),
                        downloaded: true,
                        split: true,
                        tracks_present: Some(vec![true, false]),
                        tracks_liked: Some(vec![false, true]),
                        ..SeedLifecycleConcert::default()
                    },
                    source_file: true,
                    track_files: Some(vec![0]),
                    ..SeedMediaConcert::default()
                },
            )
            .unwrap();
        db::settings::update_archive_location(&conn, "/archive").unwrap();

        let db = Arc::new(Mutex::new(conn));
        let scrape_queue = ScrapeQueue::start(db.clone(), working_dir.path().to_path_buf());
        let concerts = Concerts::new(
            db,
            working_dir.path().to_path_buf(),
            Arc::new(JobRegistry::new()),
            scrape_queue,
        );

        let state = concerts.get(concert.id).unwrap();

        assert_eq!(state.concert.id, concert.id);
        assert_eq!(
            state.tracks,
            vec![
                ConcertTrackState {
                    index: 0,
                    title: "First".to_string(),
                    persisted_available: true,
                    liked: false,
                },
                ConcertTrackState {
                    index: 1,
                    title: "Second".to_string(),
                    persisted_available: false,
                    liked: true,
                },
            ]
        );
        assert!(matches!(
            state.media.source,
            Observation::Known(SourceMediaState { path: Some(_) })
        ));
        let Observation::Known(published) = state.media.published_split else {
            panic!("Published Concert Split should be known");
        };
        assert_eq!(published.tracks_present_on_disk, vec![true, false]);
        assert!(state.archive_configured);
        assert_eq!(
            state.active_work,
            ActiveWork {
                scrape_pending: false,
                download: JobActivity {
                    persisted_started: false,
                    registry_active: false,
                },
                split: JobActivity {
                    persisted_started: false,
                    registry_active: false,
                },
                archive: JobActivity {
                    persisted_started: false,
                    registry_active: false,
                },
                split_queued_after_download: false,
            }
        );
        assert_eq!(
            state.permitted_actions,
            PermittedActions {
                can_download: false,
                can_delete_download: true,
                can_archive: true,
                can_unarchive: false,
                can_play_concert: true,
                can_delete_redundant_source: false,
                tracks_busy: false,
            }
        );
    }

    #[tokio::test]
    async fn get_keeps_source_known_when_published_split_lock_cannot_be_opened() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_media_concert(
                working_dir.path(),
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
        let concert_dir =
            crate::model::concert_dir(working_dir.path(), concert.album.as_deref().unwrap());
        std::fs::create_dir(concert_dir.join(".concert-split-publication.lock")).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let concerts = Concerts::new(
            db.clone(),
            working_dir.path().to_path_buf(),
            Arc::new(JobRegistry::new()),
            ScrapeQueue::start(db, working_dir.path().to_path_buf()),
        );

        let state = concerts.get(concert.id).unwrap();

        assert!(matches!(
            state.media.source,
            Observation::Known(SourceMediaState { path: Some(_) })
        ));
        assert!(matches!(
            state.media.published_split,
            Observation::Unknown(ObservationFailure {
                operation: MediaObservationOperation::ReadPublishedSplit,
            })
        ));
        assert!(!state.permitted_actions.can_delete_redundant_source);
    }

    #[tokio::test]
    async fn get_reports_directory_read_failures_as_unknown_not_absent() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_lifecycle_concert(SeedLifecycleConcert::default())
            .unwrap();
        let concert_dir =
            crate::model::concert_dir(working_dir.path(), concert.album.as_deref().unwrap());
        std::fs::create_dir_all(concert_dir.parent().unwrap()).unwrap();
        std::fs::write(&concert_dir, b"not a directory").unwrap();
        let db = Arc::new(Mutex::new(conn));
        let concerts = Concerts::new(
            db.clone(),
            working_dir.path().to_path_buf(),
            Arc::new(JobRegistry::new()),
            ScrapeQueue::start(db, working_dir.path().to_path_buf()),
        );

        let state = concerts.get(concert.id).unwrap();

        assert_eq!(
            state.media.source,
            Observation::Unknown(ObservationFailure {
                operation: MediaObservationOperation::ReadSourceDirectory,
            })
        );
        assert!(matches!(
            state.media.published_split,
            Observation::Unknown(ObservationFailure {
                operation: MediaObservationOperation::ReadPublishedSplit,
            })
        ));
        assert!(!state.permitted_actions.can_play_concert);
        assert!(!state.permitted_actions.can_delete_redundant_source);
    }

    #[tokio::test]
    async fn get_combines_registry_queue_and_persisted_active_work() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_media_concert(
                working_dir.path(),
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
        let db = Arc::new(Mutex::new(conn));
        let registry = Arc::new(JobRegistry::new());
        let key = |kind| JobKey {
            concert_id: concert.id,
            kind,
        };
        let (_download_reservation, _download_signal) =
            registry.try_reserve(key(JobKind::Download)).unwrap();
        let (_split_reservation, _split_signal) =
            registry.try_reserve(key(JobKind::Split)).unwrap();
        let (_archive_reservation, _archive_signal) =
            registry.try_reserve(key(JobKind::Archive)).unwrap();
        registry.add_dependent(key(JobKind::Download), key(JobKind::Split));

        let blocker = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let worker_blocker = blocker.clone();
        let scrape_queue = ScrapeQueue::start_with(
            db.clone(),
            working_dir.path().to_path_buf(),
            Arc::new(move |_, _, _| {
                let (lock, changed) = &*worker_blocker;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = changed.wait(released).unwrap();
                }
            }),
        );
        assert!(scrape_queue.enqueue(concert.id, concert.source_url.clone()));
        let concerts = Concerts::new(db, working_dir.path().to_path_buf(), registry, scrape_queue);

        let state = concerts.get(concert.id).unwrap();

        assert!(state.active_work.scrape_pending);
        assert!(state.active_work.download.registry_active);
        assert!(state.active_work.split.registry_active);
        assert!(state.active_work.archive.registry_active);
        assert!(state.active_work.split_queued_after_download);
        assert!(state.permitted_actions.tracks_busy);
        assert!(!state.permitted_actions.can_download);
        assert!(!state.permitted_actions.can_delete_download);
        assert!(!state.permitted_actions.can_archive);
        assert!(!state.permitted_actions.can_delete_redundant_source);

        let (lock, changed) = &*blocker;
        *lock.lock().unwrap() = true;
        changed.notify_all();
    }

    #[tokio::test]
    async fn histories_are_explicit_and_missing_concerts_are_typed() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_listing(db::seeds::SeedListing::default())
            .unwrap();
        db::failed_jobs::insert_failed_job(&conn, concert.id, "download", "boom").unwrap();
        let db = Arc::new(Mutex::new(conn));
        let concerts = Concerts::new(
            db.clone(),
            working_dir.path().to_path_buf(),
            Arc::new(JobRegistry::new()),
            ScrapeQueue::start(db, working_dir.path().to_path_buf()),
        );

        let events = concerts.event_history(concert.id).unwrap();
        let failed_jobs = concerts.failed_job_history(concert.id).unwrap();

        assert!(!events.is_empty());
        assert_eq!(failed_jobs.len(), 1);
        assert_eq!(failed_jobs[0].failure_message, "boom");
        assert!(matches!(
            concerts.get(i64::MAX),
            Err(ConcertQueryError::NotFound {
                concert_id: i64::MAX
            })
        ));
        assert!(matches!(
            concerts.event_history(i64::MAX),
            Err(ConcertQueryError::NotFound {
                concert_id: i64::MAX
            })
        ));
    }

    #[tokio::test]
    async fn persisted_job_activity_without_registry_state_fails_closed_per_kind() {
        for kind in [JobKind::Download, JobKind::Split, JobKind::Archive] {
            let working_dir = tempfile::tempdir().unwrap();
            let conn = db::connection::open_in_memory().unwrap();
            let seed = SeedLifecycleConcert {
                downloaded: !matches!(kind, JobKind::Download),
                ..SeedLifecycleConcert::default()
            };
            let concert = SeedContext::new(&conn)
                .seed_lifecycle_concert(seed)
                .unwrap();
            match kind {
                JobKind::Download => {
                    db::lifecycle::try_mark_download_started(&conn, concert.id).unwrap();
                }
                JobKind::Split => {
                    db::lifecycle::try_mark_split_started(&conn, concert.id).unwrap();
                }
                JobKind::Archive => {
                    db::lifecycle::try_mark_archive_started(&conn, concert.id).unwrap();
                }
            }
            db::settings::update_archive_location(&conn, "/archive").unwrap();
            let db = Arc::new(Mutex::new(conn));
            let concerts = Concerts::new(
                db.clone(),
                working_dir.path().to_path_buf(),
                Arc::new(JobRegistry::new()),
                ScrapeQueue::start(db, working_dir.path().to_path_buf()),
            );

            let state = concerts.get(concert.id).unwrap();
            let activity = match kind {
                JobKind::Download => state.active_work.download,
                JobKind::Split => state.active_work.split,
                JobKind::Archive => state.active_work.archive,
            };

            assert!(activity.persisted_started, "{kind:?}");
            assert!(!activity.registry_active, "{kind:?}");
            assert!(!state.permitted_actions.can_download, "{kind:?}");
            assert!(!state.permitted_actions.can_delete_download, "{kind:?}");
            assert!(!state.permitted_actions.can_archive, "{kind:?}");
            assert!(!state.permitted_actions.can_unarchive, "{kind:?}");
            assert_eq!(
                state.permitted_actions.tracks_busy,
                matches!(kind, JobKind::Split),
                "{kind:?}"
            );
        }
    }

    #[tokio::test]
    async fn registry_activity_applies_the_policy_independently_per_kind() {
        // Download: an inert concert is normally downloadable.
        {
            let working_dir = tempfile::tempdir().unwrap();
            let conn = db::connection::open_in_memory().unwrap();
            let concert = SeedContext::new(&conn)
                .seed_lifecycle_concert(SeedLifecycleConcert::default())
                .unwrap();
            let db = Arc::new(Mutex::new(conn));
            let registry = Arc::new(JobRegistry::new());
            let concerts = Concerts::new(
                db.clone(),
                working_dir.path().to_path_buf(),
                registry.clone(),
                ScrapeQueue::start(db, working_dir.path().to_path_buf()),
            );
            assert!(
                concerts
                    .get(concert.id)
                    .unwrap()
                    .permitted_actions
                    .can_download
            );
            let (_reservation, _signal) = registry
                .try_reserve(JobKey {
                    concert_id: concert.id,
                    kind: JobKind::Download,
                })
                .unwrap();

            let state = concerts.get(concert.id).unwrap();
            assert!(state.active_work.download.registry_active);
            assert!(!state.active_work.split.registry_active);
            assert!(!state.active_work.archive.registry_active);
            assert!(!state.permitted_actions.can_download);
            assert!(!state.permitted_actions.can_unarchive);
            assert!(!state.permitted_actions.can_play_concert);
        }

        // Split: source playback remains safe, while track playback and
        // conflicting media mutations fail closed.
        {
            let working_dir = tempfile::tempdir().unwrap();
            let conn = db::connection::open_in_memory().unwrap();
            let concert = SeedContext::new(&conn)
                .seed_media_concert(
                    working_dir.path(),
                    SeedMediaConcert {
                        lifecycle: SeedLifecycleConcert {
                            downloaded: true,
                            split: true,
                            tracks_present: Some(vec![true, true, true]),
                            ..SeedLifecycleConcert::default()
                        },
                        source_file: true,
                        track_files: Some(vec![0, 1, 2]),
                        ..SeedMediaConcert::default()
                    },
                )
                .unwrap();
            db::settings::update_archive_location(&conn, "/archive").unwrap();
            let db = Arc::new(Mutex::new(conn));
            let registry = Arc::new(JobRegistry::new());
            let concerts = Concerts::new(
                db.clone(),
                working_dir.path().to_path_buf(),
                registry.clone(),
                ScrapeQueue::start(db, working_dir.path().to_path_buf()),
            );
            let baseline = concerts.get(concert.id).unwrap();
            assert!(baseline.permitted_actions.can_delete_download);
            assert!(baseline.permitted_actions.can_archive);
            assert!(baseline.permitted_actions.can_play_concert);
            let (_reservation, _signal) = registry
                .try_reserve(JobKey {
                    concert_id: concert.id,
                    kind: JobKind::Split,
                })
                .unwrap();

            let state = concerts.get(concert.id).unwrap();
            assert!(state.active_work.split.registry_active);
            assert!(state.permitted_actions.tracks_busy);
            assert!(!state.permitted_actions.can_delete_download);
            assert!(!state.permitted_actions.can_archive);
            assert!(
                state.permitted_actions.can_play_concert,
                "known source playback remains safe while split is active"
            );
        }

        // Split with no source: reconstruction is available at rest but must
        // not be offered while its Published Concert Split may be changing.
        {
            let working_dir = tempfile::tempdir().unwrap();
            let conn = db::connection::open_in_memory().unwrap();
            let concert = SeedContext::new(&conn)
                .seed_media_concert(
                    working_dir.path(),
                    SeedMediaConcert {
                        lifecycle: SeedLifecycleConcert {
                            downloaded: true,
                            split: true,
                            tracks_present: Some(vec![true, true, true]),
                            ..SeedLifecycleConcert::default()
                        },
                        track_files: Some(vec![0, 1, 2]),
                        ..SeedMediaConcert::default()
                    },
                )
                .unwrap();
            let db = Arc::new(Mutex::new(conn));
            let registry = Arc::new(JobRegistry::new());
            let concerts = Concerts::new(
                db.clone(),
                working_dir.path().to_path_buf(),
                registry.clone(),
                ScrapeQueue::start(db, working_dir.path().to_path_buf()),
            );
            assert!(
                concerts
                    .get(concert.id)
                    .unwrap()
                    .permitted_actions
                    .can_play_concert
            );
            let (_reservation, _signal) = registry
                .try_reserve(JobKey {
                    concert_id: concert.id,
                    kind: JobKind::Split,
                })
                .unwrap();

            assert!(
                !concerts
                    .get(concert.id)
                    .unwrap()
                    .permitted_actions
                    .can_play_concert,
                "reconstruction playback is suppressed while split is active"
            );
        }

        // Archive: an archived concert is normally unarchivable; a defensive
        // registry-only Archive run suppresses that conflicting operation.
        {
            let working_dir = tempfile::tempdir().unwrap();
            let conn = db::connection::open_in_memory().unwrap();
            let concert = SeedContext::new(&conn)
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
            db::lifecycle::try_mark_archive_started(&conn, concert.id).unwrap();
            db::lifecycle::mark_archive_succeeded(&conn, concert.id).unwrap();
            let db = Arc::new(Mutex::new(conn));
            let registry = Arc::new(JobRegistry::new());
            let concerts = Concerts::new(
                db.clone(),
                working_dir.path().to_path_buf(),
                registry.clone(),
                ScrapeQueue::start(db, working_dir.path().to_path_buf()),
            );
            let baseline = concerts.get(concert.id).unwrap();
            assert!(baseline.permitted_actions.can_unarchive);
            assert!(baseline.permitted_actions.can_play_concert);
            let (_reservation, _signal) = registry
                .try_reserve(JobKey {
                    concert_id: concert.id,
                    kind: JobKind::Archive,
                })
                .unwrap();

            let state = concerts.get(concert.id).unwrap();
            assert!(state.active_work.archive.registry_active);
            assert!(!state.permitted_actions.can_unarchive);
            assert!(state.permitted_actions.can_play_concert);
        }
    }

    #[tokio::test]
    async fn persistence_and_history_failures_are_operational_errors() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_lifecycle_concert(SeedLifecycleConcert::default())
            .unwrap();
        conn.execute(
            "UPDATE concerts SET user_split_timestamps_json = 'not json' WHERE id = ?1",
            rusqlite::params![concert.id],
        )
        .unwrap();
        let db = Arc::new(Mutex::new(conn));
        let concerts = Concerts::new(
            db.clone(),
            working_dir.path().to_path_buf(),
            Arc::new(JobRegistry::new()),
            ScrapeQueue::start(db.clone(), working_dir.path().to_path_buf()),
        );

        assert!(matches!(
            concerts.get(concert.id),
            Err(ConcertQueryError::Operational(_))
        ));

        {
            let conn = db.lock().unwrap();
            conn.execute_batch("DROP TABLE events").unwrap();
        }
        assert!(matches!(
            concerts.event_history(concert.id),
            Err(ConcertQueryError::Operational(_))
        ));
    }
}
