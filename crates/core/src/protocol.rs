//! Messages between a connector (plugin or direct) and the engine. Shared so both sides agree.

use serde::{Deserialize, Serialize};

use crate::taste::Song;

#[derive(Serialize, Deserialize)]
pub struct EnrollRequest {
    pub code: String,
}

#[derive(Serialize, Deserialize)]
pub struct EnrollResponse {
    pub token: String,
}

/// Asks for a user's playlist.
#[derive(Serialize, Deserialize)]
pub struct ComputeRequest {
    /// Server-side user id or name the data belongs to.
    pub user: String,
    pub starred: Vec<String>,
    /// Artists of the albums the user played last, most recent first. Absent from older plugins, which get no mixes.
    #[serde(default)]
    pub recent: Vec<String>,
    /// Tells when the user's library changed (see `library::fingerprint`). The engine compares it with the copy it holds.
    pub fingerprint: String,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ComputeResponse {
    Playlist {
        playlist: String,
        songs: Vec<String>,
        /// More playlists to write beside it: the mixes of the recent artists. Absent from older engines.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mixes: Vec<Mix>,
    },
    /// The engine holds no copy of the library, or an older one: upload it as `LibraryChunk`s, then ask again.
    NeedLibrary,
}

/// A playlist the engine asks the connector to write.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct Mix {
    pub playlist: String,
    pub songs: Vec<String>,
}

/// One page of a user's library. A library is uploaded as consecutive chunks: the first one starts a new copy, the last
/// one makes it the copy the engine uses.
#[derive(Serialize, Deserialize)]
pub struct LibraryChunk {
    pub user: String,
    pub fingerprint: String,
    pub first: bool,
    pub last: bool,
    pub songs: Vec<Song>,
}
