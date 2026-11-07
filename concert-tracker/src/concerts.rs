//! Application interface for observing and interacting with individual concerts.
//!
//! [`Concerts`] owns individual Concert queries and commands; the child
//! [`catalog`] module owns browsing the collection of concerts. See
//! `docs/concerts.md` for the interface, the observation semantics, and why the
//! catalog is a separate module.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Error;
use rusqlite::Connection;

use crate::concert_media::ConcertMediaInventory;
use crate::db;
use crate::jobs::scrape_queue::ScrapeQueue;
use crate::jobs::{JobConfig, JobKey, JobKind, JobRegistry};
use crate::model::{
    ArchiveStatus, Concert, ConcertStatus, DownloadStatus, PlaybackItem, SplitStatus,
};

pub mod catalog;

pub use catalog::{ConcertCatalog, ConcertFilter, ListingContext};

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

/// How admitting a Job Request went. Adapters that only re-render do not care;
/// a batch tool does, because "already running" and "nothing to run" mean
/// different things in its report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobAdmission {
    /// Admitted: the Job Run is queued or running.
    Admitted,
    /// The same Job Run was already active, so the request changed nothing.
    AlreadyRunning,
    /// There was nothing to run: a split whose source is not downloaded, or a
    /// download whose source is already on disk and recorded.
    NothingToDo,
}

/// A Job Request's result: the admission decision and the Concert State observed
/// right after it. `admission` is for callers that report it; a caller that only
/// re-renders uses `state`.
#[derive(Debug)]
pub struct JobRequestResult {
    pub admission: JobAdmission,
    pub state: ConcertState,
}

fn download_admission(outcome: crate::jobs::download::StartOutcome) -> JobAdmission {
    match outcome {
        crate::jobs::download::StartOutcome::Spawned => JobAdmission::Admitted,
        crate::jobs::download::StartOutcome::AlreadyRunning => JobAdmission::AlreadyRunning,
    }
}

fn split_admission(outcome: crate::jobs::split::StartOutcome) -> JobAdmission {
    match outcome {
        crate::jobs::split::StartOutcome::Spawned => JobAdmission::Admitted,
        crate::jobs::split::StartOutcome::AlreadyRunning => JobAdmission::AlreadyRunning,
        crate::jobs::split::StartOutcome::NotDownloaded => JobAdmission::NothingToDo,
    }
}

/// What asking for an archive Job Run did. Both outcomes carry the Concert State
/// observed after the attempt, because "nothing to archive" is still worth
/// rendering.
#[derive(Debug)]
pub enum ArchiveOutcome {
    /// Archive work was admitted (or was already running).
    Accepted(ConcertState),
    /// The concert has neither a download nor a split, so there is nothing to
    /// move into the archive location.
    NothingToArchive(ConcertState),
}

impl ArchiveOutcome {
    pub fn state(&self) -> &ConcertState {
        match self {
            Self::Accepted(state) | Self::NothingToArchive(state) => state,
        }
    }
}

/// What asking for a background scrape did. A scrape is queued work, not a Job
/// Run (ADR-0006), so its admission has its own outcome vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrapeAdmission {
    /// Queued for the background worker.
    Enqueued,
    /// Already queued or in flight, so nothing more was done.
    AlreadyQueued,
    /// The concert already has metadata; there is nothing to scrape.
    NotNeeded,
}

/// What cancelling a Job Run did. Both outcomes carry the Concert State observed
/// after the attempt, because "nothing was running" is still worth rendering.
#[derive(Debug)]
pub enum JobCancellation {
    /// An active Job Run was cancelled. Its Concert is back to a terminal
    /// unsuccessful state.
    Cancelled(ConcertState),
    /// No Job Run of that kind was active. Cancellation is idempotent: asking
    /// again is not an error.
    NothingActive(ConcertState),
}

impl JobCancellation {
    pub fn state(&self) -> &ConcertState {
        match self {
            Self::Cancelled(state) | Self::NothingActive(state) => state,
        }
    }
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

/// Outcome of a `Concerts` command that changes a concert. A rejection is an
/// expected domain outcome, not a failure of the application; an operational
/// error is a genuine fault. Adapters map the two onto their own protocol.
#[derive(Debug)]
pub enum ConcertCommandError {
    Rejected(CommandRejection),
    Operational(Error),
}

/// A command that cannot apply to the concert as observed. Rejections carry the
/// facts a caller needs to explain itself; they never stand in for a fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandRejection {
    /// No such concert.
    NotFound { concert_id: i64 },
    /// The set list has no track at this index.
    NoSuchTrack { concert_id: i64, track_index: usize },
    /// The media the command needs is not available, so the command cannot
    /// have the effect it asks for.
    MissingMedia {
        concert_id: i64,
        target: ListenTarget,
    },
    /// The source file is still needed: song and interlude media do not cover
    /// all of it, so deleting it would lose audio.
    SourceNotRedundant { concert_id: i64 },
    /// A well-formed change that conflicts with the concert's current state.
    Conflict { concert_id: i64, message: String },
    /// A well-formed change whose values the concert cannot accept.
    Invalid { concert_id: i64, message: String },
}

impl std::fmt::Display for ConcertCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(rejection) => write!(formatter, "{rejection}"),
            Self::Operational(error) => error.fmt(formatter),
        }
    }
}

impl std::fmt::Display for CommandRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { concert_id } => write!(formatter, "concert {concert_id} not found"),
            Self::NoSuchTrack {
                concert_id,
                track_index,
            } => write!(
                formatter,
                "concert {concert_id} has no track at index {track_index}"
            ),
            Self::MissingMedia { concert_id, target } => write!(
                formatter,
                "concert {concert_id} has no media available for {}",
                target.describe()
            ),
            Self::SourceNotRedundant { concert_id } => write!(
                formatter,
                "concert {concert_id} source file is still needed: song and interlude media do not cover it"
            ),
            Self::Conflict {
                concert_id,
                message,
            } => write!(formatter, "concert {concert_id}: {message}"),
            Self::Invalid {
                concert_id,
                message,
            } => write!(formatter, "concert {concert_id}: {message}"),
        }
    }
}

impl std::error::Error for ConcertCommandError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Rejected(_) => None,
            Self::Operational(error) => Some(error.as_ref()),
        }
    }
}

impl From<ConcertQueryError> for ConcertCommandError {
    fn from(error: ConcertQueryError) -> Self {
        match error {
            ConcertQueryError::NotFound { concert_id } => {
                Self::Rejected(CommandRejection::NotFound { concert_id })
            }
            ConcertQueryError::Operational(error) => Self::Operational(error),
        }
    }
}

/// What a listener listened to or watched. Recording that is always an explicit
/// command: resolving media never records a listen on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenTarget {
    /// The whole concert's source file.
    Source,
    /// One set-list track.
    Track { index: usize },
}

impl ListenTarget {
    pub fn describe(&self) -> String {
        match self {
            Self::Source => "the concert source".to_string(),
            Self::Track { index } => format!("track {index}"),
        }
    }

    fn track_index(&self) -> Option<usize> {
        match self {
            Self::Source => None,
            Self::Track { index } => Some(*index),
        }
    }
}

/// Why resolving playback failed. `NotFound` is the absence of the concert
/// itself; `Lookup` is the media lookup's own typed outcome, kept separate so
/// adapters can map "nothing to play" and "the file we recorded is gone"
/// differently; `Query` is a persistence failure.
#[derive(Debug)]
pub enum ConcertPlaybackError {
    NotFound { concert_id: i64 },
    Query(ConcertQueryError),
    Lookup(crate::playback::PlaybackLookupError),
}

impl std::fmt::Display for ConcertPlaybackError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { concert_id } => write!(formatter, "concert {concert_id} not found"),
            Self::Query(error) => error.fmt(formatter),
            Self::Lookup(error) => write!(formatter, "{error:?}"),
        }
    }
}

impl std::error::Error for ConcertPlaybackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Query(error) => Some(error),
            Self::NotFound { .. } | Self::Lookup(_) => None,
        }
    }
}

/// Whether the caller has confirmed a deletion that needs confirmation. A
/// distinct type rather than a bool, because "delete this, I mean it" and
/// "delete this if nothing surprising is going on" are different requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletionConfirmation {
    /// Delete only if the recorded facts match the filesystem.
    Unconfirmed,
    /// Delete even when the filesystem disagrees with the record.
    Confirmed,
}

/// Outcome of asking for media to be absent. Deletion is idempotent: asking
/// again after the media is gone succeeds and reports the current state.
#[derive(Debug)]
pub enum DeletionOutcome {
    /// The desired absent state holds.
    Deleted(ConcertState),
    /// The database records a download whose file is already gone. Repairing
    /// that record is destructive enough to need explicit confirmation.
    ConfirmationRequired(ConcertState),
}

impl DeletionOutcome {
    /// The Concert State observed after the attempt, which adapters use to
    /// re-render without a second query.
    pub fn state(&self) -> &ConcertState {
        match self {
            Self::Deleted(state) | Self::ConfirmationRequired(state) => state,
        }
    }
}

/// A split-timestamp change that started work, with the Concert State observed
/// right after admission.
#[derive(Debug)]
pub struct SplitStart {
    pub outcome: crate::split_timestamps::SplitStartOutcome,
    pub state: ConcertState,
}

/// A concert row plus the separately stored facts needed to observe it. Loaded
/// under the database lock so a whole collection can be read at once and then
/// observed one concert at a time.
pub(super) struct LoadedConcert {
    pub(super) concert: Concert,
    pub(super) user_split_timestamps: Option<Vec<concert_types::SongTimestamp>>,
}

#[derive(Clone)]
pub struct Concerts {
    db: Arc<Mutex<Connection>>,
    working_dir: PathBuf,
    registry: Arc<JobRegistry>,
    scrape_queue: ScrapeQueue,
    /// How to run Job Runs. `None` in a one-shot process that runs none: the
    /// methods that would admit work report that instead of pretending to do it.
    jobs: Option<JobConfig>,
}

impl Concerts {
    pub fn new(
        db: Arc<Mutex<Connection>>,
        working_dir: PathBuf,
        registry: Arc<JobRegistry>,
        scrape_queue: ScrapeQueue,
        jobs: JobConfig,
    ) -> Self {
        Self {
            db,
            working_dir,
            registry,
            scrape_queue,
            jobs: Some(jobs),
        }
    }

    /// Standalone interface for a one-shot process (the `concert_db` CLI). Such
    /// a process runs no Job Runs and starts no scrape worker, so active work is
    /// only what persistence records: a job started by another process is not
    /// visible here.
    pub fn without_workers(db: Arc<Mutex<Connection>>, working_dir: PathBuf) -> Self {
        Self {
            db,
            working_dir,
            registry: Arc::new(JobRegistry::new()),
            scrape_queue: ScrapeQueue::detached(),
            jobs: None,
        }
    }

    /// The job configuration, or a typed operational error when this process
    /// cannot run Job Runs at all.
    fn job_config(&self) -> Result<JobConfig, ConcertCommandError> {
        self.jobs.clone().ok_or_else(|| {
            ConcertCommandError::Operational(anyhow::anyhow!(
                "this process does not run Job Runs, so it cannot admit work"
            ))
        })
    }

    /// The collection-browsing interface built on this same observation.
    pub fn catalog(&self) -> ConcertCatalog {
        ConcertCatalog::new(self.clone())
    }

    /// Observe one concert if it exists. Adapters that render a reference to a
    /// concert they do not own (a playlist item, a stale link) need "absent" as a
    /// value, not an error.
    pub fn get_opt(&self, concert_id: i64) -> Result<Option<ConcertState>, ConcertQueryError> {
        match self.get(concert_id) {
            Ok(state) => Ok(Some(state)),
            Err(ConcertQueryError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn get(&self, concert_id: i64) -> Result<ConcertState, ConcertQueryError> {
        let (loaded, archive_configured) = {
            let conn = self.lock_db()?;
            let loaded = self.load(&conn, concert_id)?;
            let archive_configured = archive_configured(&conn)?;
            (loaded, archive_configured)
        };
        Ok(self.observe(loaded, archive_configured))
    }

    /// Set the desired Concert Status. Available, Wanted, and Ignored are
    /// mutually exclusive, and repeating a request for the status already
    /// stored is a no-op, so a retried request cannot reverse the user's choice.
    ///
    /// Becoming Ignored also discards the cached preview image and listing
    /// thumbnail: an ignored concert is one the user has decided not to browse.
    pub fn set_status(
        &self,
        concert_id: i64,
        status: ConcertStatus,
    ) -> Result<ConcertState, ConcertCommandError> {
        let (previous_status, ignored_album) = {
            let conn = self.lock_db()?;
            let concert = self.load(&conn, concert_id)?.concert;
            let previous_status = concert.concert_status();
            db::concerts::set_concert_status(&conn, concert_id, status)
                .map_err(ConcertCommandError::Operational)?;
            let ignored_album =
                if status == ConcertStatus::Ignored && previous_status != ConcertStatus::Ignored {
                    concert.album
                } else {
                    None
                };
            (previous_status, ignored_album)
        };
        tracing::info!(
            concert_id,
            from = previous_status.slug(),
            to = status.slug(),
            "set desired Concert Status"
        );
        if let Some(album) = ignored_album.as_deref() {
            crate::scrape::remove_cached_images(&self.working_dir, album);
        }
        self.get(concert_id).map_err(ConcertCommandError::from)
    }

    /// Set the desired notes text for a concert.
    pub fn set_notes(
        &self,
        concert_id: i64,
        notes: &str,
    ) -> Result<ConcertState, ConcertCommandError> {
        {
            let conn = self.lock_db()?;
            self.ensure_exists(&conn, concert_id)?;
            db::concerts::set_notes(&conn, concert_id, notes)
                .map_err(ConcertCommandError::Operational)?;
        }
        tracing::info!(concert_id, notes_len = notes.len(), "set desired notes");
        self.get(concert_id).map_err(ConcertCommandError::from)
    }

    // ── Job Requests ────────────────────────────────────────────────────────
    //
    // Download, split, and their chain are Job Runs. These methods are where a
    // request becomes one, so `PermittedActions` and admission cannot disagree:
    // both read the same observed state.

    /// Ask for the concert's source media. When the concert has a set list and
    /// nothing split yet, this is the whole download → split chain; otherwise it
    /// is a plain download. An accepted request reports the state after
    /// persistent admission, and a repeat request while the run is active is
    /// accepted (and changes nothing).
    pub async fn request_download(
        &self,
        concert_id: i64,
    ) -> Result<JobRequestResult, ConcertCommandError> {
        let jobs = self.job_config()?;
        self.ensure_scraped(concert_id).await?;
        // Re-read after the scrape: the metadata may have just been written.
        let concert = {
            let conn = self.lock_db()?;
            self.load(&conn, concert_id)?.concert
        };

        let auto_splits = concert.album.is_some()
            && !concert.set_list.is_empty()
            && matches!(
                concert.split_status(),
                SplitStatus::NotSplit | SplitStatus::SplitError
            );
        if !auto_splits {
            return self.start_download(concert_id, jobs).await;
        }

        // Whether the chain was already in flight decides how the admission is
        // reported; preparation itself cannot tell an established run from a new
        // one.
        let already_active = {
            let state = self.command_state(concert_id)?;
            state.active_work.download.registry_active
                || state.active_work.split.registry_active
                || state.active_work.split_queued_after_download
        };
        match crate::jobs::prepare::prepare(
            self.db.clone(),
            self.registry.clone(),
            jobs.clone(),
            concert_id,
        )
        .await
        {
            Ok(crate::jobs::prepare::PrepareOutcome::Ready) => {
                // Every track exists but the source is gone (a manual copy, a
                // restored backup). The user asked for the concert itself, so
                // download anyway.
                let album = concert.album.as_deref().expect("checked above");
                if crate::concert_media::find_downloaded_file(&self.working_dir, album).is_none() {
                    return self.start_download(concert_id, jobs).await;
                }
                // The source is already on disk and preparation reconciled the
                // record, so the request has nothing left to run.
                Ok(JobRequestResult {
                    admission: JobAdmission::NothingToDo,
                    state: self.command_state(concert_id)?,
                })
            }
            Ok(_) => Ok(JobRequestResult {
                admission: if already_active {
                    JobAdmission::AlreadyRunning
                } else {
                    JobAdmission::Admitted
                },
                state: self.command_state(concert_id)?,
            }),
            Err(error)
                if error
                    .downcast_ref::<crate::jobs::prepare::NoSetList>()
                    .is_some() =>
            {
                // The metadata changed between this request's check and
                // preparation's own read: download plainly rather than fail.
                self.start_download(concert_id, jobs).await
            }
            Err(error) => Err(ConcertCommandError::Operational(error)),
        }
    }

    async fn start_download(
        &self,
        concert_id: i64,
        jobs: JobConfig,
    ) -> Result<JobRequestResult, ConcertCommandError> {
        let outcome = crate::jobs::download::start_download(
            self.db.clone(),
            self.registry.clone(),
            jobs,
            concert_id,
        )
        .await
        .map_err(ConcertCommandError::Operational)?;
        Ok(JobRequestResult {
            admission: download_admission(outcome),
            state: self.command_state(concert_id)?,
        })
    }

    /// Ask for the concert's tracks to be split from its source. A concert whose
    /// source is not downloaded accepts the request and does nothing, which is
    /// what the Split control has always done.
    pub async fn request_split(
        &self,
        concert_id: i64,
    ) -> Result<JobRequestResult, ConcertCommandError> {
        let jobs = self.job_config()?;
        {
            let conn = self.lock_db()?;
            self.ensure_exists(&conn, concert_id)?;
        }
        let outcome = crate::jobs::split::start_split(
            self.db.clone(),
            self.registry.clone(),
            jobs,
            concert_id,
            crate::jobs::SplitMode::Analyze,
        )
        .await
        .map_err(ConcertCommandError::Operational)?;
        Ok(JobRequestResult {
            admission: split_admission(outcome),
            state: self.command_state(concert_id)?,
        })
    }

    /// Make every track playable: scrape metadata when absent, then ensure the
    /// download → split chain is running. Idempotent, so a repeat call (or a
    /// click on a track that is still downloading) is safe.
    pub async fn prepare(&self, concert_id: i64) -> Result<ConcertState, ConcertCommandError> {
        let jobs = self.job_config()?;
        self.ensure_scraped(concert_id).await?;
        if let Err(error) =
            crate::jobs::prepare::prepare(self.db.clone(), self.registry.clone(), jobs, concert_id)
                .await
        {
            if error
                .downcast_ref::<crate::jobs::prepare::NoSetList>()
                .is_some()
            {
                // The user can fix this by scraping metadata, so it is a
                // rejection rather than a fault.
                return Err(ConcertCommandError::Rejected(CommandRejection::Invalid {
                    concert_id,
                    message: error.to_string(),
                }));
            }
            return Err(ConcertCommandError::Operational(error));
        }
        self.command_state(concert_id)
    }

    /// Cancel the concert's active Job Run of one kind. Cancellation goes
    /// through the Job Run engine, so the run still reaches exactly one terminal
    /// outcome and keeps its Failed Job history; having nothing to cancel is a
    /// normal outcome, not a failure.
    pub fn cancel_job(
        &self,
        concert_id: i64,
        kind: JobKind,
    ) -> Result<JobCancellation, ConcertCommandError> {
        let outcome = {
            let conn = self.lock_db()?;
            crate::lifecycle::cancel_job(&conn, &self.registry, concert_id, kind)
                .map_err(ConcertCommandError::Operational)?
        };
        let state = self.command_state(concert_id)?;
        tracing::info!(concert_id, ?kind, ?outcome, "cancelled concert job");
        Ok(match outcome {
            crate::lifecycle::CancelJobOutcome::NoSuchActiveJob => {
                JobCancellation::NothingActive(state)
            }
            _ => JobCancellation::Cancelled(state),
        })
    }

    // ── Archive ─────────────────────────────────────────────────────────────
    //
    // Archiving is a Job Run. Unarchiving is not: it repairs a symlink in place,
    // so it has no Job Run, no cancellation, and no recovery of its own.

    /// Ask for the concert's media to be moved into the configured archive
    /// location. A concert with neither a download nor a split has nothing to
    /// archive, which is an outcome rather than a failure.
    pub async fn request_archive(
        &self,
        concert_id: i64,
    ) -> Result<ArchiveOutcome, ConcertCommandError> {
        let _jobs = self.job_config()?;
        let archive_location = {
            let conn = self.lock_db()?;
            self.ensure_exists(&conn, concert_id)?;
            db::settings::get_settings(&conn)
                .map_err(ConcertCommandError::Operational)?
                .archive_location
        };
        let Some(archive_location) = archive_location else {
            return Err(ConcertCommandError::Rejected(CommandRejection::Conflict {
                concert_id,
                message: "archive location is not configured".to_string(),
            }));
        };
        let outcome = crate::jobs::archive::start_archive(
            self.db.clone(),
            self.registry.clone(),
            &self.working_dir,
            &archive_location,
            concert_id,
        )
        .await
        .map_err(ConcertCommandError::Operational)?;
        let state = self.command_state(concert_id)?;
        Ok(match outcome {
            crate::jobs::archive::StartOutcome::NothingToArchive => {
                ArchiveOutcome::NothingToArchive(state)
            }
            _ => ArchiveOutcome::Accepted(state),
        })
    }

    /// Move the concert's media back from the archive location and clear its
    /// archive lifecycle. Every eligibility check, the filesystem work, the
    /// persistence, and the failure record live here, so an adapter only
    /// translates. The work runs on a blocking thread because it is file IO.
    pub async fn unarchive(&self, concert_id: i64) -> Result<ConcertState, ConcertCommandError> {
        let album = {
            let conn = self.lock_db()?;
            let concert = self.load(&conn, concert_id)?.concert;
            if concert.archived_at.is_none() {
                return Err(ConcertCommandError::Rejected(CommandRejection::Conflict {
                    concert_id,
                    message: "is not archived".to_string(),
                }));
            }
            if concert.archive_started_at.is_some() {
                return Err(ConcertCommandError::Rejected(CommandRejection::Conflict {
                    concert_id,
                    message: "has an archive job in flight; cannot unarchive".to_string(),
                }));
            }
            match concert.album {
                Some(album) => album,
                None => {
                    return Err(ConcertCommandError::Rejected(CommandRejection::Conflict {
                        concert_id,
                        message: "has no album, so it has no media directory".to_string(),
                    }))
                }
            }
        };

        let source_dir = crate::model::concert_dir(&self.working_dir, &album);
        tracing::info!(
            concert_id,
            album,
            path = %source_dir.display(),
            "unarchive started"
        );
        let result = {
            let source = source_dir.clone();
            tokio::task::spawn_blocking(move || crate::jobs::archive::do_unarchive(&source))
                .await
                .map_err(|error| {
                    ConcertCommandError::Operational(anyhow::anyhow!("task join: {error}"))
                })?
        };

        match result {
            Ok(()) => {
                {
                    let conn = self.lock_db()?;
                    db::lifecycle::clear_archive_state(&conn, concert_id)
                        .map_err(ConcertCommandError::Operational)?;
                }
                tracing::info!(concert_id, "unarchive completed");
                self.command_state(concert_id)
            }
            Err(error) => {
                let message = format!("{error:#}");
                tracing::warn!(concert_id, %message, "unarchive failed");
                {
                    let conn = self.lock_db()?;
                    // Failure history is best-effort: the caller still learns the
                    // failure, and a broken record write must not mask it.
                    let _ = db::lifecycle::mark_archive_failed(&conn, concert_id, &message);
                    let _ = db::failed_jobs::insert_failed_job(
                        &conn,
                        concert_id,
                        "unarchive",
                        &message,
                    );
                }
                Err(ConcertCommandError::Operational(anyhow::anyhow!(message)))
            }
        }
    }

    // ── Scrape ──────────────────────────────────────────────────────────────
    //
    // Scrapes are their own protocol: they run on the Scrape Queue's worker, not
    // as a Job Run (see ADR-0006). These methods never invent a Job Run for one.

    /// Re-fetch this concert's metadata from its source page.
    pub async fn scrape(&self, concert_id: i64) -> Result<ConcertState, ConcertCommandError> {
        let source_url = {
            let conn = self.lock_db()?;
            self.load(&conn, concert_id)?.concert.source_url
        };
        tracing::info!(concert_id, "re-scrape started");
        self.fetch_metadata(concert_id, source_url).await?;
        tracing::info!(concert_id, "re-scrape completed");
        self.command_state(concert_id)
    }

    /// Queue a background scrape. The interface decides whether one is needed and
    /// whether it is already queued, so no adapter has to read the scrape queue
    /// or the metadata timestamp to decide.
    pub fn enqueue_scrape(&self, concert_id: i64) -> Result<ScrapeAdmission, ConcertCommandError> {
        let concert = {
            let conn = self.lock_db()?;
            self.load(&conn, concert_id)?.concert
        };
        if concert.metadata_scraped_at.is_some() {
            return Ok(ScrapeAdmission::NotNeeded);
        }
        Ok(
            if self.scrape_queue.enqueue(concert_id, concert.source_url) {
                ScrapeAdmission::Enqueued
            } else {
                ScrapeAdmission::AlreadyQueued
            },
        )
    }

    /// Scrape metadata only when the concert has none, tolerating failure: the
    /// command that wanted the metadata proceeds without it, as it always has.
    pub async fn ensure_scraped(&self, concert_id: i64) -> Result<(), ConcertCommandError> {
        let concert = {
            let conn = self.lock_db()?;
            self.load(&conn, concert_id)?.concert
        };
        if concert.metadata_scraped_at.is_some() {
            return Ok(());
        }
        tracing::info!(concert_id, "auto-scrape started");
        match self
            .fetch_metadata(concert_id, concert.source_url.clone())
            .await
        {
            Ok(()) => tracing::info!(concert_id, "auto-scrape completed"),
            Err(error) => tracing::warn!(concert_id, %error, "auto-scrape failed"),
        }
        Ok(())
    }

    /// Fetch and apply metadata for one concert. The scraper's HTTP client
    /// cannot run inside a runtime, so the work is handed to a blocking thread.
    async fn fetch_metadata(
        &self,
        concert_id: i64,
        source_url: String,
    ) -> Result<(), ConcertCommandError> {
        let db = self.db.clone();
        let working_dir = self.working_dir.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            let conn = db.lock().unwrap();
            crate::scrape::scrape_url(&conn, &source_url, &working_dir)
        })
        .await
        .map_err(|error| {
            ConcertCommandError::Operational(anyhow::anyhow!("scrape task join: {error}"))
        })?;
        outcome.map_err(|error| {
            tracing::warn!(concert_id, %error, "scrape failed");
            ConcertCommandError::Operational(error)
        })
    }

    // ── Split timestamps ────────────────────────────────────────────────────

    /// The concert's automatic and desired split timestamps, plus the source
    /// duration the editor needs for its scale.
    pub async fn split_timestamps(
        &self,
        concert_id: i64,
    ) -> Result<crate::split_timestamps::SplitTimestampsRead, ConcertCommandError> {
        crate::split_timestamps::read_split_timestamps(
            self.db.clone(),
            &self.working_dir,
            concert_id,
        )
        .await
        .map_err(|error| split_timestamp_rejection(concert_id, error))
    }

    /// Set the desired split timestamps and start the split that makes them
    /// real. Validation and coverage failures are rejections, so an adapter does
    /// not have to guess which of them is a client error.
    pub async fn set_split_timestamps(
        &self,
        concert_id: i64,
        payload: crate::split_timestamps::TimestampPayload,
    ) -> Result<SplitStart, ConcertCommandError> {
        self.ensure_source_observable(concert_id)?;
        let jobs = self.job_config()?;
        let outcome = crate::split_timestamps::apply_user_timestamps(
            self.db.clone(),
            self.registry.clone(),
            jobs,
            concert_id,
            payload,
        )
        .await
        .map_err(|error| split_timestamp_rejection(concert_id, error))?;
        tracing::info!(concert_id, ?outcome, "applied desired split timestamps");
        Ok(SplitStart {
            outcome,
            state: self.command_state(concert_id)?,
        })
    }

    /// Reset to the automatic split timestamps and start the split that applies
    /// them.
    pub async fn reset_split_timestamps(
        &self,
        concert_id: i64,
    ) -> Result<SplitStart, ConcertCommandError> {
        // Resetting does not read the source file — it drops the desired
        // timestamps and replays the automatic ones — so an absent or
        // unobservable source must not turn "already auto" or "no automated
        // timestamps" into a conflict. The workflow reports those itself.
        let jobs = self.job_config()?;
        let outcome = crate::split_timestamps::reset_to_auto_timestamps(
            self.db.clone(),
            self.registry.clone(),
            jobs,
            concert_id,
        )
        .await
        .map_err(|error| split_timestamp_rejection(concert_id, error))?;
        tracing::info!(concert_id, ?outcome, "reset to automatic split timestamps");
        Ok(SplitStart {
            outcome,
            state: self.command_state(concert_id)?,
        })
    }

    /// Timestamp work needs the source file. Unknown media facts are not the
    /// same as an absent file: when the source could not be observed at all, the
    /// command is refused rather than attempted on a guess. An observed-but-gone
    /// source is left to the workflow, which owns the user-facing "download the
    /// concert first" rejection.
    fn ensure_source_observable(&self, concert_id: i64) -> Result<(), ConcertCommandError> {
        let state = self.command_state(concert_id)?;
        match state.media.source {
            Observation::Known(_) => Ok(()),
            Observation::Unknown(_) => {
                Err(ConcertCommandError::Rejected(CommandRejection::Conflict {
                    concert_id,
                    message: "the source media could not be observed".to_string(),
                }))
            }
        }
    }

    // ── Deletion ────────────────────────────────────────────────────────────
    //
    // Every deletion states the desired end result — the media is absent — so a
    // repeated request after it is gone succeeds instead of reporting a
    // failure. Invalid identities are still rejections: retrying a request for
    // a track that never existed must not look like a success.

    /// Delete the concert's downloaded source file, clearing the recorded
    /// download.
    ///
    /// With [`DeletionConfirmation::Unconfirmed`], a record that claims a
    /// download while the file is already gone asks for confirmation instead of
    /// silently rewriting that record.
    pub fn delete_download(
        &self,
        concert_id: i64,
        confirmation: DeletionConfirmation,
    ) -> Result<DeletionOutcome, ConcertCommandError> {
        self.load_command_concert(concert_id)?;
        let force = confirmation == DeletionConfirmation::Confirmed;
        let outcome = {
            let conn = self.lock_db()?;
            crate::lifecycle::delete_download(&conn, &self.working_dir, concert_id, force)
                .map_err(ConcertCommandError::Operational)?
        };
        match outcome {
            crate::lifecycle::DeleteDownloadOutcome::Deleted { removed_file } => {
                tracing::info!(
                    concert_id,
                    removed = removed_file.as_ref().map(|p| p.display().to_string()),
                    "deleted concert download"
                );
                Ok(DeletionOutcome::Deleted(self.command_state(concert_id)?))
            }
            // Already absent: the desired end state holds, so this is a
            // successful retry rather than an error.
            crate::lifecycle::DeleteDownloadOutcome::NotDownloaded => {
                tracing::info!(concert_id, "concert has no download to delete");
                Ok(DeletionOutcome::Deleted(self.command_state(concert_id)?))
            }
            crate::lifecycle::DeleteDownloadOutcome::MissingFileRequiresConfirmation => {
                tracing::info!(
                    concert_id,
                    "concert is recorded as downloaded but its file is gone; confirmation required"
                );
                Ok(DeletionOutcome::ConfirmationRequired(
                    self.command_state(concert_id)?,
                ))
            }
        }
    }

    /// Delete the source file once song and interlude media fully cover it.
    pub fn delete_redundant_source(
        &self,
        concert_id: i64,
    ) -> Result<DeletionOutcome, ConcertCommandError> {
        // Decide from what is on disk, not from the recorded lifecycle: a source
        // copied in by hand (or restored from a backup) has no `downloaded_at`
        // but is still there to delete.
        let state = self.command_state(concert_id)?;
        match state.media.source {
            Observation::Known(SourceMediaState { path: None }) => {
                tracing::info!(concert_id, "no source file to delete; already absent");
                return Ok(DeletionOutcome::Deleted(state));
            }
            // Leave the decision to the lifecycle code, which re-checks coverage
            // and refuses to delete a source that songs and interludes do not
            // fully cover.
            Observation::Known(SourceMediaState { path: Some(_) }) | Observation::Unknown(_) => {}
        }
        let outcome = {
            let conn = self.lock_db()?;
            crate::lifecycle::delete_redundant_source(&conn, &self.working_dir, concert_id)
                .map_err(ConcertCommandError::Operational)?
        };
        match outcome {
            crate::lifecycle::DeleteRedundantSourceOutcome::Deleted { removed_file } => {
                tracing::info!(
                    concert_id,
                    removed = removed_file.as_ref().map(|p| p.display().to_string()),
                    "deleted redundant concert source"
                );
                Ok(DeletionOutcome::Deleted(self.command_state(concert_id)?))
            }
            crate::lifecycle::DeleteRedundantSourceOutcome::NotRedundant => Err(
                ConcertCommandError::Rejected(CommandRejection::SourceNotRedundant { concert_id }),
            ),
        }
    }

    /// Delete the split state, leaving the media as it is.
    pub fn delete_split(&self, concert_id: i64) -> Result<DeletionOutcome, ConcertCommandError> {
        self.load_command_concert(concert_id)?;
        let outcome = {
            let conn = self.lock_db()?;
            crate::lifecycle::delete_split(&conn, concert_id)
                .map_err(ConcertCommandError::Operational)?
        };
        match outcome {
            crate::lifecycle::DeleteSplitOutcome::Deleted => {
                tracing::info!(concert_id, "deleted concert split state");
            }
            // Already absent: a retried deletion is a success.
            crate::lifecycle::DeleteSplitOutcome::NoSplitState => {
                tracing::info!(concert_id, "concert has no split state to delete");
            }
        }
        Ok(DeletionOutcome::Deleted(self.command_state(concert_id)?))
    }

    /// Delete one track's media files.
    pub fn delete_track(
        &self,
        concert_id: i64,
        track_index: usize,
    ) -> Result<DeletionOutcome, ConcertCommandError> {
        let concert = self.load_command_concert(concert_id)?;
        if track_index >= concert.set_list.len() {
            return Err(ConcertCommandError::Rejected(
                CommandRejection::NoSuchTrack {
                    concert_id,
                    track_index,
                },
            ));
        }
        let outcome = {
            let conn = self.lock_db()?;
            crate::lifecycle::delete_track(&conn, &self.working_dir, concert_id, track_index)
                .map_err(ConcertCommandError::Operational)?
        };
        tracing::info!(
            concert_id,
            track_index,
            removed = outcome.removed_files.len(),
            split_cleared = outcome.split_cleared,
            "deleted concert track"
        );
        Ok(DeletionOutcome::Deleted(self.command_state(concert_id)?))
    }

    /// Delete one interlude's media file. A concert without an album has no
    /// interlude files, which is a rejection rather than a silent success: the
    /// request names media that cannot exist.
    pub fn delete_interlude(
        &self,
        concert_id: i64,
        interlude_index: usize,
    ) -> Result<DeletionOutcome, ConcertCommandError> {
        let concert = self.load_command_concert(concert_id)?;
        if concert.album.is_none() {
            return Err(ConcertCommandError::Rejected(
                CommandRejection::MissingMedia {
                    concert_id,
                    target: ListenTarget::Source,
                },
            ));
        }
        let outcome = {
            let conn = self.lock_db()?;
            crate::lifecycle::delete_interlude(
                &conn,
                &self.working_dir,
                concert_id,
                interlude_index,
            )
            .map_err(ConcertCommandError::Operational)?
        };
        tracing::info!(
            concert_id,
            interlude_index,
            removed = outcome.removed_files.len(),
            "deleted concert interlude"
        );
        Ok(DeletionOutcome::Deleted(self.command_state(concert_id)?))
    }

    /// The Concert State a command reports, mapping a vanished concert onto the
    /// command's own error type.
    fn command_state(&self, concert_id: i64) -> Result<ConcertState, ConcertCommandError> {
        self.get(concert_id).map_err(ConcertCommandError::from)
    }

    // ── Playback resolution ─────────────────────────────────────────────────
    //
    // These return presentation-neutral media descriptions. Choosing between a
    // browser player and a system player, and formatting a URL, stay adapter
    // concerns. None of them records a listen.

    /// What to play for a concert as a whole: its source file when present,
    /// otherwise the reconstructed song/interlude sequence.
    pub fn playback(
        &self,
        concert_id: i64,
    ) -> Result<crate::playback::PlaybackPlan, ConcertPlaybackError> {
        let (concert, user_split_timestamps) = self.load_for_media(concert_id)?;
        crate::playback::concert_playback_plan(
            &self.working_dir,
            &concert,
            user_split_timestamps.as_deref(),
        )
        .map_err(ConcertPlaybackError::Lookup)
    }

    /// The concert's source file as whole-album playback.
    pub fn source_media(
        &self,
        concert_id: i64,
    ) -> Result<crate::playback::SourceMedia, ConcertPlaybackError> {
        let (concert, _) = self.load_for_media(concert_id)?;
        crate::playback::source_media(&self.working_dir, &concert)
            .map_err(ConcertPlaybackError::Lookup)
    }

    /// One set-list track, with its navigation and like facts.
    pub fn track_media(
        &self,
        concert_id: i64,
        track_index: usize,
    ) -> Result<crate::playback::TrackMedia, ConcertPlaybackError> {
        let (concert, _) = self.load_for_media(concert_id)?;
        crate::playback::track_media(&self.working_dir, &concert, track_index)
            .map_err(ConcertPlaybackError::Lookup)
    }

    /// The next playable track after `after_index`.
    pub fn next_track_media(
        &self,
        concert_id: i64,
        after_index: usize,
    ) -> Result<crate::playback::TrackMedia, ConcertPlaybackError> {
        let (concert, _) = self.load_for_media(concert_id)?;
        crate::playback::next_track_media(&self.working_dir, &concert, after_index)
            .map_err(ConcertPlaybackError::Lookup)
    }

    /// The previous playable track before `before_index`.
    pub fn prev_track_media(
        &self,
        concert_id: i64,
        before_index: usize,
    ) -> Result<crate::playback::TrackMedia, ConcertPlaybackError> {
        let (concert, _) = self.load_for_media(concert_id)?;
        crate::playback::prev_track_media(&self.working_dir, &concert, before_index)
            .map_err(ConcertPlaybackError::Lookup)
    }

    /// The reconstructed song/interlude sequence, for callers that render the
    /// sequence itself rather than play it.
    pub fn reconstruction_items(
        &self,
        concert_id: i64,
    ) -> Result<Vec<PlaybackItem>, ConcertPlaybackError> {
        let (concert, user_split_timestamps) = self.load_for_media(concert_id)?;
        crate::playback::reconstruction_items(
            &self.working_dir,
            &concert,
            user_split_timestamps.as_deref(),
        )
        .map_err(ConcertPlaybackError::Lookup)
    }

    /// Per-track availability, video flag, and like state, probed on disk.
    pub fn track_details(
        &self,
        concert_id: i64,
    ) -> Result<Vec<crate::model::TrackDetailItem>, ConcertPlaybackError> {
        let (concert, _) = self.load_for_media(concert_id)?;
        crate::playback::track_details(&self.working_dir, &concert)
            .map_err(ConcertPlaybackError::Lookup)
    }

    /// The file a system player should open for `target`, resolved through
    /// Concert Media Inventory. Opening it is the adapter's choice.
    ///
    /// Reports the same media outcomes as playback resolution, so a concert
    /// recorded as downloaded whose file has vanished stays distinguishable
    /// from one that was never downloaded.
    pub fn media_file(
        &self,
        concert_id: i64,
        target: ListenTarget,
    ) -> Result<PathBuf, ConcertPlaybackError> {
        let (concert, _) = self.load_for_media(concert_id)?;
        match target.track_index() {
            None => crate::playback::source_file(&self.working_dir, &concert),
            Some(index) => crate::playback::track_file(&self.working_dir, &concert, index),
        }
        .map_err(ConcertPlaybackError::Lookup)
    }

    // ── Listens and likes ───────────────────────────────────────────────────

    /// Record that the listener listened to `target`. Rejects when there is
    /// nothing to listen to, so a failed play does not become audit history.
    pub fn record_listen(
        &self,
        concert_id: i64,
        target: ListenTarget,
    ) -> Result<ConcertState, ConcertCommandError> {
        self.record_media_event(concert_id, target, crate::events::Event::Listen)
    }

    /// Record that the listener opened `target` in a system player. Like
    /// [`Self::record_listen`], this is explicit and separate from resolving
    /// which file the player should open.
    pub fn record_watch(
        &self,
        concert_id: i64,
        target: ListenTarget,
    ) -> Result<ConcertState, ConcertCommandError> {
        self.record_media_event(concert_id, target, crate::events::Event::Watch)
    }

    /// Set the desired like state for one track. Rejects a track that is not in
    /// the set list, and one whose file is absent — the interface will not
    /// record a like for a track that is not there.
    pub fn set_track_liked(
        &self,
        concert_id: i64,
        track_index: usize,
        liked: bool,
    ) -> Result<ConcertState, ConcertCommandError> {
        let concert = self.load_command_concert(concert_id)?;
        if track_index >= concert.set_list.len() {
            return Err(ConcertCommandError::Rejected(
                CommandRejection::NoSuchTrack {
                    concert_id,
                    track_index,
                },
            ));
        }
        if !crate::model::is_track_available(&concert.tracks_present, track_index) {
            tracing::debug!(
                concert_id,
                track_index,
                "refusing to set like state on an unavailable track"
            );
            return Err(ConcertCommandError::Rejected(
                CommandRejection::MissingMedia {
                    concert_id,
                    target: ListenTarget::Track { index: track_index },
                },
            ));
        }
        {
            let conn = self.lock_db()?;
            db::split_timestamps::set_track_liked(&conn, concert_id, track_index, liked)
                .map_err(ConcertCommandError::Operational)?;
        }
        tracing::info!(concert_id, track_index, liked, "set desired like state");
        self.get(concert_id).map_err(ConcertCommandError::from)
    }

    fn record_media_event(
        &self,
        concert_id: i64,
        target: ListenTarget,
        event: crate::events::Event,
    ) -> Result<ConcertState, ConcertCommandError> {
        let concert = self.load_command_concert(concert_id)?;
        let json = listen_payload(&concert, target)?;
        let path = self
            .media_file(concert_id, target)
            .map_err(|error| match error {
                // "Nothing to listen to" is an expected rejection: the click is
                // well formed, the media simply is not there.
                ConcertPlaybackError::Lookup(_) => {
                    ConcertCommandError::Rejected(CommandRejection::MissingMedia {
                        concert_id,
                        target,
                    })
                }
                other => ConcertCommandError::Operational(anyhow::anyhow!("{other}")),
            })?;
        tracing::info!(
            concert_id,
            media = %path.display(),
            target = %target.describe(),
            event = ?event,
            "recorded media event"
        );
        {
            let conn = self.lock_db()?;
            crate::events::record_now(&conn, concert_id, event, json.as_deref());
        }
        self.get(concert_id).map_err(ConcertCommandError::from)
    }

    /// Load a concert for a command, mapping absence to a typed rejection.
    fn load_command_concert(&self, concert_id: i64) -> Result<Concert, ConcertCommandError> {
        let conn = self.lock_db()?;
        Ok(self.load(&conn, concert_id)?.concert)
    }

    /// Load a concert plus its user split timestamps for media observation.
    fn load_for_media(
        &self,
        concert_id: i64,
    ) -> Result<(Concert, Option<Vec<concert_types::SongTimestamp>>), ConcertPlaybackError> {
        let loaded = {
            let conn = self.lock_db().map_err(ConcertPlaybackError::Query)?;
            self.load(&conn, concert_id).map_err(|error| match error {
                ConcertQueryError::NotFound { concert_id } => {
                    ConcertPlaybackError::NotFound { concert_id }
                }
                error => ConcertPlaybackError::Query(error),
            })?
        };
        Ok((loaded.concert, loaded.user_split_timestamps))
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
        self.load(conn, concert_id).map(|_| ())
    }

    /// Read one concert and its separately stored timestamps under the caller's
    /// database lock. Typed `NotFound` for an absent row.
    fn load(&self, conn: &Connection, concert_id: i64) -> Result<LoadedConcert, ConcertQueryError> {
        let concert = db::concerts::get_concert(conn, concert_id).map_err(|error| {
            if matches!(
                error.downcast_ref::<rusqlite::Error>(),
                Some(rusqlite::Error::QueryReturnedNoRows)
            ) {
                ConcertQueryError::NotFound { concert_id }
            } else {
                ConcertQueryError::Operational(error)
            }
        })?;
        let user_split_timestamps = db::split_timestamps::get_split_timestamps(conn, concert_id)
            .map_err(ConcertQueryError::Operational)?
            .user;
        Ok(LoadedConcert {
            concert,
            user_split_timestamps,
        })
    }

    /// Build the canonical Concert State for a loaded concert. Needs no database
    /// lock: everything left to observe is media on disk and in-memory work.
    fn observe(&self, loaded: LoadedConcert, archive_configured: bool) -> ConcertState {
        let LoadedConcert {
            concert,
            user_split_timestamps,
        } = loaded;
        let concert_id = concert.id;
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
        ConcertState {
            concert,
            tracks,
            media,
            active_work,
            archive_configured,
            permitted_actions,
        }
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

/// Media facts a listen/watch event records: the track's title and the event's
/// JSON payload. `None` for a whole-concert event, matching the history this
/// recording has always written.
fn listen_payload(
    concert: &Concert,
    target: ListenTarget,
) -> Result<Option<String>, ConcertCommandError> {
    let Some(index) = target.track_index() else {
        return Ok(None);
    };
    let Some(title) = concert.set_list.get(index) else {
        return Err(ConcertCommandError::Rejected(
            CommandRejection::NoSuchTrack {
                concert_id: concert.id,
                track_index: index,
            },
        ));
    };
    Ok(Some(
        serde_json::json!({"track_index": index, "track_title": title}).to_string(),
    ))
}

/// Map the split-timestamp workflow's outcomes onto the interface's rejection
/// kinds, keeping the distinction adapters need (404 vs 409 vs 422 vs 500).
fn split_timestamp_rejection(
    concert_id: i64,
    error: crate::split_timestamps::SplitTimestampWorkflowError,
) -> ConcertCommandError {
    use crate::split_timestamps::SplitTimestampWorkflowError as WorkflowError;
    match error {
        WorkflowError::NotFound => {
            ConcertCommandError::Rejected(CommandRejection::NotFound { concert_id })
        }
        WorkflowError::Conflict(message) => {
            ConcertCommandError::Rejected(CommandRejection::Conflict {
                concert_id,
                message,
            })
        }
        WorkflowError::Unprocessable(message) => {
            ConcertCommandError::Rejected(CommandRejection::Invalid {
                concert_id,
                message,
            })
        }
        WorkflowError::Internal(error) => ConcertCommandError::Operational(error),
    }
}

fn archive_configured(conn: &Connection) -> Result<bool, ConcertQueryError> {
    db::settings::get_settings(conn)
        .map(|settings| settings.archive_location.is_some())
        .map_err(ConcertQueryError::Operational)
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
    use crate::db::seeds::{
        SeedContext, SeedLifecycleConcert, SeedListing, SeedMediaConcert, SeedScrapedConcert,
    };
    use crate::jobs::scrape_queue::ScrapeQueue;
    use crate::playback::PlaybackPlan;
    use crate::split_timestamps::TimestampPayload;

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
            crate::jobs::JobConfig::test(working_dir.path().to_path_buf()),
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
            crate::jobs::JobConfig::test(working_dir.path().to_path_buf()),
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
            crate::jobs::JobConfig::test(working_dir.path().to_path_buf()),
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
        let concerts = Concerts::new(
            db,
            working_dir.path().to_path_buf(),
            registry,
            scrape_queue,
            crate::jobs::JobConfig::test(working_dir.path().to_path_buf()),
        );

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

    /// Build the interface over a seeded in-memory database. `without_workers`
    /// keeps these tests free of a tokio runtime: no Job Registry or scrape
    /// worker exists, so active work comes from persistence alone.
    fn one_shot(conn: rusqlite::Connection, working_dir: &std::path::Path) -> Concerts {
        Concerts::without_workers(Arc::new(Mutex::new(conn)), working_dir.to_path_buf())
    }

    /// Every event the concert has recorded so far, in order. The matrix below
    /// compares whole histories rather than only the status events, so an extra
    /// event of any kind — or a missing one — is a failure.
    fn event_names(concerts: &Concerts, concert_id: i64) -> Vec<String> {
        concerts
            .event_history(concert_id)
            .unwrap()
            .into_iter()
            .map(|event| event.event)
            .collect()
    }

    /// The one event a status transition records, derived from the event stream
    /// the toggle-shaped callers have always produced (see
    /// `docs/change/2026-07-25-concert-catalog-and-status.md`, "Preserved
    /// behaviour") rather than from the storage: entering a status records it,
    /// leaving one for Available records its delete, and a direct Wanted/Ignored
    /// swap records only the status entered. A repeat of the same status records
    /// nothing.
    fn expected_status_event(from: ConcertStatus, to: ConcertStatus) -> Option<&'static str> {
        match (from, to) {
            // A repeat of the status the concert already has. `set_concert_status`
            // returns early for these, so no event is recorded.
            (ConcertStatus::Wanted, ConcertStatus::Wanted)
            | (ConcertStatus::Ignored, ConcertStatus::Ignored)
            | (ConcertStatus::Available, ConcertStatus::Available) => None,
            (ConcertStatus::Wanted, ConcertStatus::Available) => Some("wanted_delete"),
            (ConcertStatus::Ignored, ConcertStatus::Available) => Some("ignored_delete"),
            (_, ConcertStatus::Wanted) => Some("wanted"),
            (_, ConcertStatus::Ignored) => Some("ignored"),
        }
    }

    /// Every ordered transition between Available, Wanted, and Ignored, through
    /// the interface callers use: one mutually exclusive status, a command result
    /// that is the concert's actual state, one event per transition, and a retry
    /// that changes nothing.
    #[test]
    fn every_status_transition_sets_one_exclusive_status_and_one_event() {
        let statuses = [
            ConcertStatus::Available,
            ConcertStatus::Wanted,
            ConcertStatus::Ignored,
        ];
        for from in statuses {
            for to in statuses {
                let working_dir = tempfile::tempdir().unwrap();
                let conn = db::connection::open_in_memory().unwrap();
                let concert = SeedContext::new(&conn)
                    .seed_listing(db::seeds::SeedListing::default())
                    .unwrap();
                let concerts = one_shot(conn, working_dir.path());

                let starting = concerts.set_status(concert.id, from).unwrap();
                assert_eq!(
                    starting.concert.concert_status(),
                    from,
                    "setting the starting status from a fresh Available concert"
                );
                let mut expected = event_names(&concerts, concert.id);

                let state = concerts.set_status(concert.id, to).unwrap();

                // Available, Wanted, and Ignored are mutually exclusive: the
                // status asked for is expressed by exactly one stored intent.
                assert_eq!(state.concert.concert_status(), to, "{from:?} -> {to:?}");
                assert_eq!(state.concert.wanted, to == ConcertStatus::Wanted);
                assert_eq!(state.concert.ignored, to == ConcertStatus::Ignored);

                // The returned state is the concert's state, not a guess about
                // it: the next observation agrees.
                assert_eq!(
                    concerts.get(concert.id).unwrap().concert.concert_status(),
                    to,
                    "{from:?} -> {to:?}: the returned state must be the observed state"
                );

                if let Some(event) = expected_status_event(from, to) {
                    expected.push(event.to_string());
                }
                assert_eq!(
                    event_names(&concerts, concert.id),
                    expected,
                    "{from:?} -> {to:?} must append exactly {expected:?}"
                );

                // Repeating the desired status is safe: same state, no event.
                let retried = concerts.set_status(concert.id, to).unwrap();
                assert_eq!(retried.concert.concert_status(), to, "{from:?} -> {to:?}");
                assert_eq!(
                    event_names(&concerts, concert.id),
                    expected,
                    "{from:?} -> {to:?}: a retried request records no second event"
                );
            }
        }
    }

    #[test]
    fn becoming_ignored_discards_cached_images() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_media_concert(
                working_dir.path(),
                SeedMediaConcert {
                    lifecycle: SeedLifecycleConcert {
                        album: Some("Some Album".to_string()),
                        ..SeedLifecycleConcert::default()
                    },
                    preview_image: true,
                    ..SeedMediaConcert::default()
                },
            )
            .unwrap();
        let album = concert.album.clone().unwrap();
        let preview = crate::scrape::preview_image_path(working_dir.path(), &album);
        assert!(preview.exists());
        let concerts = one_shot(conn, working_dir.path());

        concerts
            .set_status(concert.id, ConcertStatus::Ignored)
            .unwrap();

        assert!(!preview.exists(), "cached preview image should be gone");

        // Going back to Available must not need the cache or fail without it.
        concerts
            .set_status(concert.id, ConcertStatus::Available)
            .unwrap();
    }

    #[test]
    fn set_notes_stores_the_desired_text_and_returns_state() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_listing(db::seeds::SeedListing::default())
            .unwrap();
        let concerts = one_shot(conn, working_dir.path());

        let state = concerts.set_notes(concert.id, "saw this live").unwrap();
        assert_eq!(state.concert.notes.as_deref(), Some("saw this live"));

        // Repeating the same text is safe and reports the text now stored.
        let retried = concerts.set_notes(concert.id, "saw this live").unwrap();
        assert_eq!(retried.concert.notes.as_deref(), Some("saw this live"));
        assert_eq!(
            concerts.get(concert.id).unwrap().concert.notes.as_deref(),
            Some("saw this live"),
            "the returned state must be the observed state"
        );

        let cleared = concerts.set_notes(concert.id, "").unwrap();
        assert_eq!(cleared.concert.notes.as_deref(), Some(""));
    }

    /// Notes and Concert Status are independent desired state: writing one never
    /// clears the other.
    #[test]
    fn notes_and_status_are_independent_desired_state() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_listing(db::seeds::SeedListing::default())
            .unwrap();
        let concerts = one_shot(conn, working_dir.path());

        concerts
            .set_status(concert.id, ConcertStatus::Ignored)
            .unwrap();
        let noted = concerts.set_notes(concert.id, "skip for now").unwrap();
        assert_eq!(noted.concert.concert_status(), ConcertStatus::Ignored);
        assert_eq!(noted.concert.notes.as_deref(), Some("skip for now"));

        let wanted = concerts
            .set_status(concert.id, ConcertStatus::Wanted)
            .unwrap();
        assert_eq!(wanted.concert.concert_status(), ConcertStatus::Wanted);
        assert_eq!(
            wanted.concert.notes.as_deref(),
            Some("skip for now"),
            "changing Concert Status must not clear the notes"
        );

        // The other direction: notes that already exist survive becoming Ignored.
        let available = concerts
            .set_status(concert.id, ConcertStatus::Available)
            .unwrap();
        assert_eq!(available.concert.concert_status(), ConcertStatus::Available);
        concerts.set_notes(concert.id, "revisit later").unwrap();
        let ignored = concerts
            .set_status(concert.id, ConcertStatus::Ignored)
            .unwrap();
        assert_eq!(
            ignored.concert.notes.as_deref(),
            Some("revisit later"),
            "becoming Ignored must not clear the notes"
        );
    }

    #[test]
    fn missing_concerts_are_typed_rejections_not_operational_failures() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concerts = one_shot(conn, working_dir.path());
        let missing = i64::MAX;

        let status_error = concerts
            .set_status(missing, ConcertStatus::Wanted)
            .unwrap_err();
        assert!(
            matches!(
                status_error,
                ConcertCommandError::Rejected(CommandRejection::NotFound { concert_id })
                    if concert_id == missing
            ),
            "{status_error}"
        );

        let notes_error = concerts.set_notes(missing, "notes").unwrap_err();
        assert!(
            matches!(
                notes_error,
                ConcertCommandError::Rejected(CommandRejection::NotFound { concert_id })
                    if concert_id == missing
            ),
            "{notes_error}"
        );
    }

    #[test]
    fn persistence_failures_are_operational_command_errors() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_listing(db::seeds::SeedListing::default())
            .unwrap();
        let concerts = one_shot(conn, working_dir.path());
        {
            let conn = concerts.lock_db().unwrap();
            conn.execute_batch("DROP TABLE concerts").unwrap();
        }

        assert!(matches!(
            concerts.set_status(concert.id, ConcertStatus::Wanted),
            Err(ConcertCommandError::Operational(_))
        ));
        assert!(matches!(
            concerts.set_notes(concert.id, "notes"),
            Err(ConcertCommandError::Operational(_))
        ));
    }

    /// A media fixture. `tracks_present` mirrors `track_files`, because
    /// persistence — not the filesystem — is the source of truth for whether a
    /// track exists; the files are what playback resolves.
    fn seeded_media(
        seeds: &SeedContext,
        working_dir: &std::path::Path,
        set_list: Vec<String>,
        source_file: bool,
        track_files: Option<Vec<usize>>,
    ) -> Concert {
        let tracks_present = track_files.as_ref().map(|files| {
            (0..set_list.len())
                .map(|index| files.contains(&index))
                .collect::<Vec<bool>>()
        });
        seeds
            .seed_media_concert(
                working_dir,
                SeedMediaConcert {
                    lifecycle: SeedLifecycleConcert {
                        set_list: Some(set_list),
                        downloaded: true,
                        split: track_files.is_some(),
                        tracks_present,
                        ..SeedLifecycleConcert::default()
                    },
                    source_file,
                    track_files,
                    ..SeedMediaConcert::default()
                },
            )
            .unwrap()
    }

    fn three_songs() -> Vec<String> {
        vec![
            "First".to_string(),
            "Second".to_string(),
            "Third".to_string(),
        ]
    }

    #[test]
    fn playback_prefers_the_source_file_then_the_published_split() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let seeds = SeedContext::new(&conn);
        // Source file and a complete split, so removing the source exercises the
        // reconstruction fallback.
        let with_source = seeded_media(
            &seeds,
            working_dir.path(),
            three_songs(),
            true,
            Some(vec![0, 1, 2]),
        );
        let concerts = one_shot(conn, working_dir.path());

        let plan = concerts.playback(with_source.id).unwrap();
        let PlaybackPlan::Source(source) = plan else {
            panic!("expected source playback, got reconstruction");
        };
        assert_eq!(source.title, with_source.album.clone().unwrap());

        // Without the source file, the same concert plays from its Published
        // Concert Split.
        std::fs::remove_file(
            crate::model::concert_dir(working_dir.path(), with_source.album.as_deref().unwrap())
                .join(&source.filename),
        )
        .unwrap();
        match concerts.playback(with_source.id).unwrap() {
            PlaybackPlan::Source(_) => panic!("expected reconstruction playback"),
            PlaybackPlan::Reconstruction(items) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0].title, "First");
                assert!(items.iter().all(|item| item.is_song()));
            }
        }
    }

    #[test]
    fn playback_and_media_queries_are_typed_for_absent_concerts() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concerts = one_shot(conn, working_dir.path());
        let missing = i64::MAX;

        assert!(matches!(
            concerts.playback(missing),
            Err(ConcertPlaybackError::NotFound { concert_id }) if concert_id == missing
        ));
        assert!(matches!(
            concerts.track_media(missing, 0),
            Err(ConcertPlaybackError::NotFound { concert_id }) if concert_id == missing
        ));
        assert!(matches!(
            concerts.track_details(missing),
            Err(ConcertPlaybackError::NotFound { concert_id }) if concert_id == missing
        ));
    }

    #[test]
    fn playback_reports_nothing_to_play_without_media() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_media_concert(
                working_dir.path(),
                SeedMediaConcert {
                    lifecycle: SeedLifecycleConcert {
                        set_list: Some(three_songs()),
                        ..SeedLifecycleConcert::default()
                    },
                    ..SeedMediaConcert::default()
                },
            )
            .unwrap();
        let concerts = one_shot(conn, working_dir.path());

        assert!(matches!(
            concerts.playback(concert.id),
            Err(ConcertPlaybackError::Lookup(
                crate::playback::PlaybackLookupError::NotPlayable
            ))
        ));
        assert!(matches!(
            concerts.source_media(concert.id),
            Err(ConcertPlaybackError::Lookup(
                crate::playback::PlaybackLookupError::NotPlayable
            ))
        ));
    }

    #[test]
    fn track_navigation_skips_tracks_without_files() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let seeds = SeedContext::new(&conn);
        // Only the first and third tracks were split.
        let concert = seeded_media(
            &seeds,
            working_dir.path(),
            three_songs(),
            true,
            Some(vec![0, 2]),
        );
        let concerts = one_shot(conn, working_dir.path());

        let first = concerts.track_media(concert.id, 0).unwrap();
        assert!(first.has_next, "third track is playable");
        assert!(!first.has_prev);
        assert!(!first.liked);

        let third = concerts.next_track_media(concert.id, 0).unwrap();
        assert_eq!(third.track_index, 2);
        assert_eq!(third.title, "Third");
        assert!(third.has_prev);
        assert!(!third.has_next);

        // The middle track has no file, so navigating from it goes backwards.
        let back = concerts.prev_track_media(concert.id, 2).unwrap();
        assert_eq!(back.track_index, 0);
        assert!(matches!(
            concerts.track_media(concert.id, 1),
            Err(ConcertPlaybackError::Lookup(
                crate::playback::PlaybackLookupError::NotPlayable
            ))
        ));
    }

    #[test]
    fn track_details_probe_files_on_disk() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let seeds = SeedContext::new(&conn);
        let concert = seeded_media(
            &seeds,
            working_dir.path(),
            three_songs(),
            true,
            Some(vec![0, 2]),
        );
        let concerts = one_shot(conn, working_dir.path());

        let details = concerts.track_details(concert.id).unwrap();
        let available: Vec<bool> = details.iter().map(|track| track.available).collect();
        assert_eq!(available, vec![true, false, true]);
    }

    #[test]
    fn media_file_resolves_the_source_and_track_paths() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let seeds = SeedContext::new(&conn);
        let concert = seeded_media(
            &seeds,
            working_dir.path(),
            three_songs(),
            true,
            Some(vec![1]),
        );
        let concerts = one_shot(conn, working_dir.path());

        let source = concerts
            .media_file(concert.id, ListenTarget::Source)
            .unwrap();
        assert!(source.exists(), "resolved source should exist");

        let track = concerts
            .media_file(concert.id, ListenTarget::Track { index: 1 })
            .unwrap();
        assert!(track.exists(), "resolved track should exist");

        // A track whose file is absent reports nothing to play...
        assert!(matches!(
            concerts.media_file(concert.id, ListenTarget::Track { index: 0 }),
            Err(ConcertPlaybackError::Lookup(
                crate::playback::PlaybackLookupError::NotPlayable
            ))
        ));
        // ...as does a track index outside the set list.
        assert!(matches!(
            concerts.media_file(concert.id, ListenTarget::Track { index: 9 }),
            Err(ConcertPlaybackError::Lookup(
                crate::playback::PlaybackLookupError::NotPlayable
            ))
        ));
    }

    #[test]
    fn resolving_playback_records_no_listen_and_the_command_records_one() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let seeds = SeedContext::new(&conn);
        let concert = seeded_media(
            &seeds,
            working_dir.path(),
            three_songs(),
            true,
            Some(vec![0]),
        );
        let concerts = one_shot(conn, working_dir.path());

        concerts.playback(concert.id).unwrap();
        concerts.track_media(concert.id, 0).unwrap();
        concerts.track_details(concert.id).unwrap();
        assert!(
            concerts.event_history(concert.id).unwrap().is_empty()
                || concerts
                    .event_history(concert.id)
                    .unwrap()
                    .iter()
                    .all(|event| event.event != "listen"),
            "resolving media must not record a listen"
        );

        let state = concerts
            .record_listen(concert.id, ListenTarget::Source)
            .unwrap();
        assert_eq!(state.concert.id, concert.id);
        let listens: Vec<String> = concerts
            .event_history(concert.id)
            .unwrap()
            .into_iter()
            .filter(|event| event.event == "listen")
            .map(|event| event.json.unwrap_or_default())
            .collect();
        assert_eq!(
            listens,
            vec![String::new()],
            "whole-concert listen has no payload"
        );

        // A track listen carries the track's index and title, as it always has.
        concerts
            .record_listen(concert.id, ListenTarget::Track { index: 0 })
            .unwrap();
        let track_listen = concerts
            .event_history(concert.id)
            .unwrap()
            .into_iter()
            .rfind(|event| event.event == "listen")
            .unwrap()
            .json
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&track_listen).unwrap();
        assert_eq!(json["track_index"], 0);
        assert_eq!(json["track_title"], "First");
    }

    #[test]
    fn listening_to_absent_media_is_a_rejection_that_records_nothing() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let seeds = SeedContext::new(&conn);
        // Split, but only the second track has a file.
        let concert = seeded_media(
            &seeds,
            working_dir.path(),
            three_songs(),
            false,
            Some(vec![1]),
        );
        let concerts = one_shot(conn, working_dir.path());
        let before = concerts.event_history(concert.id).unwrap().len();

        assert!(matches!(
            concerts.record_listen(concert.id, ListenTarget::Source),
            Err(ConcertCommandError::Rejected(
                CommandRejection::MissingMedia {
                    target: ListenTarget::Source,
                    ..
                }
            ))
        ));
        assert!(matches!(
            concerts.record_listen(concert.id, ListenTarget::Track { index: 0 }),
            Err(ConcertCommandError::Rejected(
                CommandRejection::MissingMedia { .. }
            ))
        ));
        assert_eq!(
            concerts.event_history(concert.id).unwrap().len(),
            before,
            "a rejected listen must not become audit history"
        );
    }

    #[test]
    fn record_watch_records_a_watch_event_for_the_opened_file() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let seeds = SeedContext::new(&conn);
        let concert = seeded_media(
            &seeds,
            working_dir.path(),
            three_songs(),
            true,
            Some(vec![2]),
        );
        let concerts = one_shot(conn, working_dir.path());

        concerts
            .record_watch(concert.id, ListenTarget::Track { index: 2 })
            .unwrap();

        let watch = concerts
            .event_history(concert.id)
            .unwrap()
            .into_iter()
            .find(|event| event.event == "watch")
            .expect("watch event recorded");
        let json: serde_json::Value = serde_json::from_str(watch.json.as_deref().unwrap()).unwrap();
        assert_eq!(json["track_index"], 2);
        assert_eq!(json["track_title"], "Third");
    }

    #[test]
    fn set_track_liked_stores_the_desired_state_and_is_idempotent() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let seeds = SeedContext::new(&conn);
        let concert = seeded_media(
            &seeds,
            working_dir.path(),
            three_songs(),
            true,
            Some(vec![0]),
        );
        let concerts = one_shot(conn, working_dir.path());

        let liked = concerts.set_track_liked(concert.id, 0, true).unwrap();
        assert!(liked.tracks[0].liked);
        let repeated = concerts.set_track_liked(concert.id, 0, true).unwrap();
        assert!(repeated.tracks[0].liked);

        let liked_events = concerts
            .event_history(concert.id)
            .unwrap()
            .into_iter()
            .filter(|event| event.event == "track_liked")
            .count();
        assert_eq!(liked_events, 1, "a retried like records no second event");

        let unliked = concerts.set_track_liked(concert.id, 0, false).unwrap();
        assert!(!unliked.tracks[0].liked);
    }

    #[test]
    fn set_track_liked_rejects_tracks_that_are_not_there() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let seeds = SeedContext::new(&conn);
        // Split, but only the first track has a file.
        let concert = seeded_media(
            &seeds,
            working_dir.path(),
            three_songs(),
            true,
            Some(vec![0]),
        );
        let concerts = one_shot(conn, working_dir.path());

        assert!(matches!(
            concerts.set_track_liked(concert.id, 9, true),
            Err(ConcertCommandError::Rejected(
                CommandRejection::NoSuchTrack { track_index: 9, .. }
            ))
        ));
        assert!(matches!(
            concerts.set_track_liked(concert.id, 1, true),
            Err(ConcertCommandError::Rejected(
                CommandRejection::MissingMedia { .. }
            ))
        ));
        assert!(
            concerts
                .get(concert.id)
                .unwrap()
                .tracks
                .iter()
                .all(|track| !track.liked),
            "a rejected like changes nothing"
        );
    }

    /// A concert whose song media covers the whole source, so the source file is
    /// redundant and may be deleted.
    fn redundant_source_concert(seeds: &SeedContext, working_dir: &std::path::Path) -> Concert {
        seeds
            .seed_media_concert(
                working_dir,
                SeedMediaConcert {
                    lifecycle: SeedLifecycleConcert {
                        set_list: Some(vec!["Only".to_string()]),
                        downloaded: true,
                        split: true,
                        tracks_present: Some(vec![true]),
                        user_timestamps: Some(vec![concert_types::SongTimestamp {
                            title: "Only".to_string(),
                            start_time: 0.0,
                            end_time: 10.0,
                            duration: 10.0,
                        }]),
                        media_duration: Some(10.0),
                        ..SeedLifecycleConcert::default()
                    },
                    source_file: true,
                    track_files: Some(vec![0]),
                    ..SeedMediaConcert::default()
                },
            )
            .unwrap()
    }

    #[test]
    fn deleting_a_download_clears_it_and_repeating_is_a_success() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let seeds = SeedContext::new(&conn);
        let concert = seeded_media(
            &seeds,
            working_dir.path(),
            three_songs(),
            true,
            Some(vec![0]),
        );
        let concerts = one_shot(conn, working_dir.path());

        let first = concerts
            .delete_download(concert.id, DeletionConfirmation::Unconfirmed)
            .unwrap();
        assert!(!first.state().concert.downloaded_at.is_some());
        assert!(!matches!(first, DeletionOutcome::ConfirmationRequired(_)));

        // The split survives a download deletion.
        assert!(first.state().concert.split_at.is_some());

        // Already absent: the desired end state holds, so this succeeds.
        let second = concerts
            .delete_download(concert.id, DeletionConfirmation::Unconfirmed)
            .unwrap();
        assert!(!second.state().concert.downloaded_at.is_some());
        assert!(
            concerts
                .event_history(concert.id)
                .unwrap()
                .iter()
                .filter(|event| event.event == "download_delete")
                .count()
                == 1,
            "a retried deletion records only the real deletion"
        );
    }

    #[test]
    fn deleting_a_download_whose_file_is_already_gone_asks_for_confirmation() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let seeds = SeedContext::new(&conn);
        let concert = seeded_media(&seeds, working_dir.path(), three_songs(), true, None);
        let concerts = one_shot(conn, working_dir.path());
        // A file that vanished out of band: the record still claims a download.
        let source = crate::concert_media::find_downloaded_file(
            working_dir.path(),
            concert.album.as_deref().unwrap(),
        )
        .expect("fixture wrote a source file");
        std::fs::remove_file(source).unwrap();

        let unconfirmed = concerts
            .delete_download(concert.id, DeletionConfirmation::Unconfirmed)
            .unwrap();
        assert!(matches!(
            unconfirmed,
            DeletionOutcome::ConfirmationRequired(_)
        ));
        assert!(concerts
            .get(concert.id)
            .unwrap()
            .concert
            .downloaded_at
            .is_some());

        let confirmed = concerts
            .delete_download(concert.id, DeletionConfirmation::Confirmed)
            .unwrap();
        assert!(matches!(confirmed, DeletionOutcome::Deleted(_)));
        assert!(concerts
            .get(concert.id)
            .unwrap()
            .concert
            .downloaded_at
            .is_none());
    }

    #[test]
    fn deleting_a_redundant_source_requires_coverage_and_then_succeeds() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let seeds = SeedContext::new(&conn);
        // A concert whose media does not cover the source keeps its file.
        let uncovered = seeded_media(
            &seeds,
            working_dir.path(),
            three_songs(),
            true,
            Some(vec![0]),
        );
        let covered = redundant_source_concert(&seeds, working_dir.path());
        let concerts = one_shot(conn, working_dir.path());

        let uncovered_outcome = concerts.delete_redundant_source(uncovered.id);
        assert!(
            matches!(
                uncovered_outcome,
                Err(ConcertCommandError::Rejected(
                    CommandRejection::SourceNotRedundant { concert_id }
                )) if concert_id == uncovered.id
            ),
            "unexpected outcome: {uncovered_outcome:?}"
        );
        assert!(concerts
            .get(uncovered.id)
            .unwrap()
            .concert
            .downloaded_at
            .is_some());

        // A fully covered source is deleted, and repeating the request is a
        // success rather than a conflict.
        let first = concerts.delete_redundant_source(covered.id).unwrap();
        assert!(matches!(first, DeletionOutcome::Deleted(_)));
        assert!(first.state().concert.downloaded_at.is_none());
        let second = concerts.delete_redundant_source(covered.id).unwrap();
        assert!(matches!(second, DeletionOutcome::Deleted(_)));
    }

    #[test]
    fn a_hand_restored_source_is_deleted_even_though_no_download_is_recorded() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let seeds = SeedContext::new(&conn);
        let concert = redundant_source_concert(&seeds, working_dir.path());
        // A restored backup: the media is there, the lifecycle record is not.
        conn.execute(
            "UPDATE concerts SET downloaded_at = NULL WHERE id = ?1",
            rusqlite::params![concert.id],
        )
        .unwrap();
        let concerts = one_shot(conn, working_dir.path());
        let album = concert.album.clone().expect("seeded concert has an album");
        let source = crate::concert_media::find_downloaded_file(working_dir.path(), &album)
            .expect("fixture wrote a source file");

        let outcome = concerts.delete_redundant_source(concert.id).unwrap();

        assert!(matches!(outcome, DeletionOutcome::Deleted(_)));
        assert!(
            !source.exists(),
            "the source file on disk is what the request is about, not the record"
        );
    }

    #[test]
    fn deleting_split_state_is_idempotent() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let seeds = SeedContext::new(&conn);
        let concert = seeded_media(
            &seeds,
            working_dir.path(),
            three_songs(),
            true,
            Some(vec![0]),
        );
        let concerts = one_shot(conn, working_dir.path());

        let first = concerts.delete_split(concert.id).unwrap();
        assert!(first.state().concert.split_at.is_none());
        let second = concerts.delete_split(concert.id).unwrap();
        assert!(matches!(second, DeletionOutcome::Deleted(_)));
    }

    #[test]
    fn deleting_a_track_removes_its_file_and_repeating_is_a_success() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let seeds = SeedContext::new(&conn);
        let concert = seeded_media(
            &seeds,
            working_dir.path(),
            three_songs(),
            true,
            Some(vec![0, 1, 2]),
        );
        let concerts = one_shot(conn, working_dir.path());
        let album = concert.album.clone().unwrap();
        assert!(
            crate::concert_media::find_track_file(working_dir.path(), &album, "First").is_some()
        );

        let first = concerts.delete_track(concert.id, 0).unwrap();
        assert!(!first.state().tracks[0].persisted_available);
        assert!(
            crate::concert_media::find_track_file(working_dir.path(), &album, "First").is_none()
        );
        assert!(concerts.get(concert.id).unwrap().concert.split_at.is_some());

        // Retrying the same deletion succeeds and records no second event.
        concerts.delete_track(concert.id, 0).unwrap();
        assert_eq!(
            concerts
                .event_history(concert.id)
                .unwrap()
                .iter()
                .filter(|event| event.event == "track_delete")
                .count(),
            1
        );
    }

    #[test]
    fn deleting_a_track_that_never_existed_is_a_rejection() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let seeds = SeedContext::new(&conn);
        let concert = seeded_media(
            &seeds,
            working_dir.path(),
            three_songs(),
            true,
            Some(vec![0]),
        );
        let concerts = one_shot(conn, working_dir.path());

        assert!(matches!(
            concerts.delete_track(concert.id, 9),
            Err(ConcertCommandError::Rejected(
                CommandRejection::NoSuchTrack { track_index: 9, .. }
            ))
        ));
    }

    #[test]
    fn deleting_an_interlude_removes_its_file_and_repeating_is_a_success() {
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
                        tracks_present: Some(vec![true, true]),
                        user_timestamps: Some(vec![
                            concert_types::SongTimestamp {
                                title: "First".to_string(),
                                start_time: 0.0,
                                end_time: 10.0,
                                duration: 10.0,
                            },
                            concert_types::SongTimestamp {
                                title: "Second".to_string(),
                                start_time: 20.0,
                                end_time: 30.0,
                                duration: 10.0,
                            },
                        ]),
                        media_duration: Some(30.0),
                        ..SeedLifecycleConcert::default()
                    },
                    source_file: true,
                    track_files: Some(vec![0, 1]),
                    interlude_files: true,
                    ..SeedMediaConcert::default()
                },
            )
            .unwrap();
        let concerts = one_shot(conn, working_dir.path());
        let album = concert.album.clone().unwrap();
        assert!(crate::concert_media::find_interlude_file(
            working_dir.path(),
            &album,
            1
        ));

        concerts.delete_interlude(concert.id, 1).unwrap();
        assert!(!crate::concert_media::find_interlude_file(
            working_dir.path(),
            &album,
            1
        ));

        // Retrying succeeds and records no second event.
        let repeated = concerts.delete_interlude(concert.id, 1).unwrap();
        assert!(matches!(repeated, DeletionOutcome::Deleted(_)));
        assert_eq!(
            concerts
                .event_history(concert.id)
                .unwrap()
                .iter()
                .filter(|event| event.event == "interlude_delete")
                .count(),
            1
        );
    }

    /// The interface backed by a test Job config: timestamps can be read, and a
    /// *valid* change would admit a Job Run through the deterministic runner.
    fn with_test_jobs(conn: rusqlite::Connection, working_dir: &std::path::Path) -> Concerts {
        Concerts::new(
            Arc::new(Mutex::new(conn)),
            working_dir.to_path_buf(),
            Arc::new(JobRegistry::new()),
            ScrapeQueue::detached(),
            crate::jobs::JobConfig::test(working_dir.to_path_buf()),
        )
    }

    fn timestamps_payload(titles: &[&str], end_time: f64) -> TimestampPayload {
        TimestampPayload {
            songs: titles
                .iter()
                .map(|title| crate::split_timestamps::TimestampPayloadSong {
                    title: title.to_string(),
                    start_time: 0.0,
                    end_time,
                })
                .collect(),
        }
    }

    /// A Job config whose download and split commands never finish on their own,
    /// so admitted work is still observable when the assertion runs.
    fn with_blocking_jobs(conn: rusqlite::Connection, working_dir: &std::path::Path) -> Concerts {
        use tokio::process::Command;
        fn blocking() -> Command {
            let mut command = Command::new("sleep");
            command.arg("30");
            command
        }
        Concerts::new(
            Arc::new(Mutex::new(conn)),
            working_dir.to_path_buf(),
            Arc::new(JobRegistry::new()),
            ScrapeQueue::detached(),
            crate::jobs::JobConfig::from_commands(
                working_dir.to_path_buf(),
                Arc::new(|_| blocking()),
                Arc::new(|_| blocking()),
                Arc::new(|_| Command::new("true")),
            ),
        )
    }

    /// Seed a concert whose archive symlink exists at `archive_dir/<album>` and
    /// whose lifecycle records it as archived.
    fn archived_concert(
        conn: &rusqlite::Connection,
        working_dir: &std::path::Path,
        archive_dir: &std::path::Path,
        album: &str,
    ) -> Concert {
        let concert = SeedContext::new(conn)
            .seed_lifecycle_concert(SeedLifecycleConcert {
                album: Some(album.to_string()),
                downloaded: true,
                split: true,
                ..SeedLifecycleConcert::default()
            })
            .unwrap();
        let archived = archive_dir.join(album);
        std::fs::create_dir_all(&archived).unwrap();
        std::fs::write(archived.join("show.mp4"), b"media").unwrap();
        let concert_dir = crate::model::concert_dir(working_dir, album);
        std::fs::create_dir_all(concert_dir.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&archived, &concert_dir).unwrap();
        db::settings::update_archive_location(conn, archive_dir.to_str().unwrap()).unwrap();
        db::lifecycle::mark_archive_succeeded(conn, concert.id).unwrap();
        db::concerts::get_concert(conn, concert.id).unwrap()
    }

    #[tokio::test]
    async fn an_archive_request_without_a_configured_location_is_a_conflict() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_lifecycle_concert(SeedLifecycleConcert {
                downloaded: true,
                ..SeedLifecycleConcert::default()
            })
            .unwrap();
        let concerts = with_blocking_jobs(conn, working_dir.path());

        let outcome = concerts.request_archive(concert.id).await;

        assert!(
            matches!(
                outcome,
                Err(ConcertCommandError::Rejected(
                    CommandRejection::Conflict { concert_id, .. }
                )) if concert_id == concert.id
            ),
            "an unconfigured archive location is a rejection: {outcome:?}"
        );
    }

    #[tokio::test]
    async fn an_archive_request_on_media_that_is_not_downloaded_has_nothing_to_archive() {
        let working_dir = tempfile::tempdir().unwrap();
        let archive_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_lifecycle_concert(SeedLifecycleConcert::default())
            .unwrap();
        db::settings::update_archive_location(&conn, archive_dir.path().to_str().unwrap()).unwrap();
        let concerts = with_blocking_jobs(conn, working_dir.path());

        let outcome = concerts.request_archive(concert.id).await.unwrap();

        assert!(
            matches!(outcome, ArchiveOutcome::NothingToArchive(_)),
            "there is no media to move, which is not a failure"
        );
    }

    #[tokio::test]
    async fn unarchiving_moves_the_media_back_and_clears_the_lifecycle() {
        let working_dir = tempfile::tempdir().unwrap();
        let archive_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = archived_concert(
            &conn,
            working_dir.path(),
            archive_dir.path(),
            "Unarchive Album",
        );
        let concerts = with_blocking_jobs(conn, working_dir.path());

        let state = concerts.unarchive(concert.id).await.unwrap();

        let concert_dir = crate::model::concert_dir(working_dir.path(), "Unarchive Album");
        assert!(
            concert_dir.join("show.mp4").is_file(),
            "the media should be back in the concert directory"
        );
        assert!(
            std::fs::symlink_metadata(&concert_dir)
                .map(|meta| !meta.file_type().is_symlink())
                .unwrap_or(false),
            "the archive symlink should be replaced by real media"
        );
        assert_eq!(state.concert.archive_status(), ArchiveStatus::NotArchived);
    }

    #[tokio::test]
    async fn unarchiving_a_concert_that_is_not_archived_is_a_conflict() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_lifecycle_concert(SeedLifecycleConcert {
                album: Some("Never Archived".to_string()),
                downloaded: true,
                ..SeedLifecycleConcert::default()
            })
            .unwrap();
        let concerts = with_blocking_jobs(conn, working_dir.path());

        let outcome = concerts.unarchive(concert.id).await;

        assert!(
            matches!(
                outcome,
                Err(ConcertCommandError::Rejected(
                    CommandRejection::Conflict { concert_id, .. }
                )) if concert_id == concert.id
            ),
            "expected a conflict, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn unarchiving_without_an_archive_symlink_records_the_failure_and_repeats_cleanly() {
        let working_dir = tempfile::tempdir().unwrap();
        let archive_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_lifecycle_concert(SeedLifecycleConcert {
                album: Some("Missing Symlink".to_string()),
                downloaded: true,
                split: true,
                ..SeedLifecycleConcert::default()
            })
            .unwrap();
        db::settings::update_archive_location(&conn, archive_dir.path().to_str().unwrap()).unwrap();
        db::lifecycle::mark_archive_succeeded(&conn, concert.id).unwrap();
        let concerts = with_blocking_jobs(conn, working_dir.path());

        let first = concerts.unarchive(concert.id).await;
        assert!(
            matches!(first, Err(ConcertCommandError::Operational(_))),
            "a missing symlink is a fault the user must see: {first:?}"
        );

        let failed = {
            let conn = concerts.lock_db().unwrap();
            db::failed_jobs::list_failed_jobs(&conn, 10).unwrap()
        };
        assert!(
            failed.iter().any(|job| job.name == "unarchive"),
            "the failure should be recorded in Failed Job history"
        );

        // The record is unchanged, so a retry fails the same way rather than
        // pretending the concert was restored.
        let second = concerts.unarchive(concert.id).await;
        assert!(matches!(second, Err(ConcertCommandError::Operational(_))));
    }

    #[tokio::test]
    async fn ensure_scraped_keeps_metadata_that_is_already_there() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_scraped_concert(SeedScrapedConcert::default())
            .unwrap();
        let concerts = with_blocking_jobs(conn, working_dir.path());

        concerts.ensure_scraped(concert.id).await.unwrap();

        let state = concerts.get(concert.id).unwrap();
        assert_eq!(state.concert.artist, concert.artist);
        assert_eq!(state.concert.set_list, concert.set_list);
        assert!(state.concert.metadata_scraped_at.is_some());
    }

    #[tokio::test]
    async fn ensure_scraped_tolerates_a_failing_fetch() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        // An unusable URL fails immediately, without touching the network.
        let concert = SeedContext::new(&conn)
            .seed_listing(SeedListing {
                source_url: Some("not-a-url".to_string()),
                ..SeedListing::default()
            })
            .unwrap();
        let concerts = with_blocking_jobs(conn, working_dir.path());

        concerts.ensure_scraped(concert.id).await.unwrap();

        let state = concerts.get(concert.id).unwrap();
        assert!(
            state.concert.metadata_scraped_at.is_none(),
            "a failed auto-scrape keeps the row unscraped so the next view retries"
        );
        assert_eq!(state.concert.title, concert.title);
    }

    #[test]
    fn enqueueing_a_scrape_reports_whether_one_was_needed() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let scraped = SeedContext::new(&conn)
            .seed_scraped_concert(SeedScrapedConcert::default())
            .unwrap();
        let unscraped = SeedContext::new(&conn)
            .seed_listing(SeedListing {
                source_url: Some("https://example.org/never-scraped".to_string()),
                ..SeedListing::default()
            })
            .unwrap();
        // A detached queue accepts work but runs none of it, so the second call
        // observes the first as already queued.
        let concerts =
            Concerts::without_workers(Arc::new(Mutex::new(conn)), working_dir.path().to_path_buf());

        assert_eq!(
            concerts.enqueue_scrape(scraped.id).unwrap(),
            ScrapeAdmission::NotNeeded
        );
        assert_eq!(
            concerts.enqueue_scrape(unscraped.id).unwrap(),
            ScrapeAdmission::AlreadyQueued,
            "a one-shot process has no scrape worker to accept the work"
        );
        assert!(matches!(
            concerts.enqueue_scrape(i64::MAX),
            Err(ConcertCommandError::Rejected(
                CommandRejection::NotFound { .. }
            ))
        ));
    }

    #[tokio::test]
    async fn a_download_request_with_a_set_list_queues_the_split_behind_it() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_scraped_concert(SeedScrapedConcert::default())
            .unwrap();
        let concerts = with_blocking_jobs(conn, working_dir.path());

        let state = concerts.request_download(concert.id).await.unwrap().state;

        assert!(
            state.active_work.download.registry_active,
            "the download should be running"
        );
        assert!(
            state.active_work.split_queued_after_download,
            "the split should wait behind the download"
        );
    }

    #[tokio::test]
    async fn a_download_request_without_a_set_list_starts_only_a_download() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_scraped_concert(SeedScrapedConcert {
                set_list: Some(vec![]),
                ..SeedScrapedConcert::default()
            })
            .unwrap();
        let concerts = with_blocking_jobs(conn, working_dir.path());

        let state = concerts.request_download(concert.id).await.unwrap().state;

        assert!(state.active_work.download.registry_active);
        assert!(
            !state.active_work.split_queued_after_download,
            "there is nothing to split, so no split may be queued"
        );
    }

    #[tokio::test]
    async fn a_second_download_request_reports_an_already_active_chain() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_scraped_concert(SeedScrapedConcert::default())
            .unwrap();
        let concerts = with_blocking_jobs(conn, working_dir.path());

        let first = concerts.request_download(concert.id).await.unwrap();
        let second = concerts.request_download(concert.id).await.unwrap();

        assert_eq!(first.admission, JobAdmission::Admitted);
        assert_eq!(
            second.admission,
            JobAdmission::AlreadyRunning,
            "the second request finds the chain already running"
        );
    }

    #[tokio::test]
    async fn a_split_request_without_a_source_reports_nothing_to_do() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_scraped_concert(SeedScrapedConcert::default())
            .unwrap();
        let concerts = with_blocking_jobs(conn, working_dir.path());

        let request = concerts.request_split(concert.id).await.unwrap();

        assert_eq!(request.admission, JobAdmission::NothingToDo);
    }

    #[tokio::test]
    async fn a_second_download_request_keeps_the_queued_split() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_scraped_concert(SeedScrapedConcert::default())
            .unwrap();
        let concerts = with_blocking_jobs(conn, working_dir.path());

        concerts.request_download(concert.id).await.unwrap();
        let state = concerts.request_download(concert.id).await.unwrap().state;

        assert!(
            state.active_work.split_queued_after_download,
            "a repeated request must not drop the queued split"
        );
    }

    #[tokio::test]
    async fn a_split_request_without_a_source_is_accepted_and_does_nothing() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_scraped_concert(SeedScrapedConcert::default())
            .unwrap();
        let concerts = with_blocking_jobs(conn, working_dir.path());

        let state = concerts.request_split(concert.id).await.unwrap().state;

        assert!(!state.active_work.split.registry_active);
        assert!(!state.active_work.split.persisted_started);
    }

    #[tokio::test]
    async fn cancelling_a_running_job_is_reported_and_repeating_it_is_not_an_error() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_scraped_concert(SeedScrapedConcert::default())
            .unwrap();
        let concerts = with_blocking_jobs(conn, working_dir.path());
        concerts.request_download(concert.id).await.unwrap();

        let cancelled = concerts.cancel_job(concert.id, JobKind::Download).unwrap();
        assert!(matches!(cancelled, JobCancellation::Cancelled(_)));
        assert_eq!(
            cancelled.state().concert.download_status(),
            DownloadStatus::DownloadError,
            "a cancelled download is a terminal unsuccessful outcome"
        );

        let again = concerts.cancel_job(concert.id, JobKind::Download).unwrap();
        assert!(
            matches!(again, JobCancellation::NothingActive(_)),
            "cancelling nothing is an outcome, not a failure"
        );
    }

    #[tokio::test]
    async fn preparing_queues_the_download_and_split_chain() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_scraped_concert(SeedScrapedConcert::default())
            .unwrap();
        let concerts = with_blocking_jobs(conn, working_dir.path());

        let state = concerts.prepare(concert.id).await.unwrap();

        assert!(state.active_work.download.registry_active);
        assert!(state.active_work.split_queued_after_download);
    }

    #[tokio::test]
    async fn preparing_without_a_set_list_is_an_invalid_rejection() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_scraped_concert(SeedScrapedConcert {
                set_list: Some(vec![]),
                ..SeedScrapedConcert::default()
            })
            .unwrap();
        let concerts = with_blocking_jobs(conn, working_dir.path());

        let outcome = concerts.prepare(concert.id).await;

        match outcome {
            Err(ConcertCommandError::Rejected(CommandRejection::Invalid {
                concert_id,
                message,
            })) => {
                assert_eq!(concert_id, concert.id);
                assert!(
                    message.contains("no set list"),
                    "the rejection should say what to fix: {message}"
                );
            }
            other => panic!("expected an invalid rejection, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn job_requests_are_rejected_for_an_absent_concert() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concerts = with_blocking_jobs(conn, working_dir.path());
        let missing = i64::MAX;

        for outcome in [
            concerts.request_download(missing).await.map(|_| ()),
            concerts.request_split(missing).await.map(|_| ()),
            concerts.prepare(missing).await.map(|_| ()),
        ] {
            assert!(
                matches!(
                    outcome,
                    Err(ConcertCommandError::Rejected(
                        CommandRejection::NotFound { concert_id }
                    )) if concert_id == missing
                ),
                "an absent concert is a rejection, not a fault"
            );
        }
    }

    #[tokio::test]
    async fn a_one_shot_process_cannot_admit_job_requests() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_scraped_concert(SeedScrapedConcert::default())
            .unwrap();
        let concerts = one_shot(conn, working_dir.path());

        for outcome in [
            concerts.request_download(concert.id).await.map(|_| ()),
            concerts.request_split(concert.id).await.map(|_| ()),
            concerts.prepare(concert.id).await.map(|_| ()),
        ] {
            assert!(
                matches!(outcome, Err(ConcertCommandError::Operational(_))),
                "a process with no Job Runs must say so: {outcome:?}"
            );
        }
    }

    #[tokio::test]
    async fn split_timestamps_read_the_stored_values() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_lifecycle_concert(SeedLifecycleConcert {
                set_list: Some(vec!["First".to_string(), "Second".to_string()]),
                auto_timestamps: Some(vec![
                    concert_types::SongTimestamp {
                        title: "First".to_string(),
                        start_time: 0.0,
                        end_time: 10.0,
                        duration: 10.0,
                    },
                    concert_types::SongTimestamp {
                        title: "Second".to_string(),
                        start_time: 20.0,
                        end_time: 30.0,
                        duration: 10.0,
                    },
                ]),
                ..SeedLifecycleConcert::default()
            })
            .unwrap();
        let concerts = with_test_jobs(conn, working_dir.path());

        let read = concerts.split_timestamps(concert.id).await.unwrap();

        assert_eq!(read.set_list, vec!["First", "Second"]);
        assert_eq!(read.auto.unwrap().len(), 2);
        assert!(read.user.is_none());
    }

    #[tokio::test]
    async fn split_timestamp_commands_reject_an_absent_concert() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concerts = with_test_jobs(conn, working_dir.path());
        let missing = i64::MAX;

        assert!(matches!(
            concerts.split_timestamps(missing).await,
            Err(ConcertCommandError::Rejected(
                CommandRejection::NotFound { concert_id }
            )) if concert_id == missing
        ));
        assert!(matches!(
            concerts.set_split_timestamps(missing, timestamps_payload(&["A"], 10.0)).await,
            Err(ConcertCommandError::Rejected(
                CommandRejection::NotFound { concert_id }
            )) if concert_id == missing
        ));
        assert!(matches!(
            concerts.reset_split_timestamps(missing).await,
            Err(ConcertCommandError::Rejected(
                CommandRejection::NotFound { concert_id }
            )) if concert_id == missing
        ));
    }

    #[tokio::test]
    async fn timestamp_changes_refuse_an_unobserved_source_instead_of_assuming_it_is_absent() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_lifecycle_concert(SeedLifecycleConcert {
                set_list: Some(vec!["First".to_string()]),
                ..SeedLifecycleConcert::default()
            })
            .unwrap();
        // Make the concert directory unreadable: it is a file, so the source
        // cannot be observed as present or absent.
        let concert_dir =
            crate::model::concert_dir(working_dir.path(), concert.album.as_deref().unwrap());
        std::fs::create_dir_all(concert_dir.parent().unwrap()).unwrap();
        std::fs::write(&concert_dir, b"not a directory").unwrap();
        let concerts = with_test_jobs(conn, working_dir.path());

        let outcome = concerts
            .set_split_timestamps(concert.id, timestamps_payload(&["First"], 10.0))
            .await;

        assert!(
            matches!(
                outcome,
                Err(ConcertCommandError::Rejected(
                    CommandRejection::Conflict { concert_id, .. }
                )) if concert_id == concert.id
            ),
            "an unobservable source must not be treated as absent: {outcome:?}"
        );
    }

    #[tokio::test]
    async fn timestamp_validation_failures_are_invalid_rejections() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = seeded_media(
            &SeedContext::new(&conn),
            working_dir.path(),
            three_songs(),
            true,
            Some(vec![0, 1, 2]),
        );
        let concerts = with_test_jobs(conn, working_dir.path());

        // The payload names one song but the set list has three.
        let outcome = concerts
            .set_split_timestamps(concert.id, timestamps_payload(&["First"], 10.0))
            .await;

        assert!(
            matches!(
                outcome,
                Err(ConcertCommandError::Rejected(
                    CommandRejection::Invalid { concert_id, .. }
                )) if concert_id == concert.id
            ),
            "expected an invalid rejection, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn a_one_shot_process_reports_that_it_cannot_admit_split_work() {
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = seeded_media(
            &SeedContext::new(&conn),
            working_dir.path(),
            three_songs(),
            true,
            Some(vec![0, 1, 2]),
        );
        let concerts = one_shot(conn, working_dir.path());

        let outcome = concerts.reset_split_timestamps(concert.id).await;

        assert!(
            matches!(outcome, Err(ConcertCommandError::Operational(_))),
            "expected an operational error, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn resetting_timestamps_does_not_need_the_source_file() {
        // Resetting drops the desired timestamps and replays the automatic ones;
        // it never reads the source. A concert whose media is absent must still
        // report "already auto" rather than a conflict — the hurl contract for
        // /split-timestamps/reset depends on it.
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_lifecycle_concert(SeedLifecycleConcert {
                set_list: Some(vec!["First".to_string()]),
                auto_timestamps: Some(vec![concert_types::SongTimestamp {
                    title: "First".to_string(),
                    start_time: 0.0,
                    end_time: 55.0,
                    duration: 55.0,
                }]),
                ..SeedLifecycleConcert::default()
            })
            .unwrap();
        let concerts = with_test_jobs(conn, working_dir.path());

        let outcome = concerts.reset_split_timestamps(concert.id).await;

        let start = outcome.expect("reset should be admitted");
        assert_eq!(
            start.outcome,
            crate::split_timestamps::SplitStartOutcome::AlreadyAuto
        );
        assert_eq!(start.state.concert.id, concert.id);
    }

    #[tokio::test]
    async fn resetting_without_automatic_timestamps_is_an_invalid_rejection() {
        // "Nothing to reset to" is a validation failure about the request, not a
        // conflict about the source file: 422, not 409.
        let working_dir = tempfile::tempdir().unwrap();
        let conn = db::connection::open_in_memory().unwrap();
        let concert = SeedContext::new(&conn)
            .seed_lifecycle_concert(SeedLifecycleConcert {
                set_list: Some(vec!["First".to_string()]),
                ..SeedLifecycleConcert::default()
            })
            .unwrap();
        let concerts = with_test_jobs(conn, working_dir.path());

        let outcome = concerts.reset_split_timestamps(concert.id).await;

        assert!(
            matches!(
                outcome,
                Err(ConcertCommandError::Rejected(
                    CommandRejection::Invalid { concert_id, .. }
                )) if concert_id == concert.id
            ),
            "expected an invalid rejection, got {outcome:?}"
        );
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
            crate::jobs::JobConfig::test(working_dir.path().to_path_buf()),
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
                crate::jobs::JobConfig::test(working_dir.path().to_path_buf()),
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
                crate::jobs::JobConfig::test(working_dir.path().to_path_buf()),
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
                crate::jobs::JobConfig::test(working_dir.path().to_path_buf()),
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
                crate::jobs::JobConfig::test(working_dir.path().to_path_buf()),
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
                crate::jobs::JobConfig::test(working_dir.path().to_path_buf()),
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
            crate::jobs::JobConfig::test(working_dir.path().to_path_buf()),
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
