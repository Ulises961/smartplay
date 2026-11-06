//! Server-agnostic core. No I/O: everything it needs from a server goes through [`Subsonic`].

use serde::Deserialize;

pub mod identity;
pub mod library;
pub mod mix;
pub mod normalize;
pub mod playlist;
pub mod protocol;
pub mod taste;

/// A way to call a Subsonic-compatible server. `request` is the endpoint with its query,
/// e.g. `getStarred2?u=me`, and the answer is the raw JSON body.
pub trait Subsonic {
    fn call(&self, request: &str) -> Result<String, String>;
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "subsonic-response")]
    response: Response,
}

#[derive(Deserialize)]
struct Response {
    status: String,
    #[serde(default)]
    starred2: Starred,
}

#[derive(Deserialize, Default)]
struct Starred {
    #[serde(default)]
    song: Vec<Song>,
}

#[derive(Deserialize)]
struct Song {
    id: String,
}

/// Ids of the user's starred songs.
pub fn starred_ids(api: &dyn Subsonic, user: &str) -> Result<Vec<String>, String> {
    let body = api.call(&format!("getStarred2?u={}", encode(user)))?;
    let env: Envelope = serde_json::from_str(&body).map_err(|e| format!("bad response: {e}"))?;
    if env.response.status != "ok" {
        return Err(format!("server status {}", env.response.status));
    }
    Ok(env
        .response
        .starred2
        .song
        .into_iter()
        .map(|s| s.id)
        .collect())
}

/// Percent-encodes everything but unreserved characters.
pub fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
