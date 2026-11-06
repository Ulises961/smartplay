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

/// Adds `play` to `log`, keeping it in time order and at most [`KEEP`] long. Returns `false`, changing nothing, when
/// the same song at the same time is already there: a server that could not be sure the engine heard a play sends it again.
pub fn record(log: &mut Vec<Play>, play: Play) -> bool {
    if log.iter().any(|p| p.song == play.song && p.at == play.at) {
        return false;
    }
    log.push(play);
    log.sort_by_key(|p| p.at); // stable: plays at the same second keep the order they arrived in
    if log.len() > KEEP {
        log.drain(..log.len() - KEEP);
    }
    true
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
    fn the_main_artist_of_a_song_is_the_first_one_named() {
        let log = [Play {
            song: "s1".into(),
            artists: vec![" ".into(), "Lead".into(), "Guest".into()],
            at: 1,
        }];
        assert_eq!(recent_artists(&log, 5), ["Lead"]);
    }
}
