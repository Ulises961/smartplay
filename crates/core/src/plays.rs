//! The plays the engine has been told about, newest last, and what a mix takes from them.
//!
//! A play is a neutral event: the song (by its id on the server when it is known, else by artist and title), when it
//! started, and, when the source knows them, how much was heard and how it ended. Plays of songs the server does not
//! hold are kept too: what a user plays elsewhere says as much about their taste. Plays are kept for [`RETENTION`]
//! and then dropped; nothing here needs the history to be long, and every field beyond the song and the time is optional.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::normalize::fold;

/// How long a play is kept: two years.
pub const RETENTION: i64 = 2 * 365 * 24 * 60 * 60;

/// Two reports of one song whose starts are this many seconds apart or less are one play: a server that could not be sure
/// the engine heard a play sends it again, and two sources may both tell about it.
pub const DUPLICATE_WINDOW: i64 = 10;

/// Less than this share of a track heard is a skip, and this share or more is a play heard in full.
pub const SKIP_BELOW: f64 = 0.35;
pub const FULL_FROM: f64 = 0.9;

/// How a play ended.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Ended {
    /// Cut short: the user changed the song almost at once.
    Skip,
    /// Heard for a good while, not to the end.
    Heard,
    /// Heard to the end, or nearly.
    Full,
}

/// One play.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Play {
    /// The song's id on the server; empty when the source does not know the server (a play elsewhere).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub song: String,
    /// The artists credited on the song, the main one first.
    pub artists: Vec<String>,
    /// When the song started playing, in seconds since the epoch.
    pub at: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub album: String,
    /// How much of the song was heard, in milliseconds, when the source says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listened_ms: Option<u64>,
    /// How long the song is, in milliseconds, when the source says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_ms: Option<u64>,
    /// How the play ended, when the source says; otherwise [`Play::ended`] works it out from the two times above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<Ended>,
    /// Where the play came from (`scrobbler`, `import:NAME`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
}

impl Play {
    /// A play of `song`. `credited` are the artists the server lists for the song; when it lists none, the artist as
    /// the server displays it stands in.
    pub fn new(song: &str, credited: &[String], displayed: &str, at: i64) -> Play {
        let mut artists: Vec<String> = credited
            .iter()
            .map(|a| a.trim())
            .filter(|a| !a.is_empty())
            .map(String::from)
            .collect();
        if artists.is_empty() && !displayed.trim().is_empty() {
            artists.push(displayed.trim().into());
        }
        Play {
            song: song.into(),
            artists,
            at,
            ..Play::default()
        }
    }

    /// How the play ended: what the source said, else a skip, a play heard or one heard in full by the share of the song
    /// that was heard, else nothing (a source that reports only finished plays does not say).
    pub fn ended(&self) -> Option<Ended> {
        if self.ended.is_some() {
            return self.ended;
        }
        let share = self.listened_ms? as f64 / self.track_ms.filter(|&t| t > 0)? as f64;
        Some(if share < SKIP_BELOW {
            Ended::Skip
        } else if share < FULL_FROM {
            Ended::Heard
        } else {
            Ended::Full
        })
    }

    /// Whether the play names a song (by id, or by title) and an artist: otherwise it says nothing a mix can use.
    pub fn is_usable(&self) -> bool {
        (!self.song.trim().is_empty() || !self.title.trim().is_empty())
            && main_artist(self).is_some()
    }
}

/// Whether both plays are of one song: the same id on the server, or, when either has none, the same main artist and title.
/// Versions of a song (a remix, a live take) are other songs.
fn same_song(a: &Play, b: &Play) -> bool {
    if !a.song.is_empty() && !b.song.is_empty() {
        return a.song == b.song;
    }
    let title = |p: &Play| fold(&p.title);
    let artist = |p: &Play| main_artist(p).map(fold);
    !title(a).is_empty() && title(a) == title(b) && artist(a).is_some() && artist(a) == artist(b)
}

/// Whether `a` and `b` are one play told twice.
pub fn same_play(a: &Play, b: &Play) -> bool {
    (a.at - b.at).abs() <= DUPLICATE_WINDOW && same_song(a, b)
}

/// Adds `play` to `log`, keeping it in time order. Returns `false`, changing nothing, when the same play is there.
pub fn record(log: &mut Vec<Play>, play: Play) -> bool {
    if log.iter().any(|p| same_play(p, &play)) {
        return false;
    }
    log.push(play);
    log.sort_by_key(|p| p.at); // stable: plays at the same second keep the order they arrived in
    true
}

/// Adds to `log` (in time order, as [`record`] keeps it) the plays of `new` that are not in it yet, each one looking only at
/// the plays near it in time, so that a long history can take a long import. Returns the plays added, in the order given.
pub fn merge_new(log: &mut Vec<Play>, new: Vec<Play>) -> Vec<Play> {
    let mut added = vec![];
    for play in new {
        let from = log.partition_point(|p| p.at < play.at - DUPLICATE_WINDOW);
        let near = log[from..]
            .iter()
            .take_while(|p| p.at <= play.at + DUPLICATE_WINDOW);
        if near.clone().any(|p| same_play(p, &play)) {
            continue;
        }
        let at = log.partition_point(|p| p.at <= play.at);
        log.insert(at, play.clone());
        added.push(play);
    }
    added
}

/// One play of a file to import, as a line of JSON. The fields are those of [`Play`], with `artist` (one name) as a
/// shorthand for `artists`. `at` is in seconds since the epoch, and `listened_ms` and `track_ms` in milliseconds.
#[derive(Deserialize)]
struct ImportLine {
    at: i64,
    #[serde(default)]
    artist: String,
    #[serde(default)]
    artists: Vec<String>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    album: String,
    #[serde(default)]
    song: String,
    #[serde(default)]
    listened_ms: Option<u64>,
    #[serde(default)]
    track_ms: Option<u64>,
    #[serde(default)]
    ended: Option<Ended>,
}

/// The play a line of an import file tells of, marked as coming from `import:<source>`.
pub fn from_import_line(line: &str, source: &str) -> Result<Play, String> {
    let l: ImportLine = serde_json::from_str(line).map_err(|e| e.to_string())?;
    // Milliseconds in the place of seconds is the usual mistake, and it would put every play thousands of years ahead.
    if !(0..4_102_444_800).contains(&l.at) {
        return Err(format!(
            "`at` is {}: seconds since the epoch are meant",
            l.at
        ));
    }
    let artists = if l.artists.is_empty() {
        vec![l.artist]
    } else {
        l.artists
    };
    let mut play = Play::new(&l.song, &artists, "", l.at);
    play.title = l.title;
    play.album = l.album;
    play.listened_ms = l.listened_ms;
    play.track_ms = l.track_ms;
    play.ended = l.ended;
    play.source = format!("import:{source}");
    if play.is_usable() {
        Ok(play)
    } else {
        Err("a play names a song (an id or a title) and an artist".into())
    }
}

/// Drops the plays older than `retention` seconds before `now`.
pub fn prune(log: &mut Vec<Play>, now: i64, retention: i64) {
    log.retain(|p| p.at >= now - retention);
}

/// What the server counted before the engine heard of any play: how many times each song was played. Taken once, from the
/// first copy of a user's library the engine receives; plays told after that are in the history as events. It has no
/// times, no sittings and no skips, so it can only say what was played most of all.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Baseline {
    /// When it was taken, in seconds since the epoch.
    pub at: i64,
    pub tallies: Vec<Tally>,
}

/// One song of a [`Baseline`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Tally {
    pub song: String,
    pub artist: String,
    pub title: String,
    pub count: u32,
    /// When the server says it was last played, as it writes it (an ISO 8601 date and time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_played: Option<String>,
}

/// The main artist of a play: the first one credited.
fn main_artist(play: &Play) -> Option<&str> {
    play.artists
        .iter()
        .map(|a| a.trim())
        .find(|a| !a.is_empty())
}

/// How many artists a mix starts from: the ones played most lately, then the ones played most of all.
pub const LATELY_TOP: usize = 3;
pub const ALL_TIME_TOP: usize = 2;
/// "Lately" is this long, counted back from the latest play and not from the clock: a user who stopped listening a while
/// ago still has the artists they last played. Four weeks.
pub const LATELY: i64 = 28 * 24 * 60 * 60;

/// Plays of each artist, as the artists are spelled once folded: how many, when last, and named as last spelled.
#[derive(Default)]
struct Counts(std::collections::HashMap<String, (usize, i64, String)>);

impl Counts {
    fn add(&mut self, artist: &str, times: usize, at: i64) {
        let entry = self
            .0
            .entry(fold(artist))
            .or_insert((0, at, artist.to_string()));
        entry.0 += times;
        if at >= entry.1 {
            (entry.1, entry.2) = (at, artist.to_string());
        }
    }

    /// The artists, most played first. Ties go to the one played last, then to the folded name, so that the order depends
    /// on nothing but the plays.
    fn ranked(self) -> Vec<String> {
        let mut ranked: Vec<_> = self.0.into_iter().collect();
        ranked.sort_by(|(ka, (na, ta, _)), (kb, (nb, tb, _))| {
            nb.cmp(na).then(tb.cmp(ta)).then_with(|| ka.cmp(kb))
        });
        ranked.into_iter().map(|(_, (_, _, name))| name).collect()
    }
}

/// The artists a user's mixes start from, in this order: the [`LATELY_TOP`] most played in the weeks before the latest
/// play, then [`ALL_TIME_TOP`] more that were played most of all, each artist once.
///
/// Most played of all counts what the server counted before the engine heard of any play (the `baseline`) and the plays of
/// `log` told after it. When fewer than [`LATELY_TOP`] artists were played lately, the artists of the albums the server
/// lists as recent (`recent_albums`, most recent first) fill the places: they cover the time before the engine was told
/// about plays.
pub fn seed_artists(
    log: &[Play],
    baseline: Option<&Baseline>,
    recent_albums: &[String],
) -> Vec<String> {
    let mut seeds: Vec<String> = vec![];
    let mut seen = HashSet::new();
    let mut take = |name: String, seeds: &mut Vec<String>| {
        if seen.insert(fold(&name)) {
            seeds.push(name);
        }
    };

    let since = log.last().map_or(0, |p| p.at) - LATELY;
    let mut lately = Counts::default();
    for p in log.iter().filter(|p| p.at >= since) {
        if let Some(artist) = main_artist(p) {
            lately.add(artist, 1, p.at);
        }
    }
    for name in lately.ranked().into_iter().take(LATELY_TOP) {
        take(name, &mut seeds);
    }
    for name in recent_albums {
        if seeds.len() < LATELY_TOP {
            take(name.clone(), &mut seeds);
        }
    }

    let mut all_time = Counts::default();
    let told_after = baseline.map_or(i64::MIN, |b| b.at);
    for tally in baseline.iter().flat_map(|b| &b.tallies) {
        let artist = tally.artist.trim();
        if !artist.is_empty() {
            all_time.add(artist, tally.count as usize, 0);
        }
    }
    for p in log.iter().filter(|p| p.at > told_after) {
        if let Some(artist) = main_artist(p) {
            all_time.add(artist, 1, p.at);
        }
    }
    let before = seeds.len();
    for name in all_time.ranked() {
        if seeds.len() - before >= ALL_TIME_TOP {
            break;
        }
        take(name, &mut seeds);
    }
    seeds
}

/// How many times each artist was played, by folded name: what the server counted before the engine heard of any play (the
/// `baseline`) and the plays of `log` told after it.
pub fn artist_counts(
    log: &[Play],
    baseline: Option<&Baseline>,
) -> std::collections::BTreeMap<String, usize> {
    let mut counts = std::collections::BTreeMap::new();
    let told_after = baseline.map_or(i64::MIN, |b| b.at);
    for tally in baseline.iter().flat_map(|b| &b.tallies) {
        if !tally.artist.trim().is_empty() {
            *counts.entry(fold(&tally.artist)).or_default() += tally.count as usize;
        }
    }
    for p in log.iter().filter(|p| p.at > told_after) {
        if let Some(artist) = main_artist(p) {
            *counts.entry(fold(artist)).or_default() += 1;
        }
    }
    counts
}

/// Plays further apart than this (their start times) are not part of one sitting. An hour allows for a long track.
pub const SESSION_GAP: i64 = 60 * 60;

/// The plays cut into sittings: a new one starts when a play begins more than [`SESSION_GAP`] after the one before.
/// `log` is in time order, as [`record`] keeps it.
pub fn sessions(log: &[Play]) -> Vec<&[Play]> {
    let mut sittings = vec![];
    let mut start = 0;
    for i in 1..log.len() {
        if log[i].at - log[i - 1].at > SESSION_GAP {
            sittings.push(&log[start..i]);
            start = i;
        }
    }
    if !log.is_empty() {
        sittings.push(&log[start..]);
    }
    sittings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn play(song: &str, artist: &str, at: i64) -> Play {
        Play {
            song: song.into(),
            artists: vec![artist.into()],
            at,
            ..Play::default()
        }
    }

    /// A play of a song the server does not hold: named, not identified.
    fn elsewhere(artist: &str, title: &str, at: i64) -> Play {
        Play {
            artists: vec![artist.into()],
            title: title.into(),
            at,
            ..Play::default()
        }
    }

    #[test]
    fn the_displayed_artist_stands_in_when_the_server_lists_none() {
        let listed = ["Lead".to_string(), " ".to_string()];
        assert_eq!(Play::new("s", &listed, "Lead feat. X", 1).artists, ["Lead"]);
        assert_eq!(
            Play::new("s", &[], " Lead feat. X ", 1).artists,
            ["Lead feat. X"]
        );
        assert!(Play::new("s", &[], "", 1).artists.is_empty());
    }

    #[test]
    fn a_play_sent_twice_is_kept_once() {
        let mut log = vec![];
        assert!(record(&mut log, play("s1", "A", 10)));
        assert!(!record(&mut log, play("s1", "A", 10)));
        assert!(
            !record(&mut log, play("s1", "A", 10 + DUPLICATE_WINDOW)),
            "a start a few seconds off is the same play"
        );
        assert!(
            record(&mut log, play("s1", "A", 11 + DUPLICATE_WINDOW)),
            "the same song again later is a new play"
        );
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn two_sources_telling_of_one_play_make_one_even_when_only_one_knows_the_server() {
        let mut log = vec![];
        let mut known = play("s1", "Sigur Rós", 1000);
        known.title = "Hoppípolla".into();
        assert!(record(&mut log, known));
        assert!(
            !record(&mut log, elsewhere("sigur ros", "hoppipolla", 1004)),
            "the same artist and title, spelled differently"
        );
        assert!(
            record(&mut log, elsewhere("Sigur Rós", "Hoppípolla (live)", 1004)),
            "a live take is another song"
        );
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn plays_of_songs_the_server_does_not_hold_are_usable_when_they_name_a_title_and_an_artist() {
        assert!(elsewhere("A", "T", 1).is_usable());
        assert!(play("s1", "A", 1).is_usable());
        assert!(!elsewhere(" ", "T", 1).is_usable());
        assert!(!elsewhere("A", " ", 1).is_usable());
        assert!(!play("", "A", 1).is_usable());
    }

    #[test]
    fn how_a_play_ended_is_what_the_source_says_or_the_share_heard() {
        let heard = |listened: u64, track: u64| Play {
            listened_ms: Some(listened),
            track_ms: Some(track),
            ..play("s", "A", 1)
        };
        assert_eq!(heard(10_000, 200_000).ended(), Some(Ended::Skip));
        assert_eq!(heard(100_000, 200_000).ended(), Some(Ended::Heard));
        assert_eq!(heard(190_000, 200_000).ended(), Some(Ended::Full));
        assert_eq!(
            heard(400_000, 200_000).ended(),
            Some(Ended::Full),
            "repeated"
        );
        let said = Play {
            ended: Some(Ended::Skip),
            ..heard(190_000, 200_000)
        };
        assert_eq!(said.ended(), Some(Ended::Skip), "the source's word wins");
        assert_eq!(play("s", "A", 1).ended(), None, "nothing is known");
        assert_eq!(heard(5, 0).ended(), None, "a track of no length");
    }

    #[test]
    fn a_play_without_the_optional_fields_stays_small_and_reads_back() {
        let line = serde_json::to_string(&play("s1", "A", 5)).unwrap();
        assert_eq!(line, r#"{"song":"s1","artists":["A"],"at":5}"#);
        let old: Play = serde_json::from_str(&line).unwrap();
        assert_eq!(old, play("s1", "A", 5));
    }

    #[test]
    fn plays_arriving_late_take_their_place_in_time() {
        let mut log = vec![];
        record(&mut log, play("s2", "B", 20));
        record(&mut log, play("s1", "A", 10));
        assert_eq!(
            log.iter().map(|p| p.song.as_str()).collect::<Vec<_>>(),
            ["s1", "s2"]
        );
    }

    #[test]
    fn merging_adds_what_is_new_and_a_second_merge_adds_nothing() {
        let mut log = vec![play("s1", "A", 100), play("s3", "A", 300)];
        let new = vec![
            play("s3", "A", 302),         // told twice
            elsewhere("B", "Song", 200),  // between the two
            elsewhere("b", "song", 205),  // the same play, told by a second source
            elsewhere("B", "Song", 5000), // the same song, much later
        ];
        let added = merge_new(&mut log, new.clone());
        assert_eq!(added.len(), 2);
        assert_eq!(
            log.iter().map(|p| p.at).collect::<Vec<_>>(),
            [100, 200, 300, 5000],
            "the history stays in time order"
        );
        assert!(merge_new(&mut log, new).is_empty());
    }

    #[test]
    fn a_line_of_an_import_file_becomes_a_play_marked_with_its_source() {
        let play = from_import_line(
            r#"{"at":1700000000,"artist":"Portishead","title":"Glory Box","album":"Dummy",
                "listened_ms":10000,"track_ms":300000}"#,
            "spotify",
        )
        .unwrap();
        assert_eq!(play.artists, ["Portishead"]);
        assert_eq!(play.source, "import:spotify");
        assert_eq!(play.ended(), Some(Ended::Skip));
        let listed = from_import_line(r#"{"at":5,"artists":["A","B"],"song":"s1"}"#, "x").unwrap();
        assert_eq!(listed.artists, ["A", "B"]);
    }

    #[test]
    fn a_line_that_is_not_a_play_says_why() {
        for line in [
            "not json",
            r#"{"artist":"A","title":"T"}"#,
            r#"{"at":1700000000000,"artist":"A","title":"T"}"#,
            r#"{"at":5,"artist":"A"}"#,
            r#"{"at":5,"title":"T"}"#,
        ] {
            assert!(from_import_line(line, "x").is_err(), "{line}");
        }
    }

    #[test]
    fn plays_older_than_the_retention_are_dropped_and_the_rest_kept() {
        let now = 10 * RETENTION;
        let mut log = vec![
            play("old", "A", now - RETENTION - 1),
            play("edge", "A", now - RETENTION),
            play("new", "A", now),
        ];
        prune(&mut log, now, RETENTION);
        assert_eq!(
            log.iter().map(|p| p.song.as_str()).collect::<Vec<_>>(),
            ["edge", "new"]
        );
    }

    #[test]
    fn a_long_history_is_kept_whole() {
        let mut log = vec![];
        for i in 0..2000 {
            record(&mut log, play(&format!("s{i}"), "A", i * 100));
        }
        assert_eq!(log.len(), 2000);
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    const DAY: i64 = 24 * 3600;

    /// `plays` as (artist, day) pairs, played in that order.
    fn days(plays: &[(&str, i64)]) -> Vec<Play> {
        plays
            .iter()
            .enumerate()
            .map(|(i, (artist, day))| play(&format!("s{i}"), artist, day * DAY))
            .collect()
    }

    fn baseline(at_day: i64, tallies: &[(&str, u32)]) -> Baseline {
        Baseline {
            at: at_day * DAY,
            tallies: tallies
                .iter()
                .map(|(artist, count)| Tally {
                    song: format!("{artist}-1"),
                    artist: artist.to_string(),
                    title: "T".into(),
                    count: *count,
                    last_played: None,
                })
                .collect(),
        }
    }

    #[test]
    fn the_artists_played_most_in_the_last_weeks_come_first_and_ties_go_to_the_latest() {
        let log = days(&[
            ("A", 10),
            ("B", 11),
            ("B", 12),
            ("C", 13),
            ("D", 14),
            ("B", 15),
            ("A", 16),
        ]);
        // B three times, A twice, then C and D once each: D was played last.
        assert_eq!(seed_artists(&log, None, &[])[..3], ["B", "A", "D"]);
    }

    #[test]
    fn plays_older_than_the_last_weeks_count_for_all_time_and_not_for_lately() {
        let log = days(&[
            ("Old", 1),
            ("Old", 2),
            ("Old", 3),
            ("Old", 4),
            ("Old", 5),
            ("New", 60),
        ]);
        // New is the only artist lately; Old is the most played of all.
        assert_eq!(seed_artists(&log, None, &[]), ["New", "Old"]);
    }

    #[test]
    fn the_most_played_of_all_follow_and_skip_the_artists_already_taken() {
        let log = days(&[("A", 100), ("B", 101), ("C", 102)]);
        let counted = baseline(0, &[("B", 50), ("Z", 40), ("Y", 30), ("X", 20)]);
        // Lately: C, B, A (one play each, the latest first). B is already there, so Z and Y are the all-time two.
        assert_eq!(
            seed_artists(&log, Some(&counted), &[]),
            ["C", "B", "A", "Z", "Y"]
        );
    }

    #[test]
    fn what_the_server_counted_adds_to_the_plays_told_after_and_not_to_those_before() {
        let log = days(&[
            ("Z", 1),
            ("Z", 2),
            ("Z", 3),
            ("A", 100),
            ("B", 101),
            ("C", 102),
        ]);
        // The baseline was taken on day 50: the three early plays of Z are in its count of Z (1), not on top of it.
        let counted = baseline(50, &[("Z", 1), ("Q", 2)]);
        assert_eq!(
            seed_artists(&log, Some(&counted), &[]),
            ["C", "B", "A", "Q", "Z"],
            "Q (2) is ahead of Z (1), not of Z (4)"
        );
    }

    #[test]
    fn without_a_baseline_the_whole_history_stands_in_for_all_time() {
        let mut plays = vec![("Gone", 1), ("Gone", 2), ("Gone", 3), ("Gone2", 4)];
        plays.extend([("A", 100), ("B", 101), ("C", 102)]);
        let log = days(&plays);
        assert_eq!(
            seed_artists(&log, None, &[]),
            ["C", "B", "A", "Gone", "Gone2"]
        );
    }

    #[test]
    fn the_recent_albums_fill_the_places_the_plays_leave() {
        let log = days(&[("A", 100)]);
        let albums = names(&["a", "B", "C", "D"]);
        // "a" is A again, spelled differently.
        assert_eq!(seed_artists(&log, None, &albums), ["A", "B", "C"]);
        assert_eq!(seed_artists(&[], None, &albums), ["a", "B", "C"]);
    }

    #[test]
    fn spellings_of_one_artist_count_as_one_and_are_named_as_last_spelled() {
        let log = days(&[("Sigur Rós", 100), ("sigur ros", 101), ("B", 102)]);
        assert_eq!(seed_artists(&log, None, &[])[..2], ["sigur ros", "B"]);
    }

    #[test]
    fn no_plays_and_nothing_listed_means_no_seeds() {
        assert!(seed_artists(&[], None, &[]).is_empty());
    }

    #[test]
    fn plays_are_cut_into_sittings_where_the_gap_is_long() {
        let at = |s: &str, t: i64| play(s, "A", t);
        let log = [
            at("s1", 0),
            at("s2", SESSION_GAP), // exactly the gap: still the same sitting
            at("s3", 2 * SESSION_GAP + 1),
            at("s4", 2 * SESSION_GAP + 2),
            at("s5", 10 * SESSION_GAP),
        ];
        let sittings: Vec<Vec<&str>> = sessions(&log)
            .iter()
            .map(|s| s.iter().map(|p| p.song.as_str()).collect())
            .collect();
        assert_eq!(sittings, [vec!["s1", "s2"], vec!["s3", "s4"], vec!["s5"]]);
        assert!(sessions(&[]).is_empty());
    }

    #[test]
    fn the_main_artist_of_a_song_is_the_first_one_named() {
        let log = [Play {
            song: "s1".into(),
            artists: vec![" ".into(), "Lead".into(), "Guest".into()],
            at: 1,
            ..Play::default()
        }];
        assert_eq!(seed_artists(&log, None, &[]), ["Lead"]);
    }
}
