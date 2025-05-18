use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SetMetaData {
    pub artist: String,
    pub album: Option<String>,
    pub date: Option<String>,
    pub show: Option<String>,
}

impl SetMetaData {
    pub fn year(&self) -> Option<String> {
        self.date
            .as_ref()
            .and_then(|date| date.split('-').next().map(|s| s.to_string()))
    }
}