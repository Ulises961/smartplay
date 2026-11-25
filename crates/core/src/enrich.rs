//! What outside services know about artists: the tags people give them and the artists they are close to.
//!
//! This is the data and the reading of the answers of Last.fm; fetching them is the engine's job, so nothing here touches a
//! network. Crowd tags are about an artist, not a song: they place an artist in a neighbourhood (`trip hop`, `chillout`)
//! and say little about which of its songs is which. The tags a library carries stay what they were; these are added beside
//! them, and everything works without them.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::normalize::{fold, work_title};

/// A tag counts when it is at least this share of the artist's strongest tag (Last.fm scales the strongest to 100).
const MIN_TAG_WEIGHT: f64 = 0.1;
/// An artist is described by at most this many tags.
const MAX_TAGS: usize = 10;
/// Tags that say nothing about the music: how people use their lists, not what the music is.
const NOISE: &[&str] = &[
    "seen live",
    "favorites",
    "favourites",
    "favorite",
    "favourite",
    "albums i own",
    "my music",
    "love",
    "loved",
    "awesome",
    "all",
    "spotify",
    "under 2000 listeners",
];

/// What is known of one artist.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct ArtistInfo {
    /// Folded tag names and their weight, between 0 and 1, the strongest first.
    pub tags: Vec<(String, f64)>,
    /// Names of close artists and how close, between 0 and 1, the closest first.
    pub similar: Vec<(String, f64)>,
    /// The artist's most played tracks on the service: the title of the work (see `normalize::work_title`) and how many
    /// times it was played, the most played first. `None` when the service was not asked yet, which is not the same as an
    /// artist with no tracks to list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<Vec<(String, u64)>>,
}

/// What is known of the artists of a library, by folded artist name.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Enrichment {
    artists: HashMap<String, ArtistInfo>,
}

impl Enrichment {
    pub fn get(&self, artist: &str) -> Option<&ArtistInfo> {
        self.artists.get(&fold(artist))
    }

    pub fn insert(&mut self, artist: &str, info: ArtistInfo) {
        self.artists.insert(fold(artist), info);
    }

    pub fn is_empty(&self) -> bool {
        self.artists.is_empty()
    }

    pub fn len(&self) -> usize {
        self.artists.len()
    }
}

/// Why an answer gave nothing.
#[derive(Debug, PartialEq)]
pub enum Failure {
    /// The service does not know the artist. It is safe to remember that.
    Unknown,
    /// The service could not answer now (busy, down, rate limited, a bad key): not an answer about the artist, so nothing
    /// is to be remembered about it.
    Temporary(String),
}

#[derive(Deserialize)]
struct Answer<T> {
    #[serde(flatten)]
    body: Option<T>,
    error: Option<i64>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct TopTags {
    toptags: TagList,
}
#[derive(Deserialize)]
struct TagList {
    #[serde(default)]
    tag: Vec<RawTag>,
}
#[derive(Deserialize)]
struct RawTag {
    name: String,
    #[serde(default, deserialize_with = "number")]
    count: f64,
}

#[derive(Deserialize)]
struct TopTracks {
    toptracks: TrackList,
}
#[derive(Deserialize)]
struct TrackList {
    /// Absent when the answer has no list at all, which is not the same as an empty one; one track may come alone.
    track: Option<OneOrMany<RawTrack>>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany<T> {
    Many(Vec<T>),
    One(T),
}
#[derive(Deserialize)]
struct RawTrack {
    name: String,
    #[serde(default, deserialize_with = "number")]
    playcount: f64,
}

#[derive(Deserialize)]
struct Similar {
    similarartists: ArtistList,
}
#[derive(Deserialize)]
struct ArtistList {
    #[serde(default)]
    artist: Vec<RawArtist>,
}
#[derive(Deserialize)]
struct RawArtist {
    name: String,
    #[serde(default, rename = "match", deserialize_with = "number")]
    matched: f64,
}

/// Numbers arrive as numbers or as text (`"match":"1"`); anything else is zero.
fn number<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0),
        serde_json::Value::String(s) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    })
}

/// The service's own answer when it gives an error instead of data: 6 is "no such artist"; every other code (a bad key, a
/// busy service, a rate limit) is a reason to try later.
fn failure(error: i64, message: Option<String>) -> Failure {
    match error {
        6 => Failure::Unknown,
        code => Failure::Temporary(format!("error {code}: {}", message.unwrap_or_default())),
    }
}

fn read<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T, Failure> {
    let answer: Answer<T> =
        serde_json::from_str(text).map_err(|e| Failure::Temporary(format!("bad answer: {e}")))?;
    match (answer.body, answer.error) {
        (_, Some(code)) => Err(failure(code, answer.message)),
        (Some(body), None) => Ok(body),
        (None, None) => Err(Failure::Temporary("an answer with no data".into())),
    }
}

/// The tags in an `artist.getTopTags` answer: folded, merged when they fold to one (`trip-hop`, `trip hop`), without the
/// tags that describe a list and not the music, the strongest first, with weights between 0 and 1.
pub fn parse_top_tags(text: &str) -> Result<Vec<(String, f64)>, Failure> {
    let parsed: TopTags = read(text)?;
    let top = parsed
        .toptags
        .tag
        .iter()
        .map(|t| t.count)
        .fold(0.0, f64::max);
    if top <= 0.0 {
        return Ok(vec![]);
    }
    let mut merged: HashMap<String, f64> = HashMap::new();
    for tag in parsed.toptags.tag {
        let name = fold(&tag.name);
        if name.is_empty() || NOISE.contains(&name.as_str()) {
            continue;
        }
        *merged.entry(name).or_default() += tag.count / top;
    }
    let mut tags: Vec<(String, f64)> = merged
        .into_iter()
        .map(|(name, weight)| (name, weight.min(1.0)))
        .filter(|(_, weight)| *weight >= MIN_TAG_WEIGHT)
        .collect();
    tags.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    tags.truncate(MAX_TAGS);
    Ok(tags)
}

/// The tracks in an `artist.getTopTracks` answer, by the title of the work (see `normalize::work_title`), the most played
/// first. Versions of one work (`Fever`, `Fever - Live`) count as the most played of them. An answer with no list of tracks
/// is not an answer about the artist (it is how a service that changed its reply would look), so it is "could not answer".
pub fn parse_top_tracks(text: &str) -> Result<Vec<(String, u64)>, Failure> {
    let parsed: TopTracks = read(text)?;
    let tracks = match parsed.toptracks.track {
        Some(OneOrMany::Many(list)) => list,
        Some(OneOrMany::One(one)) => vec![one],
        None => {
            return Err(Failure::Temporary(
                "an answer with no list of tracks".into(),
            ))
        }
    };
    let mut best: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for track in tracks {
        let title = work_title(&track.name);
        if !title.is_empty() && track.playcount > 0.0 {
            let played = best.entry(title).or_default();
            *played = (*played).max(track.playcount as u64);
        }
    }
    let mut top: Vec<(String, u64)> = best.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(top)
}

/// The artists in an `artist.getSimilar` answer, the closest first, with how close between 0 and 1.
pub fn parse_similar(text: &str) -> Result<Vec<(String, f64)>, Failure> {
    let parsed: Similar = read(text)?;
    let mut similar: Vec<(String, f64)> = parsed
        .similarartists
        .artist
        .into_iter()
        .filter(|a| !a.name.trim().is_empty())
        .map(|a| (a.name, a.matched.clamp(0.0, 1.0)))
        .collect();
    similar.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(similar)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAGS: &str = r#"{"toptags":{"tag":[
        {"count":100,"name":"trip-hop","url":"u"},{"count":55,"name":"electronic","url":"u"},
        {"count":26,"name":"female vocalists","url":"u"},{"count":24,"name":"seen live","url":"u"},
        {"count":7,"name":"trip hop","url":"u"},{"count":3,"name":"british","url":"u"}],
        "@attr":{"artist":"Portishead"}}}"#;

    #[test]
    fn tags_are_folded_merged_scaled_and_stripped_of_what_describes_a_list() {
        let tags = parse_top_tags(TAGS).unwrap();
        let names: Vec<&str> = tags.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["trip hop", "electronic", "female vocalists"]);
        assert!(
            (tags[0].1 - 1.0).abs() < 1e-9,
            "trip-hop 100 and trip hop 7 merge and cap at 1"
        );
        assert!((tags[1].1 - 0.55).abs() < 1e-9);
        assert!(
            !names.contains(&"british"),
            "3 of 100 is under the minimum weight"
        );
    }

    #[test]
    fn the_top_tracks_are_by_work_most_played_first_and_a_lone_track_is_a_list() {
        let text = r#"{"toptracks":{"track":[
            {"name":"Fever","playcount":"500","listeners":"9","mbid":"","url":"u","@attr":{"rank":"1"}},
            {"name":"Fever - Live","playcount":"900","listeners":"3","url":"u"},
            {"name":"Houdini","playcount":1200,"url":"u"},
            {"name":"Silence","playcount":"0","url":"u"},
            {"name":" ","playcount":"7","url":"u"}],
            "@attr":{"artist":"Dua Lipa","page":"1","perPage":"50","totalPages":"9","total":"430"}}}"#;
        assert_eq!(
            parse_top_tracks(text).unwrap(),
            vec![("houdini".to_string(), 1200), ("fever".to_string(), 900)]
        );
        let alone = r#"{"toptracks":{"track":{"name":"Only One","playcount":"12"}}}"#;
        assert_eq!(
            parse_top_tracks(alone).unwrap(),
            vec![("only one".to_string(), 12)]
        );
        assert_eq!(
            parse_top_tracks(r#"{"toptracks":{"track":[]}}"#).unwrap(),
            vec![]
        );
    }

    #[test]
    fn an_answer_without_a_list_of_tracks_is_not_taken_for_an_artist_with_none() {
        for odd in [
            r#"{"toptracks":{"@attr":{"artist":"X"}}}"#,
            r#"{"tracks":{"track":[]}}"#,
            "<lfm>xml</lfm>",
        ] {
            assert!(
                matches!(parse_top_tracks(odd), Err(Failure::Temporary(_))),
                "{odd}"
            );
        }
        assert_eq!(
            parse_top_tracks(r#"{"error":6,"message":"no such artist"}"#),
            Err(Failure::Unknown)
        );
    }

    #[test]
    fn an_artist_with_no_tags_has_none() {
        assert_eq!(
            parse_top_tags(r#"{"toptags":{"tag":[],"@attr":{"artist":"X"}}}"#).unwrap(),
            vec![]
        );
    }

    #[test]
    fn similar_artists_come_closest_first_and_the_match_may_be_text() {
        let text = r#"{"similarartists":{"artist":[
            {"name":"Massive Attack","match":"0.8","url":"u"},
            {"name":"Pati Yang","match":"1","url":"u"},
            {"name":" ","match":"0.9"}],"@attr":{"artist":"Portishead"}}}"#;
        assert_eq!(
            parse_similar(text).unwrap(),
            vec![
                ("Pati Yang".to_string(), 1.0),
                ("Massive Attack".to_string(), 0.8)
            ]
        );
    }

    #[test]
    fn an_artist_the_service_does_not_know_is_told_apart_from_a_service_that_could_not_answer() {
        let unknown = r#"{"error":6,"message":"The artist you supplied could not be found"}"#;
        assert_eq!(parse_top_tags(unknown), Err(Failure::Unknown));
        assert_eq!(parse_similar(unknown), Err(Failure::Unknown));
        for busy in [
            r#"{"error":29,"message":"Rate limit exceeded"}"#,
            r#"{"error":11,"message":"Service offline"}"#,
            r#"{"error":10,"message":"Invalid API key"}"#,
            "<html>bad gateway</html>",
            "{}",
        ] {
            assert!(
                matches!(parse_top_tags(busy), Err(Failure::Temporary(_))),
                "{busy}"
            );
        }
    }

    #[test]
    fn artists_are_found_by_folded_name() {
        let mut known = Enrichment::default();
        known.insert(
            "Sigur Rós",
            ArtistInfo {
                tags: vec![("post rock".into(), 1.0)],
                similar: vec![],
                top: None,
            },
        );
        assert!(known.get("sigur ros").is_some());
        assert!(known.get("Nobody").is_none());
        assert_eq!(known.len(), 1);
    }
}
