use concert_types::{SongTimestamp, TimestampsFile};
use serde::Deserialize;
use std::fmt;

const MIN_SONG_DURATION_SECONDS: f64 = 1.0;

/// Per-song payload from the POST /concerts/:id/split-timestamps request body.
#[derive(Deserialize)]
pub struct TimestampPayload {
    pub songs: Vec<TimestampPayloadSong>,
}

#[derive(Deserialize)]
pub struct TimestampPayloadSong {
    pub title: String,
    pub start_time: f64,
    pub end_time: f64,
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

#[cfg(test)]
mod tests {
    use super::*;

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
