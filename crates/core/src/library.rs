//! Reading a user's library through the Subsonic API: a fingerprint that tells when it changed, and the songs page by page.

use serde::Deserialize;

use crate::{encode, normalize::fold, taste::Song, Subsonic};

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
    #[serde(default)]
    artists: Option<Artists>,
    #[serde(default)]
    genres: Option<Genres>,
    #[serde(default, rename = "searchResult3")]
    search: Option<Search>,
    #[serde(default, rename = "albumList2")]
    albums: Option<Albums>,
}

#[derive(Deserialize, Default)]
struct Albums {
    #[serde(default)]
    album: Vec<AlbumRaw>,
}

#[derive(Deserialize)]
struct AlbumRaw {
    #[serde(default)]
    artist: String,
}

#[derive(Deserialize)]
struct Scan {
    #[serde(default)]
    count: u64,
}

#[derive(Deserialize, Default)]
struct Artists {
    #[serde(default)]
    index: Vec<Index>,
}

#[derive(Deserialize)]
struct Index {
    #[serde(default)]
    artist: Vec<ArtistRaw>,
}

#[derive(Deserialize)]
struct ArtistRaw {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default, rename = "albumCount")]
    album_count: u64,
}

#[derive(Deserialize, Default)]
struct Genres {
    #[serde(default)]
    genre: Vec<GenreRaw>,
}

#[derive(Deserialize)]
struct GenreRaw {
    #[serde(default)]
    value: String,
    #[serde(default, rename = "songCount")]
    song_count: u64,
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
    #[serde(default)]
    genres: Vec<GenreName>,
    #[serde(default)]
    moods: Vec<String>,
    #[serde(default, deserialize_with = "number")]
    year: Option<u32>,
    #[serde(default, deserialize_with = "number")]
    duration: Option<u32>,
    #[serde(default, deserialize_with = "number")]
    bpm: Option<u32>,
    #[serde(default)]
    created: Option<String>,
}

/// A number the server gives as a number or as text; anything else, or a zero, is no number. One odd value must not
/// make the whole page unreadable.
fn number<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    let n = match value {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    };
    Ok(n.filter(|&n| n > 0).and_then(|n| u32::try_from(n).ok()))
}

#[derive(Deserialize)]
struct GenreName {
    #[serde(default)]
    name: String,
}

/// Tags with no text are no tags.
fn non_empty(tags: impl IntoIterator<Item = String>) -> Vec<String> {
    tags.into_iter().filter(|t| !t.trim().is_empty()).collect()
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

/// The artists of the albums the user played last, most recent first, each once. The server lists albums, not songs, so
/// this is as close to "what they listened to lately" as the API goes. Compilations credited to "Various Artists" name
/// no artist worth seeding a mix with and are left out.
pub fn recent_artists(
    api: &dyn Subsonic,
    user: &str,
    albums: usize,
) -> Result<Vec<String>, String> {
    let request = format!("getAlbumList2?u={}&type=recent&size={albums}", encode(user));
    let found = body(api, &request)?.albums.unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    Ok(found
        .album
        .into_iter()
        .map(|a| a.artist.trim().to_string())
        .filter(|a| !a.is_empty() && fold(a) != "various artists" && seen.insert(fold(a)))
        .collect())
}

/// Changes when what taste reads changes, and only then: the song count, each artist with its album count and each genre
/// with its song count. The time of the last scan is left out on purpose: the server rescans whenever any watched folder
/// changes, a folder of playlists included, and each such scan would send the whole library again.
pub fn fingerprint(api: &dyn Subsonic, user: &str) -> Result<String, String> {
    let u = encode(user);
    let count = body(api, &format!("getScanStatus?u={u}"))?
        .scan_status
        .ok_or("the server gave no scan status")?
        .count;
    let artists = body(api, &format!("getArtists?u={u}"))?
        .artists
        .unwrap_or_default();
    let genres = body(api, &format!("getGenres?u={u}"))?
        .genres
        .unwrap_or_default();

    let mut lines: Vec<String> = artists
        .index
        .iter()
        .flat_map(|i| &i.artist)
        .map(|a| format!("a\t{}\t{}\t{}", a.id, a.name, a.album_count))
        .chain(
            genres
                .genre
                .iter()
                .map(|g| format!("g\t{}\t{}", g.value, g.song_count)),
        )
        .collect();
    // What a song carries changed: a library copied under an older reading is read again, once.
    lines.push(format!("v\t{SONG_FIELDS}"));
    lines.sort();
    Ok(format!(
        "{count}-{:016x}",
        fnv1a(lines.join("\n").as_bytes())
    ))
}

/// Which fields of a song are read; raised whenever one is added, so that copies of a library made before it are replaced.
const SONG_FIELDS: u32 = 2;

/// FNV-1a: a small, dependency-free hash. It only has to tell two libraries apart, not resist anyone.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, b| {
        (hash ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
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
            genres: non_empty(s.genres.into_iter().map(|g| g.name)),
            moods: non_empty(s.moods),
            year: s.year.and_then(|y| u16::try_from(y).ok()),
            duration: s.duration,
            bpm: s.bpm.and_then(|b| u16::try_from(b).ok()),
            created: s.created.filter(|c| !c.trim().is_empty()),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server holding `songs` songs, a scan status, two artists and two genres.
    struct Server {
        songs: usize,
        last_scan: &'static str,
        artist: &'static str,
        rock: u32,
    }

    #[allow(non_snake_case)]
    fn Server(songs: usize) -> Server {
        Server {
            songs,
            last_scan: "2026-10-31T10:00:00Z",
            artist: "Queen",
            rock: 5,
        }
    }

    impl Subsonic for Server {
        fn call(&self, request: &str) -> Result<String, String> {
            if request.starts_with("getScanStatus") {
                return Ok(format!(
                    r#"{{"subsonic-response":{{"status":"ok","scanStatus":{{"scanning":false,"count":7,"lastScan":"{}"}}}}}}"#,
                    self.last_scan
                ));
            }
            if request.starts_with("getArtists") {
                return Ok(format!(
                    r#"{{"subsonic-response":{{"status":"ok","artists":{{"index":[{{"name":"Q","artist":[{{"id":"a1","name":"{}","albumCount":3}}]}},{{"name":"A","artist":[{{"id":"a2","name":"ABBA","albumCount":2}}]}}]}}}}}}"#,
                    self.artist
                ));
            }
            if request.starts_with("getGenres") {
                return Ok(format!(
                    r#"{{"subsonic-response":{{"status":"ok","genres":{{"genre":[{{"value":"Rock","songCount":{},"albumCount":2}},{{"value":"Pop","songCount":2,"albumCount":1}}]}}}}}}"#,
                    self.rock
                ));
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
            let songs: Vec<String> = (offset..self.songs.min(offset + count))
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

    fn print(server: &Server) -> String {
        fingerprint(server, "me").unwrap()
    }

    #[test]
    fn a_scan_that_changes_nothing_keeps_the_fingerprint() {
        // The server rescans whenever any watched folder changes, such as one that only holds playlists.
        let later = Server {
            last_scan: "2026-10-31T10:05:00Z",
            ..Server(0)
        };
        assert_eq!(print(&Server(0)), print(&later));
    }

    #[test]
    fn renaming_an_artist_changes_the_fingerprint() {
        let renamed = Server {
            artist: "Queen II",
            ..Server(0)
        };
        assert_ne!(print(&Server(0)), print(&renamed));
    }

    #[test]
    fn a_song_changing_genre_changes_the_fingerprint() {
        let regenred = Server {
            rock: 4,
            ..Server(0)
        };
        assert_ne!(print(&Server(0)), print(&regenred));
    }

    #[test]
    fn the_fingerprint_is_short_and_stable() {
        let a = print(&Server(0));
        assert_eq!(a, print(&Server(0)));
        assert!(a.len() < 40, "{a}");
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
    fn every_genre_and_mood_of_a_song_is_read() {
        struct Tagged;
        impl Subsonic for Tagged {
            fn call(&self, _: &str) -> Result<String, String> {
                Ok(
                    r#"{"subsonic-response":{"status":"ok","searchResult3":{"song":[
                    {"id":"a","artist":"A","album":"B","title":"T","genre":"Rock",
                     "genres":[{"name":"Rock"},{"name":"Indie"},{"name":" "}],"moods":["Calm",""]},
                    {"id":"b","artist":"A","album":"B","title":"U"}]}}}"#
                        .into(),
                )
            }
        }
        let found = songs(&Tagged, "me", 0).unwrap();
        assert_eq!(found[0].genres, vec!["Rock", "Indie"]);
        assert_eq!(found[0].moods, vec!["Calm"]);
        assert!(found[1].genres.is_empty() && found[1].moods.is_empty());
    }

    #[test]
    fn year_length_tempo_and_date_added_are_read_and_an_odd_value_is_only_missing() {
        struct Fields;
        impl Subsonic for Fields {
            fn call(&self, _: &str) -> Result<String, String> {
                Ok(
                    r#"{"subsonic-response":{"status":"ok","searchResult3":{"song":[
                    {"id":"a","year":1994,"duration":215,"bpm":120,"created":"2026-10-30T10:00:00Z"},
                    {"id":"b","year":"1994","duration":0,"bpm":"fast","created":" "},
                    {"id":"c","year":-3,"duration":1e12,"bpm":null}]}}}"#
                        .into(),
                )
            }
        }
        let found = songs(&Fields, "me", 0).unwrap();
        assert_eq!(
            (found[0].year, found[0].duration, found[0].bpm),
            (Some(1994), Some(215), Some(120))
        );
        assert_eq!(found[0].created.as_deref(), Some("2026-10-30T10:00:00Z"));
        assert_eq!(
            (
                found[1].year,
                found[1].duration,
                found[1].bpm,
                &found[1].created
            ),
            (Some(1994), None, None, &None),
            "text that is a number counts; zero, words and blanks do not"
        );
        assert_eq!(
            (found[2].year, found[2].duration, found[2].bpm),
            (None, None, None)
        );
    }

    struct Recent(&'static str);
    impl Subsonic for Recent {
        fn call(&self, request: &str) -> Result<String, String> {
            assert!(request.starts_with("getAlbumList2?u=me&type=recent&size=5"));
            Ok(format!(
                r#"{{"subsonic-response":{{"status":"ok","albumList2":{}}}}}"#,
                self.0
            ))
        }
    }

    #[test]
    fn recent_artists_come_most_recent_first_once_each() {
        let server = Recent(
            r#"{"album":[{"artist":"Band B"},{"artist":"Various Artists"},{"artist":"Band A"},
                {"artist":"band  b"},{"artist":""}]}"#,
        );
        assert_eq!(
            recent_artists(&server, "me", 5).unwrap(),
            vec!["Band B", "Band A"]
        );
    }

    #[test]
    fn a_user_who_played_nothing_has_no_recent_artists() {
        // The server answers an empty object, not an empty list.
        assert!(recent_artists(&Recent("{}"), "me", 5).unwrap().is_empty());
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
