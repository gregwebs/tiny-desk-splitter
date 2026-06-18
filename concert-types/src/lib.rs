use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The `{"songs":[...]}` wire format consumed by `live-set-splitter --timestamps-file`.
/// Shared between the splitter (which writes it) and concert-tracker (which produces it).
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct TimestampsFile {
    pub songs: Vec<SongTimestamp>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Musician {
    pub name: String,
    pub instruments: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Song {
    pub title: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, ToSchema)]
pub struct SongTimestamp {
    pub title: String,
    pub start_time: f64,
    pub end_time: f64,
    pub duration: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ConcertInfo {
    pub artist: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub show: String,
    pub date: Option<String>,
    #[serde(default)]
    pub album: String,
    pub description: Option<String>,
    #[serde(default)]
    pub set_list: Vec<Song>,
    #[serde(default)]
    pub musicians: Vec<Musician>,
    #[serde(default)]
    pub preview_image_url: Option<String>,
    #[serde(default)]
    pub teaser: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamps: Option<Vec<SongTimestamp>>,
}

impl ConcertInfo {
    pub fn year(&self) -> Option<String> {
        self.date
            .as_ref()
            .and_then(|date| date.split('-').next().map(|s| s.to_string()))
    }
}

/// Shortest span worth capturing as a standalone interlude track. Spans shorter
/// than this are treated as negligible (and absorbed by keyframe snapping when
/// cutting), so they do not block source-file deletion.
pub const MIN_INTERLUDE_SECONDS: f64 = 1.0;

/// An uncovered span of the source timeline between (or around) song tracks.
/// Together, song tracks + interlude tracks cover the whole `[0, media_duration]`
/// timeline, making the source file redundant.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq)]
pub struct Interlude {
    /// 1-based position in time order across all interludes of a concert.
    pub index: usize,
    pub start_time: f64,
    pub end_time: f64,
}

/// Filename stem (no extension) for the interlude at `index`. The single
/// formatter shared by the splitter (which writes the file) and concert-tracker
/// (which finds it) so the two can never disagree on the name.
pub fn interlude_filename_stem(index: usize) -> String {
    format!("interlude_{index:02}")
}

/// Derive the ordered interlude spans (head, inter-song, tail) that are not
/// covered by any song, within `[0, media_duration]`. Only spans at least
/// [`MIN_INTERLUDE_SECONDS`] long are emitted. `songs` is assumed to be in time
/// order and non-overlapping (as enforced by validation upstream).
pub fn derive_interludes(songs: &[SongTimestamp], media_duration: f64) -> Vec<Interlude> {
    let mut interludes = Vec::new();
    let push = |start: f64, end: f64, interludes: &mut Vec<Interlude>| {
        if end - start >= MIN_INTERLUDE_SECONDS {
            interludes.push(Interlude {
                index: interludes.len() + 1,
                start_time: start,
                end_time: end,
            });
        }
    };

    let mut cursor = 0.0;
    for song in songs {
        push(cursor, song.start_time, &mut interludes);
        cursor = song.end_time;
    }
    push(cursor, media_duration, &mut interludes);

    interludes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(start: f64, end: f64) -> SongTimestamp {
        SongTimestamp {
            title: "s".to_string(),
            start_time: start,
            end_time: end,
            duration: end - start,
        }
    }

    #[test]
    fn no_gaps_yields_no_interludes() {
        let songs = vec![song(0.0, 100.0), song(100.0, 200.0)];
        assert!(derive_interludes(&songs, 200.0).is_empty());
    }

    #[test]
    fn head_only() {
        let songs = vec![song(10.0, 200.0)];
        let got = derive_interludes(&songs, 200.0);
        assert_eq!(
            got,
            vec![Interlude {
                index: 1,
                start_time: 0.0,
                end_time: 10.0
            }]
        );
    }

    #[test]
    fn tail_only() {
        let songs = vec![song(0.0, 190.0)];
        let got = derive_interludes(&songs, 200.0);
        assert_eq!(
            got,
            vec![Interlude {
                index: 1,
                start_time: 190.0,
                end_time: 200.0
            }]
        );
    }

    #[test]
    fn inter_song_gap() {
        let songs = vec![song(0.0, 90.0), song(100.0, 200.0)];
        let got = derive_interludes(&songs, 200.0);
        assert_eq!(
            got,
            vec![Interlude {
                index: 1,
                start_time: 90.0,
                end_time: 100.0
            }]
        );
    }

    #[test]
    fn head_inter_song_and_tail_indexed_in_time_order() {
        let songs = vec![song(5.0, 90.0), song(100.0, 190.0)];
        let got = derive_interludes(&songs, 200.0);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].index, 1);
        assert_eq!((got[0].start_time, got[0].end_time), (0.0, 5.0));
        assert_eq!(got[1].index, 2);
        assert_eq!((got[1].start_time, got[1].end_time), (90.0, 100.0));
        assert_eq!(got[2].index, 3);
        assert_eq!((got[2].start_time, got[2].end_time), (190.0, 200.0));
    }

    #[test]
    fn sub_threshold_gaps_are_skipped() {
        // 0.5s head gap and 0.5s inter-song gap are below MIN_INTERLUDE_SECONDS.
        let songs = vec![song(0.5, 90.0), song(90.5, 200.0)];
        assert!(derive_interludes(&songs, 200.0).is_empty());
    }

    #[test]
    fn filename_stem_is_zero_padded() {
        assert_eq!(interlude_filename_stem(1), "interlude_01");
        assert_eq!(interlude_filename_stem(12), "interlude_12");
    }
}
