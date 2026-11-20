//! Plays from the history Last.fm keeps for a user (`user.getRecentTracks`): what was heard anywhere, not only on this
//! server, each with the second it started. Fetching is the engine's job; this reads the answer.
//!
//! Last.fm keeps only finished plays, so a play from here says nothing about how it ended.

use serde::Deserialize;

use crate::enrich::Failure;
use crate::plays::Play;

/// One page of the history, newest play first.
#[derive(Debug, PartialEq)]
pub struct Page {
    pub plays: Vec<Play>,
    pub page: u32,
    pub total_pages: u32,
}

#[derive(Deserialize)]
struct Answer {
    recenttracks: Option<Recent>,
    error: Option<i64>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct Recent {
    #[serde(default, deserialize_with = "one_or_many")]
    track: Vec<Track>,
    #[serde(rename = "@attr")]
    attr: Attr,
}

#[derive(Deserialize)]
struct Attr {
    #[serde(deserialize_with = "number")]
    page: u32,
    #[serde(rename = "totalPages", deserialize_with = "number")]
    total_pages: u32,
}

#[derive(Deserialize)]
struct Track {
    name: String,
    artist: Text,
    #[serde(default)]
    album: Option<Text>,
    date: Option<Date>,
    /// Present, as `nowplaying`, on the song being played now, which is no play yet.
    #[serde(rename = "@attr")]
    attr: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Text {
    #[serde(rename = "#text", default)]
    text: String,
}

#[derive(Deserialize)]
struct Date {
    #[serde(deserialize_with = "number")]
    uts: i64,
}

/// Numbers arrive as text (`"page":"1"`) or as numbers.
fn number<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: std::str::FromStr + TryFrom<i64>,
{
    use serde::de::Error;
    match serde_json::Value::deserialize(d)? {
        serde_json::Value::Number(n) => n.as_i64().and_then(|n| T::try_from(n).ok()),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .ok_or_else(|| D::Error::custom("not a number"))
}

/// A list of tracks, which Last.fm gives as one object when there is one track.
fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Track>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Shape {
        Many(Vec<Track>),
        One(Box<Track>),
    }
    Ok(match Shape::deserialize(d)? {
        Shape::Many(many) => many,
        Shape::One(one) => vec![*one],
    })
}

/// The plays in a page of the history, marked as coming from `import:lastfm`. The song being played now is left out, and
/// so is anything that names no artist or no title.
pub fn parse_recent_tracks(text: &str) -> Result<Page, Failure> {
    let answer: Answer =
        serde_json::from_str(text).map_err(|e| Failure::Temporary(format!("bad answer: {e}")))?;
    if let Some(code) = answer.error {
        let message = answer.message.unwrap_or_default();
        return Err(match code {
            6 => Failure::Unknown,
            _ => Failure::Temporary(format!("error {code}: {message}")),
        });
    }
    let recent = answer
        .recenttracks
        .ok_or_else(|| Failure::Temporary("an answer with no history".into()))?;
    let plays = recent
        .track
        .into_iter()
        .filter(|t| t.attr.is_none())
        .filter_map(|t| {
            let at = t.date?.uts;
            let mut play = Play::new("", &[t.artist.text], "", at);
            play.title = t.name;
            play.album = t.album.map(|a| a.text).unwrap_or_default();
            play.source = "import:lastfm".into();
            play.is_usable().then_some(play)
        })
        .collect();
    Ok(Page {
        plays,
        page: recent.attr.page,
        total_pages: recent.attr.total_pages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r##"{"recenttracks":{"track":[
        {"artist":{"mbid":"","#text":"Playing Now"},"name":"Live Song","album":{"#text":"X"},"@attr":{"nowplaying":"true"}},
        {"artist":{"mbid":"m","#text":"Portishead"},"name":"Glory Box","album":{"mbid":"","#text":"Dummy"},
         "date":{"uts":"1700000300","#text":"14 Nov 2023, 22:18"}},
        {"artist":{"#text":"Massive Attack"},"name":"Teardrop","album":{"#text":""},"date":{"uts":"1700000000"}},
        {"artist":{"#text":""},"name":"No Artist","date":{"uts":"1699999000"}}],
        "@attr":{"user":"someone","totalPages":"12","page":"2","perPage":"200","total":"2341"}}}"##;

    #[test]
    fn a_page_gives_its_plays_without_the_song_being_played_now_or_the_ones_with_no_artist() {
        let page = parse_recent_tracks(PAGE).unwrap();
        assert_eq!((page.page, page.total_pages), (2, 12));
        let told: Vec<(&str, &str, i64)> = page
            .plays
            .iter()
            .map(|p| (p.artists[0].as_str(), p.title.as_str(), p.at))
            .collect();
        assert_eq!(
            told,
            [
                ("Portishead", "Glory Box", 1_700_000_300),
                ("Massive Attack", "Teardrop", 1_700_000_000)
            ]
        );
        assert_eq!(page.plays[0].album, "Dummy");
        assert_eq!(page.plays[0].source, "import:lastfm");
        assert_eq!(page.plays[0].ended(), None, "only finished plays are kept");
    }

    #[test]
    fn a_page_with_one_track_is_not_a_list() {
        let one = r##"{"recenttracks":{"track":{"artist":{"#text":"A"},"name":"T","date":{"uts":"5"}},
            "@attr":{"totalPages":"1","page":"1"}}}"##;
        assert_eq!(parse_recent_tracks(one).unwrap().plays.len(), 1);
    }

    #[test]
    fn an_empty_history_is_an_empty_page() {
        let none = r#"{"recenttracks":{"track":[],"@attr":{"totalPages":"0","page":"1"}}}"#;
        assert!(parse_recent_tracks(none).unwrap().plays.is_empty());
    }

    #[test]
    fn a_user_that_is_not_there_is_told_apart_from_a_service_that_could_not_answer() {
        assert_eq!(
            parse_recent_tracks(r#"{"error":6,"message":"User not found"}"#),
            Err(Failure::Unknown)
        );
        for busy in [
            r#"{"error":29,"message":"Rate limit exceeded"}"#,
            r#"{"error":17,"message":"Login: User required to be logged in"}"#,
            "<html>bad gateway</html>",
            "{}",
        ] {
            assert!(
                matches!(parse_recent_tracks(busy), Err(Failure::Temporary(_))),
                "{busy}"
            );
        }
    }
}
