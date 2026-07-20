use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use concert_types::{SongTimestamp, TimestampsFile};
use rusqlite::Connection;
use serde::Deserialize;
use std::fmt;
use utoipa::ToSchema;

use crate::concert_media::find_downloaded_file;
use crate::db;
use crate::jobs::{self, JobConfig, JobRegistry, SplitMode};

const MIN_SONG_DURATION_SECONDS: f64 = 1.0;

/// Per-song payload from the POST /concerts/:id/split-timestamps request body.
#[derive(Deserialize, ToSchema)]
pub struct TimestampPayload {
    pub songs: Vec<TimestampPayloadSong>,
}

#[derive(Deserialize, ToSchema)]
pub struct TimestampPayloadSong {
    pub title: String,
    pub start_time: f64,
    pub end_time: f64,
}

/// Response body for GET /concerts/:id/split-timestamps.
#[derive(serde::Serialize, ToSchema)]
pub struct SplitTimestampsResponse {
    pub set_list: Vec<String>,
    pub auto: Option<Vec<SongTimestamp>>,
    pub user: Option<Vec<SongTimestamp>>,
    /// Total source-media duration in seconds, from `ffprobe`. `None` when the
    /// source isn't downloaded or `ffprobe` is unavailable/fails. The frontend
    /// timeline uses this for its scale and right-edge clamp.
    pub media_duration: Option<f64>,
}

/// Success body for the split-start endpoints.
#[derive(serde::Serialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SplitStartStatus {
    Splitting,
    AlreadyAuto,
}

#[derive(serde::Serialize, ToSchema)]
pub struct SplitStartResponse {
    pub status: SplitStartStatus,
}

pub struct SplitTimestampsRead {
    pub set_list: Vec<String>,
    pub auto: Option<Vec<SongTimestamp>>,
    pub user: Option<Vec<SongTimestamp>>,
    pub media_duration: Option<f64>,
}

impl From<SplitTimestampsRead> for SplitTimestampsResponse {
    fn from(read: SplitTimestampsRead) -> Self {
        SplitTimestampsResponse {
            set_list: read.set_list,
            auto: read.auto,
            user: read.user,
            media_duration: read.media_duration,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitStartOutcome {
    Splitting,
    AlreadyAuto,
}

impl From<SplitStartOutcome> for SplitStartResponse {
    fn from(outcome: SplitStartOutcome) -> Self {
        let status = match outcome {
            SplitStartOutcome::Splitting => SplitStartStatus::Splitting,
            SplitStartOutcome::AlreadyAuto => SplitStartStatus::AlreadyAuto,
        };
        SplitStartResponse { status }
    }
}

#[derive(Debug)]
pub enum SplitTimestampWorkflowError {
    NotFound,
    Conflict(String),
    Unprocessable(String),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for SplitTimestampWorkflowError {
    fn from(e: anyhow::Error) -> Self {
        SplitTimestampWorkflowError::Internal(e)
    }
}

#[derive(Debug, PartialEq)]
pub enum TimestampValidationError {
    EmptySetList,
    CountMismatch {
        expected: usize,
        got: usize,
    },
    TitleMismatch {
        index: usize,
        expected: String,
        got: String,
    },
    NonFinite {
        index: usize,
        field: &'static str,
    },
    NegativeStart {
        index: usize,
    },
    TooShort {
        index: usize,
        duration: f64,
    },
    Overlap {
        index: usize,
    },
    BeyondMediaDuration {
        index: usize,
        end_time: f64,
        duration: f64,
    },
    /// Set list changed since analysis — user must re-run analysis before resetting.
    SetListChangedSinceAnalysis {
        message: String,
    },
}

impl fmt::Display for TimestampValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySetList => write!(f, "Concert has no set list"),
            Self::CountMismatch { expected, got } => write!(
                f,
                "Expected {} timestamps (one per track), got {}",
                expected, got
            ),
            Self::TitleMismatch {
                index,
                expected,
                got,
            } => write!(
                f,
                "Track {} title mismatch: expected {:?}, got {:?}",
                index + 1,
                expected,
                got
            ),
            Self::NonFinite { index, field } => {
                write!(f, "Track {}: {} must be a finite number", index + 1, field)
            }
            Self::NegativeStart { index } => {
                write!(f, "Track {}: start_time must be >= 0", index + 1)
            }
            Self::TooShort { index, duration } => write!(
                f,
                "Track {}: duration {:.2}s is below the minimum {:.2}s",
                index + 1,
                duration,
                MIN_SONG_DURATION_SECONDS
            ),
            Self::Overlap { index } => write!(
                f,
                "Track {}: end_time exceeds the start_time of the next track",
                index + 1
            ),
            Self::BeyondMediaDuration {
                index,
                end_time,
                duration,
            } => write!(
                f,
                "Track {}: end_time {:.2}s exceeds the source file duration {:.2}s",
                index + 1,
                end_time,
                duration
            ),
            Self::SetListChangedSinceAnalysis { message } => write!(
                f,
                "Cannot reset: {}. Re-run analysis to generate new automated timestamps.",
                message
            ),
        }
    }
}

pub async fn read_split_timestamps(
    database: Arc<Mutex<Connection>>,
    working_dir: &Path,
    concert_id: i64,
) -> Result<SplitTimestampsRead, SplitTimestampWorkflowError> {
    let (set_list, auto, user, source_path, stored_media_duration) = {
        let conn = database.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, concert_id)
            .map_err(|_| SplitTimestampWorkflowError::NotFound)?;
        let auto = jobs::split::auto_timestamps_with_backfill(&conn, working_dir, &concert)
            .map_err(SplitTimestampWorkflowError::Internal)?;
        let stored = db::split_timestamps::get_split_timestamps(&conn, concert_id)
            .map_err(SplitTimestampWorkflowError::Internal)?;
        let source_path = concert
            .album
            .as_deref()
            .and_then(|a| find_downloaded_file(working_dir, a));
        (
            concert.set_list,
            auto,
            stored.user,
            source_path,
            concert.media_duration,
        )
    };

    Ok(SplitTimestampsRead {
        set_list,
        auto,
        user,
        media_duration: media_duration_for_read(source_path, stored_media_duration).await,
    })
}

pub async fn apply_user_timestamps(
    database: Arc<Mutex<Connection>>,
    registry: Arc<JobRegistry>,
    jobs: JobConfig,
    concert_id: i64,
    payload: TimestampPayload,
) -> Result<SplitStartOutcome, SplitTimestampWorkflowError> {
    let (concert, source_path) = {
        let conn = database.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, concert_id)
            .map_err(|_| SplitTimestampWorkflowError::NotFound)?;
        let source_path = concert
            .album
            .as_deref()
            .and_then(|a| find_downloaded_file(&jobs.working_dir, a));
        (concert, source_path)
    };

    let source_path = source_path.ok_or_else(|| {
        SplitTimestampWorkflowError::Conflict(
            "Source file not found — download the concert first".to_string(),
        )
    })?;

    if payload.songs.len() != concert.set_list.len() {
        return Err(SplitTimestampWorkflowError::Unprocessable(format!(
            "Expected {} timestamps (one per set-list song), got {}",
            concert.set_list.len(),
            payload.songs.len()
        )));
    }

    let media_duration = match probe_media_duration(&source_path).await {
        Ok(duration) => duration,
        Err(e) => {
            tracing::warn!("ffprobe failed for {}: {}", source_path.display(), e);
            return Err(SplitTimestampWorkflowError::Internal(e));
        }
    };
    let validated =
        ValidatedTimestamps::validate(&concert.set_list, Some(media_duration), &payload.songs)
            .map_err(|e| SplitTimestampWorkflowError::Unprocessable(e.to_string()))?;

    reconcile_downloaded_at_if_missing(&database, concert_id, concert.downloaded_at.is_none());

    start_split_timestamp_job(
        database,
        registry,
        jobs,
        concert_id,
        SplitMode::UserTimestamps {
            ts: validated,
            media_duration,
        },
    )
    .await
}

pub async fn reset_to_auto_timestamps(
    database: Arc<Mutex<Connection>>,
    registry: Arc<JobRegistry>,
    jobs: JobConfig,
    concert_id: i64,
) -> Result<SplitStartOutcome, SplitTimestampWorkflowError> {
    let (concert, auto_ts, user_is_null) = {
        let conn = database.lock().unwrap();
        let concert = db::concerts::get_concert(&conn, concert_id)
            .map_err(|_| SplitTimestampWorkflowError::NotFound)?;
        let auto_ts =
            jobs::split::auto_timestamps_with_backfill(&conn, &jobs.working_dir, &concert)
                .map_err(SplitTimestampWorkflowError::Internal)?;
        let stored = db::split_timestamps::get_split_timestamps(&conn, concert_id)
            .map_err(SplitTimestampWorkflowError::Internal)?;
        (concert, auto_ts, stored.user.is_none())
    };

    let auto_ts = auto_ts.ok_or_else(|| {
        SplitTimestampWorkflowError::Unprocessable(
            "No automated split timestamps available — run analysis first".to_string(),
        )
    })?;
    if user_is_null {
        return Ok(SplitStartOutcome::AlreadyAuto);
    }

    let payload_songs = song_timestamps_to_payload(&auto_ts);
    let validated = ValidatedTimestamps::validate_for_reset(&concert.set_list, &payload_songs)
        .map_err(|e| SplitTimestampWorkflowError::Unprocessable(e.to_string()))?;

    reconcile_downloaded_at_if_missing(&database, concert_id, concert.downloaded_at.is_none());

    start_split_timestamp_job(
        database,
        registry,
        jobs,
        concert_id,
        SplitMode::ResetToAuto(validated),
    )
    .await
}

/// Probe a media file using ffprobe on the blocking pool.
pub async fn probe_media_duration(path: &Path) -> anyhow::Result<f64> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || crate::scan::ffprobe_duration_sync(&path))
        .await
        .context("ffprobe task panicked")?
}

async fn media_duration_for_read(
    source_path: Option<PathBuf>,
    stored_media_duration: Option<f64>,
) -> Option<f64> {
    match source_path {
        Some(path) => match probe_media_duration(&path).await {
            Ok(d) => Some(d),
            Err(e) => {
                tracing::warn!(
                    "ffprobe failed for {}: {}; falling back to stored duration",
                    path.display(),
                    e
                );
                stored_media_duration
            }
        },
        None => stored_media_duration,
    }
}

fn reconcile_downloaded_at_if_missing(
    database: &Arc<Mutex<Connection>>,
    concert_id: i64,
    downloaded_at_missing: bool,
) {
    if !downloaded_at_missing {
        return;
    }
    let conn = database.lock().unwrap();
    let now = db::time::now_string();
    if let Err(e) = db::lifecycle::set_downloaded_at_if_missing(&conn, concert_id, &now) {
        tracing::warn!(
            "set_downloaded_at_if_missing failed for concert {}: {}",
            concert_id,
            e
        );
    }
}

async fn start_split_timestamp_job(
    database: Arc<Mutex<Connection>>,
    registry: Arc<JobRegistry>,
    jobs: JobConfig,
    concert_id: i64,
    mode: SplitMode,
) -> Result<SplitStartOutcome, SplitTimestampWorkflowError> {
    match jobs::split::start_split(database, registry, jobs, concert_id, mode)
        .await
        .map_err(SplitTimestampWorkflowError::Internal)?
    {
        jobs::split::StartOutcome::Spawned => Ok(SplitStartOutcome::Splitting),
        jobs::split::StartOutcome::AlreadyRunning => Err(SplitTimestampWorkflowError::Conflict(
            "A split job is already running for this concert".to_string(),
        )),
        jobs::split::StartOutcome::NotDownloaded => Err(SplitTimestampWorkflowError::Conflict(
            "Concert source file not downloaded".to_string(),
        )),
    }
}

/// Validated, typed set of per-song split timestamps.
/// Constructed only via `validate`; the inner Vec is private.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedTimestamps(Vec<SongTimestamp>);

impl ValidatedTimestamps {
    /// Validate payload songs against the concert's set_list.
    ///
    /// `media_duration`: `Some` when validating user POST (bounds-checks end times against
    /// the source file); `None` when loading stored auto timestamps for reset (those values
    /// came from the same file, so the bounds check is skipped).
    pub fn validate(
        set_list: &[String],
        media_duration: Option<f64>,
        songs: &[TimestampPayloadSong],
    ) -> Result<Self, TimestampValidationError> {
        if set_list.is_empty() {
            return Err(TimestampValidationError::EmptySetList);
        }
        if songs.len() != set_list.len() {
            return Err(TimestampValidationError::CountMismatch {
                expected: set_list.len(),
                got: songs.len(),
            });
        }

        let mut result = Vec::with_capacity(songs.len());

        for (i, song) in songs.iter().enumerate() {
            // Title must match positionally
            if song.title != set_list[i] {
                return Err(TimestampValidationError::TitleMismatch {
                    index: i,
                    expected: set_list[i].clone(),
                    got: song.title.clone(),
                });
            }

            if !song.start_time.is_finite() {
                return Err(TimestampValidationError::NonFinite {
                    index: i,
                    field: "start_time",
                });
            }
            if !song.end_time.is_finite() {
                return Err(TimestampValidationError::NonFinite {
                    index: i,
                    field: "end_time",
                });
            }

            if song.start_time < 0.0 {
                return Err(TimestampValidationError::NegativeStart { index: i });
            }

            let duration = song.end_time - song.start_time;
            if duration < MIN_SONG_DURATION_SECONDS {
                return Err(TimestampValidationError::TooShort { index: i, duration });
            }

            if let Some(md) = media_duration {
                if song.end_time > md {
                    return Err(TimestampValidationError::BeyondMediaDuration {
                        index: i,
                        end_time: song.end_time,
                        duration: md,
                    });
                }
            }

            result.push(SongTimestamp {
                title: song.title.clone(),
                start_time: song.start_time,
                end_time: song.end_time,
                duration,
            });
        }

        // Check ordering and non-overlap across adjacent tracks.
        // Gaps (end_time < next start_time) are allowed — cutting out talking.
        for i in 0..result.len().saturating_sub(1) {
            if result[i].end_time > result[i + 1].start_time {
                return Err(TimestampValidationError::Overlap { index: i });
            }
        }

        Ok(ValidatedTimestamps(result))
    }

    /// Validate stored auto timestamps (from DB or disk) for use in reset.
    /// Uses `SetListChangedSinceAnalysis` instead of generic errors for mismatch.
    pub fn validate_for_reset(
        set_list: &[String],
        songs: &[TimestampPayloadSong],
    ) -> Result<Self, TimestampValidationError> {
        if set_list.is_empty() {
            return Err(TimestampValidationError::EmptySetList);
        }
        if songs.len() != set_list.len() {
            return Err(TimestampValidationError::SetListChangedSinceAnalysis {
                message: format!(
                    "set list has {} tracks but automated timestamps have {}",
                    set_list.len(),
                    songs.len()
                ),
            });
        }
        for (i, song) in songs.iter().enumerate() {
            if song.title != set_list[i] {
                return Err(TimestampValidationError::SetListChangedSinceAnalysis {
                    message: format!(
                        "track {} is now {:?} but automated timestamps have {:?}",
                        i + 1,
                        set_list[i],
                        song.title
                    ),
                });
            }
        }
        // No media_duration check needed — these values came from the same source file.
        Self::validate(set_list, None, songs)
    }

    pub fn songs(&self) -> &[SongTimestamp] {
        &self.0
    }

    pub fn to_timestamps_file(&self) -> TimestampsFile {
        TimestampsFile {
            songs: self.0.clone(),
        }
    }
}

/// Convert stored `SongTimestamp`s back into payload form for validation.
pub fn song_timestamps_to_payload(ts: &[SongTimestamp]) -> Vec<TimestampPayloadSong> {
    ts.iter()
        .map(|t| TimestampPayloadSong {
            title: t.title.clone(),
            start_time: t.start_time,
            end_time: t.end_time,
        })
        .collect()
}

// Helper for tests — cheap clone without deriving Clone on the whole type.
#[cfg(test)]
impl TimestampPayloadSong {
    fn clone_for_test(&self) -> Self {
        TimestampPayloadSong {
            title: self.title.clone(),
            start_time: self.start_time,
            end_time: self.end_time,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::jobs::{JobConfig, JobRegistry};
    use crate::model::concert_dir;
    use std::sync::{Arc, Mutex};

    fn set_list() -> Vec<String> {
        vec!["Alpha".to_string(), "Beta".to_string(), "Gamma".to_string()]
    }

    fn valid_songs() -> Vec<TimestampPayloadSong> {
        vec![
            TimestampPayloadSong {
                title: "Alpha".to_string(),
                start_time: 0.0,
                end_time: 100.0,
            },
            TimestampPayloadSong {
                title: "Beta".to_string(),
                start_time: 105.0,
                end_time: 200.0,
            },
            TimestampPayloadSong {
                title: "Gamma".to_string(),
                start_time: 210.0,
                end_time: 350.0,
            },
        ]
    }

    fn seed_ts_concert(conn: &rusqlite::Connection, album: &str, songs: &[&str]) -> i64 {
        db::seeds::SeedContext::new(conn)
            .seed_scraped_concert(db::seeds::SeedScrapedConcert {
                source_url: Some(format!("https://npr.org/split-timestamps/{album}")),
                title: Some(format!("{album} Concert")),
                concert_date: Some("2024-06-01".to_string()),
                artist: Some("Test Artist".to_string()),
                album: Some(album.to_string()),
                set_list: Some(songs.iter().map(|s| s.to_string()).collect()),
            })
            .unwrap()
            .id
    }

    fn sample_timestamps(songs: &[&str]) -> Vec<SongTimestamp> {
        songs
            .iter()
            .enumerate()
            .map(|(i, title)| SongTimestamp {
                title: title.to_string(),
                start_time: (i * 60) as f64,
                end_time: (i * 60 + 55) as f64,
                duration: 55.0,
            })
            .collect()
    }

    fn payload_for(songs: &[&str]) -> TimestampPayload {
        TimestampPayload {
            songs: songs
                .iter()
                .enumerate()
                .map(|(i, title)| TimestampPayloadSong {
                    title: title.to_string(),
                    start_time: (i * 60) as f64,
                    end_time: (i * 60 + 55) as f64,
                })
                .collect(),
        }
    }

    #[test]
    fn validate_happy_path() {
        let ts = ValidatedTimestamps::validate(&set_list(), None, &valid_songs()).unwrap();
        assert_eq!(ts.songs().len(), 3);
        assert_eq!(ts.songs()[0].duration, 100.0);
        assert_eq!(ts.songs()[1].duration, 95.0);
    }

    #[test]
    fn validate_gaps_between_songs_allowed() {
        // Gap of 5s between Alpha end (100) and Beta start (105) is fine.
        ValidatedTimestamps::validate(&set_list(), None, &valid_songs()).unwrap();
    }

    #[test]
    fn validate_empty_set_list() {
        let err = ValidatedTimestamps::validate(&[], None, &[]).unwrap_err();
        assert_eq!(err, TimestampValidationError::EmptySetList);
    }

    #[test]
    fn validate_count_mismatch() {
        let songs = vec![valid_songs()[0].clone_for_test()];
        let err = ValidatedTimestamps::validate(&set_list(), None, &songs).unwrap_err();
        assert!(matches!(
            err,
            TimestampValidationError::CountMismatch {
                expected: 3,
                got: 1
            }
        ));
    }

    #[test]
    fn validate_title_mismatch() {
        let mut songs = valid_songs();
        songs[1].title = "Wrong".to_string();
        let err = ValidatedTimestamps::validate(&set_list(), None, &songs).unwrap_err();
        assert!(matches!(
            err,
            TimestampValidationError::TitleMismatch { index: 1, .. }
        ));
    }

    #[test]
    fn validate_negative_start() {
        let mut songs = valid_songs();
        songs[0].start_time = -1.0;
        let err = ValidatedTimestamps::validate(&set_list(), None, &songs).unwrap_err();
        assert_eq!(err, TimestampValidationError::NegativeStart { index: 0 });
    }

    #[test]
    fn validate_too_short() {
        let mut songs = valid_songs();
        songs[0].end_time = songs[0].start_time + 0.5;
        let err = ValidatedTimestamps::validate(&set_list(), None, &songs).unwrap_err();
        assert!(matches!(
            err,
            TimestampValidationError::TooShort { index: 0, .. }
        ));
    }

    #[test]
    fn validate_exact_minimum_duration_ok() {
        let mut songs = valid_songs();
        songs[0].end_time = songs[0].start_time + MIN_SONG_DURATION_SECONDS;
        ValidatedTimestamps::validate(&set_list(), None, &songs).unwrap();
    }

    #[test]
    fn validate_overlap() {
        let mut songs = valid_songs();
        // Alpha ends at 110, Beta starts at 105 — overlap
        songs[0].end_time = 110.0;
        songs[1].start_time = 105.0;
        let err = ValidatedTimestamps::validate(&set_list(), None, &songs).unwrap_err();
        assert_eq!(err, TimestampValidationError::Overlap { index: 0 });
    }

    #[test]
    fn validate_adjacent_ok() {
        let mut songs = valid_songs();
        // Alpha ends exactly where Beta starts — no gap, no overlap — allowed.
        songs[0].end_time = 100.0;
        songs[1].start_time = 100.0;
        ValidatedTimestamps::validate(&set_list(), None, &songs).unwrap();
    }

    #[test]
    fn validate_beyond_media_duration() {
        let mut songs = valid_songs();
        songs[2].end_time = 400.0;
        let err = ValidatedTimestamps::validate(&set_list(), Some(350.0), &songs).unwrap_err();
        assert!(matches!(
            err,
            TimestampValidationError::BeyondMediaDuration {
                index: 2,
                end_time: 400.0,
                duration: 350.0
            }
        ));
    }

    #[test]
    fn validate_within_media_duration_ok() {
        ValidatedTimestamps::validate(&set_list(), Some(400.0), &valid_songs()).unwrap();
    }

    #[test]
    fn validate_for_reset_set_list_count_changed() {
        let songs = vec![
            valid_songs()[0].clone_for_test(),
            valid_songs()[1].clone_for_test(),
        ];
        let err = ValidatedTimestamps::validate_for_reset(&set_list(), &songs).unwrap_err();
        assert!(matches!(
            err,
            TimestampValidationError::SetListChangedSinceAnalysis { .. }
        ));
    }

    #[test]
    fn validate_for_reset_title_changed() {
        let mut songs = valid_songs();
        songs[0].title = "Different".to_string();
        let err = ValidatedTimestamps::validate_for_reset(&set_list(), &songs).unwrap_err();
        assert!(matches!(
            err,
            TimestampValidationError::SetListChangedSinceAnalysis { .. }
        ));
    }

    #[test]
    fn to_timestamps_file_round_trips() {
        let ts = ValidatedTimestamps::validate(&set_list(), None, &valid_songs()).unwrap();
        let file = ts.to_timestamps_file();
        assert_eq!(file.songs.len(), 3);
        assert_eq!(file.songs[0].title, "Alpha");
    }

    #[test]
    fn song_timestamps_to_payload_conversion() {
        let ts = ValidatedTimestamps::validate(&set_list(), None, &valid_songs()).unwrap();
        let payload = song_timestamps_to_payload(ts.songs());
        assert_eq!(payload.len(), 3);
        assert_eq!(payload[0].title, "Alpha");
        assert_eq!(payload[0].start_time, 0.0);
        assert_eq!(payload[0].end_time, 100.0);
    }

    #[test]
    fn split_timestamps_response_serializes_media_duration() {
        let resp = SplitTimestampsResponse {
            set_list: vec!["Song A".to_string()],
            auto: Some(vec![SongTimestamp {
                title: "Song A".to_string(),
                start_time: 0.0,
                end_time: 180.0,
                duration: 180.0,
            }]),
            user: None,
            media_duration: Some(212.5),
        };
        let v: serde_json::Value = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["media_duration"], 212.5);
        assert!(v["user"].is_null());
        assert_eq!(v["auto"][0]["end_time"], 180.0);

        let resp2 = SplitTimestampsResponse {
            set_list: vec![],
            auto: None,
            user: None,
            media_duration: None,
        };
        let v2: serde_json::Value = serde_json::to_value(&resp2).unwrap();
        assert!(v2["media_duration"].is_null());
    }

    #[tokio::test]
    async fn read_workflow_returns_stored_auto_and_user_timestamps() {
        let conn = db::connection::open_in_memory().unwrap();
        let songs = ["Alpha", "Beta"];
        let id = seed_ts_concert(&conn, "Stored Workflow Album", &songs);
        let auto = sample_timestamps(&songs);
        db::split_timestamps::set_auto_split_timestamps(&conn, id, &auto).unwrap();
        let user = vec![
            SongTimestamp {
                title: "Alpha".to_string(),
                start_time: 1.0,
                end_time: 50.0,
                duration: 49.0,
            },
            SongTimestamp {
                title: "Beta".to_string(),
                start_time: 55.0,
                end_time: 100.0,
                duration: 45.0,
            },
        ];
        db::split_timestamps::set_user_split_timestamps(&conn, id, &user).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let workdir = tempfile::tempdir().unwrap();

        let read = read_split_timestamps(db, workdir.path(), id).await.unwrap();

        assert_eq!(read.set_list, vec!["Alpha", "Beta"]);
        assert_eq!(read.auto.unwrap()[0].title, "Alpha");
        assert_eq!(read.user.unwrap()[0].start_time, 1.0);
        assert_eq!(read.media_duration, None);
    }

    #[tokio::test]
    async fn read_workflow_lazy_backfills_auto_timestamps_from_disk() {
        let conn = db::connection::open_in_memory().unwrap();
        let album = "Backfill Workflow Album";
        let songs = ["Old A", "Old B"];
        let id = seed_ts_concert(&conn, album, &songs);
        let workdir = tempfile::tempdir().unwrap();
        let concert_dir = concert_dir(workdir.path(), album);
        std::fs::create_dir_all(&concert_dir).unwrap();
        let timestamps_json = serde_json::to_string(&concert_types::ConcertInfo {
            artist: "Test".to_string(),
            source: String::new(),
            show: String::new(),
            date: None,
            album: album.to_string(),
            description: None,
            set_list: vec![],
            musicians: vec![],
            preview_image_url: None,
            teaser: None,
            timestamps: Some(sample_timestamps(&songs)),
        })
        .unwrap();
        std::fs::write(concert_dir.join("timestamps.json"), timestamps_json).unwrap();
        let db = Arc::new(Mutex::new(conn));

        let read = read_split_timestamps(db.clone(), workdir.path(), id)
            .await
            .unwrap();

        assert_eq!(read.auto.unwrap().len(), 2);
        let stored = db::split_timestamps::get_split_timestamps(&db.lock().unwrap(), id).unwrap();
        assert!(
            stored.auto.is_some(),
            "backfill should persist auto timestamps"
        );
    }

    #[tokio::test]
    async fn read_workflow_uses_stored_duration_when_source_absent() {
        let conn = db::connection::open_in_memory().unwrap();
        let id = seed_ts_concert(&conn, "Duration Workflow Album", &["A"]);
        db::split_timestamps::set_media_duration(&conn, id, 123.5).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let workdir = tempfile::tempdir().unwrap();

        let read = read_split_timestamps(db, workdir.path(), id).await.unwrap();

        assert_eq!(read.media_duration, Some(123.5));
    }

    #[tokio::test]
    async fn apply_user_timestamps_conflicts_when_source_missing() {
        let conn = db::connection::open_in_memory().unwrap();
        let id = seed_ts_concert(&conn, "Missing Source Workflow Album", &["A", "B"]);
        let db = Arc::new(Mutex::new(conn));
        let workdir = tempfile::tempdir().unwrap();

        let err = apply_user_timestamps(
            db.clone(),
            Arc::new(JobRegistry::new()),
            JobConfig::test(workdir.path().to_path_buf()),
            id,
            payload_for(&["A", "B"]),
        )
        .await
        .unwrap_err();

        assert!(matches!(
            err,
            SplitTimestampWorkflowError::Conflict(ref msg)
                if msg == "Source file not found — download the concert first"
        ));
    }

    #[tokio::test]
    async fn apply_user_timestamps_rejects_count_mismatch_before_ffprobe() {
        let conn = db::connection::open_in_memory().unwrap();
        let album = "Count Workflow Album";
        let id = seed_ts_concert(&conn, album, &["A", "B"]);
        let workdir = tempfile::tempdir().unwrap();
        let concert_dir = concert_dir(workdir.path(), album);
        std::fs::create_dir_all(&concert_dir).unwrap();
        std::fs::write(concert_dir.join(format!("{album}.mp4")), b"not media").unwrap();
        let db = Arc::new(Mutex::new(conn));

        let err = apply_user_timestamps(
            db,
            Arc::new(JobRegistry::new()),
            JobConfig::test(workdir.path().to_path_buf()),
            id,
            payload_for(&["A"]),
        )
        .await
        .unwrap_err();

        assert!(matches!(
            err,
            SplitTimestampWorkflowError::Unprocessable(ref msg)
                if msg == "Expected 2 timestamps (one per set-list song), got 1"
        ));
    }

    #[tokio::test]
    async fn reset_to_auto_returns_already_auto_when_user_timestamps_are_absent() {
        let conn = db::connection::open_in_memory().unwrap();
        let songs = ["A", "B"];
        let id = seed_ts_concert(&conn, "Already Auto Workflow Album", &songs);
        db::split_timestamps::set_auto_split_timestamps(&conn, id, &sample_timestamps(&songs))
            .unwrap();
        let db = Arc::new(Mutex::new(conn));
        let workdir = tempfile::tempdir().unwrap();

        let outcome = reset_to_auto_timestamps(
            db,
            Arc::new(JobRegistry::new()),
            JobConfig::test(workdir.path().to_path_buf()),
            id,
        )
        .await
        .unwrap();

        assert_eq!(outcome, SplitStartOutcome::AlreadyAuto);
    }

    #[tokio::test]
    async fn reset_to_auto_rejects_missing_auto_timestamps() {
        let conn = db::connection::open_in_memory().unwrap();
        let id = seed_ts_concert(&conn, "No Auto Workflow Album", &["A"]);
        let db = Arc::new(Mutex::new(conn));
        let workdir = tempfile::tempdir().unwrap();

        let err = reset_to_auto_timestamps(
            db,
            Arc::new(JobRegistry::new()),
            JobConfig::test(workdir.path().to_path_buf()),
            id,
        )
        .await
        .unwrap_err();

        assert!(matches!(
            err,
            SplitTimestampWorkflowError::Unprocessable(ref msg)
                if msg == "No automated split timestamps available — run analysis first"
        ));
    }

    #[tokio::test]
    async fn reset_to_auto_rejects_stale_auto_timestamps() {
        let conn = db::connection::open_in_memory().unwrap();
        let id = seed_ts_concert(&conn, "Stale Auto Workflow Album", &["A", "B"]);
        db::split_timestamps::set_auto_split_timestamps(&conn, id, &sample_timestamps(&["A"]))
            .unwrap();
        db::split_timestamps::set_user_split_timestamps(&conn, id, &sample_timestamps(&["A", "B"]))
            .unwrap();
        let db = Arc::new(Mutex::new(conn));
        let workdir = tempfile::tempdir().unwrap();

        let err = reset_to_auto_timestamps(
            db,
            Arc::new(JobRegistry::new()),
            JobConfig::test(workdir.path().to_path_buf()),
            id,
        )
        .await
        .unwrap_err();

        assert!(matches!(
            err,
            SplitTimestampWorkflowError::Unprocessable(ref msg)
                if msg.contains("set list has 2 tracks but automated timestamps have 1")
        ));
    }
}
