//! Reading a user's library through the Subsonic API: a fingerprint that tells when it changed, and the songs page by page.

use serde::Deserialize;

use crate::{encode, taste::Song, Subsonic};

/// Songs asked for in one request.
pub const PAGE: usize = 500;

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "subsonic-response")]
    response: Body,
}

#[derive(Deserialize)]
struct Body {
    status: String,
    #[serde(default, rename = "scanStatus")]
    scan_status: Option<Scan>,
    #[serde(default, rename = "searchResult3")]
    search: Option<Search>,
}

#[derive(Deserialize)]
struct Scan {
    #[serde(default)]
    count: u64,
    #[serde(default, rename = "lastScan")]
    last_scan: String,
}

#[derive(Deserialize, Default)]
struct Search {
    #[serde(default)]
    song: Vec<Raw>,
}

#[derive(Deserialize)]
struct Raw {
    id: String,
    #[serde(default)]
    artist: String,
    #[serde(default)]
    album: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    genre: Option<String>,
}

fn body(api: &dyn Subsonic, request: &str) -> Result<Body, String> {
    let text = api.call(request)?;
    let envelope: Envelope =
        serde_json::from_str(&text).map_err(|e| format!("bad response: {e}"))?;
    if envelope.response.status != "ok" {
        return Err(format!("server status {}", envelope.response.status));
    }
    Ok(envelope.response)
}

/// Changes whenever the server's scan changes the library: the song count and the time of the last scan.
pub fn fingerprint(api: &dyn Subsonic, user: &str) -> Result<String, String> {
    let scan = body(api, &format!("getScanStatus?u={}", encode(user)))?
        .scan_status
        .ok_or("the server gave no scan status")?;
    Ok(format!("{}@{}", scan.count, scan.last_scan))
}

/// The songs of the user's library from `offset`, at most [`PAGE`]. A shorter page than [`PAGE`] is the last one.
pub fn songs(api: &dyn Subsonic, user: &str, offset: usize) -> Result<Vec<Song>, String> {
    let request = format!(
        "search3?u={}&query=&artistCount=0&albumCount=0&songCount={PAGE}&songOffset={offset}",
        encode(user)
    );
    let found = body(api, &request)?.search.unwrap_or_default();
    Ok(found
        .song
        .into_iter()
        .map(|s| Song {
            id: s.id,
            artist: s.artist,
            album: s.album,
            title: s.title,
            genre: s.genre.filter(|g| !g.is_empty()),
            artist_mbid: None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server holding `n` songs and a scan status.
    struct Server(usize);

    impl Subsonic for Server {
        fn call(&self, request: &str) -> Result<String, String> {
            if request.starts_with("getScanStatus") {
                return Ok(r#"{"subsonic-response":{"status":"ok","scanStatus":{"scanning":false,"count":7,"lastScan":"2026-10-31T10:00:00Z"}}}"#.into());
            }
            let value = |key: &str| -> usize {
                request
                    .split(&format!("&{key}="))
                    .nth(1)
                    .and_then(|v| v.split('&').next())
                    .and_then(|v| v.parse().ok())
                    .unwrap()
            };
            let (offset, count) = (value("songOffset"), value("songCount"));
            let songs: Vec<String> = (offset..self.0.min(offset + count))
                .map(|i| {
                    format!(r#"{{"id":"s{i}","artist":"A","album":"B","title":"T{i}","genre":""}}"#)
                })
                .collect();
            Ok(format!(
                r#"{{"subsonic-response":{{"status":"ok","searchResult3":{{"song":[{}]}}}}}}"#,
                songs.join(",")
            ))
        }
    }

    #[test]
    fn the_fingerprint_is_the_count_and_the_last_scan() {
        assert_eq!(
            fingerprint(&Server(0), "me").unwrap(),
            "7@2026-10-31T10:00:00Z"
        );
    }

    #[test]
    fn pages_cover_the_library_and_the_last_one_is_short() {
        let server = Server(PAGE + 3);
        let first = songs(&server, "me", 0).unwrap();
        let second = songs(&server, "me", PAGE).unwrap();
        assert_eq!((first.len(), second.len()), (PAGE, 3));
        assert_eq!(second[0].id, format!("s{PAGE}"));
        assert_eq!(first[0].genre, None, "an empty genre is no genre");
    }

    #[test]
    fn an_empty_library_is_one_empty_page() {
        assert!(songs(&Server(0), "me", 0).unwrap().is_empty());
    }

    #[test]
    fn a_failed_status_is_an_error() {
        struct Failing;
        impl Subsonic for Failing {
            fn call(&self, _: &str) -> Result<String, String> {
                Ok(r#"{"subsonic-response":{"status":"failed"}}"#.into())
            }
        }
        assert!(fingerprint(&Failing, "me").is_err());
        assert!(songs(&Failing, "me", 0).is_err());
    }
}
