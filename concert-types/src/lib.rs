use serde::{Deserialize, Serialize};

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

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
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
