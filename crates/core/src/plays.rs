//! The plays the engine has been told about, newest last, and what a mix takes from them.
//!
//! Only a short tail is kept per user ([`KEEP`] plays): enough to know what was played lately, not a history of the
//! user's listening.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::normalize::fold;

/// Plays kept per user; recording another drops the oldest.
pub const KEEP: usize = 500;

/// One completed play.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Play {
    /// The song's id on the server.
    pub song: String,
    /// The artists credited on the song, the main one first.
    pub artists: Vec<String>,
    /// When the song started playing, in seconds since the epoch.
    pub at: i64,
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
        }
    }
}

/// Adds `play` to `log`, keeping it in time order and at most [`KEEP`] long. Returns `false`, changing nothing, when
/// the same song at the same time is already there: a server that could not be sure the engine heard a play sends it again.
pub fn record(log: &mut Vec<Play>, play: Play) -> bool {
    if log.iter().any(|p| p.song == play.song && p.at == play.at) {
        return false;
    }
    let (song, at) = (play.song.clone(), play.at);
    log.push(play);
    log.sort_by_key(|p| p.at); // stable: plays at the same second keep the order they arrived in
    if log.len() > KEEP {
        log.drain(..log.len() - KEEP);
    }
    // A play older than all of a full log's plays is dropped at once: it was not recorded.
    log.iter().any(|p| p.song == song && p.at == at)
}

/// The main artist of each play, most recent first, each once (spellings that differ only in case, accents or a joiner
/// count as one), at most `limit`.
pub fn recent_artists(log: &[Play], limit: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    log.iter()
        .rev()
        .filter_map(|p| p.artists.iter().map(|a| a.trim()).find(|a| !a.is_empty()))
        .filter(|a| seen.insert(fold(a)))
        .take(limit)
        .map(String::from)
        .collect()
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
            record(&mut log, play("s1", "A", 11)),
            "the same song again later is a new play"
        );
        assert_eq!(log.len(), 2);
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
    fn only_the_latest_plays_are_kept() {
        let mut log = vec![];
        for i in 0..(KEEP as i64 + 3) {
            record(&mut log, play(&format!("s{i}"), "A", i));
        }
        assert_eq!(log.len(), KEEP);
        assert_eq!(log[0].song, "s3", "the oldest went first");
    }

    #[test]
    fn a_play_older_than_a_full_log_is_not_recorded() {
        let mut log = vec![];
        for i in 0..KEEP as i64 {
            record(&mut log, play(&format!("s{i}"), "A", 100 + i));
        }
        assert!(!record(&mut log, play("old", "A", 1)));
        assert_eq!((log.len(), log[0].at), (KEEP, 100));
        assert!(record(&mut log, play("new", "A", 1000)), "a newer one is");
        assert_eq!((log.len(), log[0].at), (KEEP, 101));
    }

    #[test]
    fn recent_artists_come_newest_first_once_each() {
        let log = [
            play("s1", "Band A", 1),
            play("s2", "Band B", 2),
            play("s3", "band  a", 3),
            play("s4", "Band C", 4),
        ];
        assert_eq!(
            recent_artists(&log, 10),
            ["Band C", "band  a", "Band B"],
            "Band A is there once, as it was last spelled"
        );
        assert_eq!(recent_artists(&log, 2), ["Band C", "band  a"]);
        assert!(recent_artists(&[], 5).is_empty());
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
        }];
        assert_eq!(recent_artists(&log, 5), ["Lead"]);
    }
}
