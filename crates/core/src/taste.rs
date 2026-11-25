//! "More of what you star": a playlist of songs the user has not starred, from the artists and genres they star most.
//!
//! Pure functions over a snapshot of the library. Artists are counted by identity, not by spelling, so a band the
//! library names three ways counts once.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::identity::{resolve, Decisions, Track};
use crate::normalize::fold;

/// One song of the library.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Song {
    pub id: String,
    pub artist: String,
    pub album: String,
    pub title: String,
    /// The first genre, as older copies of a library hold it.
    #[serde(default)]
    pub genre: Option<String>,
    #[serde(default)]
    pub artist_mbid: Option<String>,
    /// Every genre tag of the song (the server splits `Rock;Indie` in two). Empty in copies uploaded before it was read.
    #[serde(default)]
    pub genres: Vec<String>,
    /// Mood tags of the song. Empty when the files carry none.
    #[serde(default)]
    pub moods: Vec<String>,
    /// The year the files carry. Often the year of a reissue, not of the first release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u16>,
    /// How long the song is, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u32>,
    /// Beats per minute, when the files carry it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bpm: Option<u16>,
    /// When the song was added to the library, as the server says it (an ISO 8601 date and time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// How many times the server counts the song as played, and when it last was (an ISO 8601 date and time). These
    /// change with every play and are not part of what makes a library "changed": only the first copy of a library the
    /// engine receives is read for them (see `plays::Baseline`), and they are not kept in the copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub play_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_played: Option<String>,
}

/// An artist weighs twice a genre: a star says more about who made the song than about its label.
const ARTIST_WEIGHT: usize = 2;

/// Up to `size` ids of unstarred songs, best first.
///
/// A song scores `2 * stars on its artist + stars on its genre`; songs that score nothing are left out, so a user
/// with no stars gets an empty list. Each pick lowers the next song of the same artist (its score is divided by
/// one plus the songs already picked from that artist), so the list mixes artists instead of playing one out.
/// The result depends only on the library and the stars, not on the order of `library`.
pub fn more_of_what_you_star(
    library: &[Song],
    starred: &[String],
    decisions: &Decisions,
    size: usize,
) -> Vec<String> {
    let starred: HashSet<&str> = starred.iter().map(String::as_str).collect();
    if starred.is_empty() || size == 0 {
        return vec![];
    }

    let tracks: Vec<Track> = library
        .iter()
        .map(|s| Track {
            artist: s.artist.clone(),
            album: s.album.clone(),
            title: s.title.clone(),
            artist_mbid: s.artist_mbid.clone(),
        })
        .collect();
    let mut artist_of_spelling: HashMap<String, usize> = HashMap::new();
    for (i, cluster) in resolve(&tracks, decisions).clusters.iter().enumerate() {
        for spelling in &cluster.spellings {
            artist_of_spelling.insert(spelling.clone(), i);
        }
    }
    let artist = |s: &Song| artist_of_spelling.get(s.artist.trim()).copied();
    let genre = |s: &Song| s.genre.as_deref().map(fold).filter(|g| !g.is_empty());

    let mut artist_stars: HashMap<usize, usize> = HashMap::new();
    let mut genre_stars: HashMap<String, usize> = HashMap::new();
    for song in library.iter().filter(|s| starred.contains(s.id.as_str())) {
        if let Some(a) = artist(song) {
            *artist_stars.entry(a).or_default() += 1;
        }
        if let Some(g) = genre(song) {
            *genre_stars.entry(g).or_default() += 1;
        }
    }

    struct Candidate<'a> {
        id: &'a str,
        artist: Option<usize>,
        score: usize,
    }
    let mut seen = HashSet::new();
    let mut candidates: Vec<Candidate> = library
        .iter()
        .filter(|s| !starred.contains(s.id.as_str()) && seen.insert(s.id.as_str()))
        .filter_map(|s| {
            let a = artist(s);
            let score = ARTIST_WEIGHT * a.and_then(|a| artist_stars.get(&a)).copied().unwrap_or(0)
                + genre(s)
                    .and_then(|g| genre_stars.get(&g))
                    .copied()
                    .unwrap_or(0);
            (score > 0).then_some(Candidate {
                id: &s.id,
                artist: a,
                score,
            })
        })
        .collect();

    let mut picked_from: HashMap<usize, usize> = HashMap::new();
    let mut playlist = vec![];
    while playlist.len() < size && !candidates.is_empty() {
        let weight = |c: &Candidate| {
            let taken = c
                .artist
                .map_or(0, |a| picked_from.get(&a).copied().unwrap_or(0));
            c.score as f64 / (1 + taken) as f64
        };
        let Some(best) = (0..candidates.len()).max_by(|&x, &y| {
            weight(&candidates[x])
                .total_cmp(&weight(&candidates[y]))
                .then_with(|| candidates[y].id.cmp(candidates[x].id))
        }) else {
            break; // the loop only runs while there are candidates, so this is not reached
        };
        let chosen = candidates.swap_remove(best);
        if let Some(a) = chosen.artist {
            *picked_from.entry(a).or_default() += 1;
        }
        playlist.push(chosen.id.to_string());
    }
    playlist
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str, artist: &str, album: &str, genre: &str) -> Song {
        Song {
            id: id.into(),
            artist: artist.into(),
            album: album.into(),
            title: format!("Song {id}"),
            genre: (!genre.is_empty()).then(|| genre.into()),
            ..Default::default()
        }
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn library() -> Vec<Song> {
        vec![
            song("q1", "Queen", "A Night at the Opera", "Rock"),
            song("q2", "Queen", "A Night at the Opera", "Rock"),
            song("q3", "Queen", "The Game", "Rock"),
            song("p1", "Portishead", "Dummy", "Trip Hop"),
            song("p2", "Portishead", "Dummy", "Trip Hop"),
            song("a1", "ABBA", "Arrival", "Pop"),
        ]
    }

    #[test]
    fn no_stars_no_playlist() {
        assert!(more_of_what_you_star(&library(), &[], &Decisions::default(), 10).is_empty());
    }

    #[test]
    fn starred_songs_are_never_suggested_and_unrelated_ones_are_left_out() {
        let list = more_of_what_you_star(&library(), &ids(&["q1"]), &Decisions::default(), 10);
        assert_eq!(list, ids(&["q2", "q3"]));
    }

    #[test]
    fn the_artist_counts_for_more_than_the_genre() {
        let library = vec![
            song("s1", "Queen", "x", "Rock"),
            song("same-genre", "Kiss", "y", "Rock"),
            song("same-artist", "Queen", "x", "Pop"),
        ];
        let list = more_of_what_you_star(&library, &ids(&["s1"]), &Decisions::default(), 10);
        assert_eq!(list, ids(&["same-artist", "same-genre"]));
    }

    #[test]
    fn genre_matches_ignore_case_and_accents() {
        let library = vec![
            song("s1", "Queen", "x", "Rock"),
            song("other", "Kiss", "y", "ROCK "),
        ];
        let list = more_of_what_you_star(&library, &ids(&["s1"]), &Decisions::default(), 10);
        assert_eq!(list, ids(&["other"]));
    }

    #[test]
    fn spellings_of_one_artist_count_as_one() {
        // The library holds the band under two names; they share an album, so they are one artist.
        let library = vec![
            song(
                "s1",
                "Patricio Rey y sus Redonditos de Ricota",
                "Oktubre",
                "",
            ),
            song(
                "s2",
                "Patricio Rey y sus Redonditos de Ricota",
                "Oktubre",
                "",
            ),
            song("v1", "Los Redonditos de Ricota", "Oktubre", ""),
            song("x1", "Queen", "The Game", ""),
        ];
        let list = more_of_what_you_star(&library, &ids(&["s1", "s2"]), &Decisions::default(), 10);
        assert_eq!(list, ids(&["v1"]));
    }

    #[test]
    fn a_decision_joins_spellings_the_library_cannot_join() {
        let library = vec![
            song(
                "s1",
                "Patricio Rey y sus Redonditos de Ricota",
                "Oktubre",
                "",
            ),
            song("v1", "Los Redondos", "Ultimo bondi a Finisterre", ""),
        ];
        let stars = ids(&["s1"]);
        assert!(more_of_what_you_star(&library, &stars, &Decisions::default(), 10).is_empty());
        let mut decisions = Decisions::default();
        decisions.same("Los Redondos", "Patricio Rey y sus Redonditos de Ricota");
        assert_eq!(
            more_of_what_you_star(&library, &stars, &decisions, 10),
            ids(&["v1"])
        );
    }

    #[test]
    fn the_list_mixes_artists_instead_of_playing_one_out() {
        let mut library = vec![song("q0", "Queen", "x", "")];
        library.push(song("p0", "Portishead", "y", ""));
        for i in 1..=6 {
            library.push(song(&format!("q{i}"), "Queen", "x", ""));
            library.push(song(&format!("p{i}"), "Portishead", "y", ""));
        }
        let list = more_of_what_you_star(&library, &ids(&["q0", "p0"]), &Decisions::default(), 4);
        let queen = list.iter().filter(|id| id.starts_with('q')).count();
        assert_eq!((list.len(), queen), (4, 2), "{list:?}");
    }

    #[test]
    fn the_list_never_exceeds_the_size_and_repeats_no_song() {
        let list = more_of_what_you_star(&library(), &ids(&["q1"]), &Decisions::default(), 1);
        assert_eq!(list.len(), 1);
        let mut library = library();
        library.push(song("q2", "Queen", "A Night at the Opera", "Rock"));
        let list = more_of_what_you_star(&library, &ids(&["q1"]), &Decisions::default(), 10);
        assert_eq!(list, ids(&["q2", "q3"]));
    }

    #[test]
    fn the_order_of_the_library_does_not_change_the_result() {
        let forward = library();
        let mut backward = library();
        backward.reverse();
        let stars = ids(&["q1", "p1"]);
        assert_eq!(
            more_of_what_you_star(&forward, &stars, &Decisions::default(), 10),
            more_of_what_you_star(&backward, &stars, &Decisions::default(), 10)
        );
    }

    #[test]
    fn stars_on_songs_missing_from_the_library_are_ignored() {
        let list = more_of_what_you_star(&library(), &ids(&["gone"]), &Decisions::default(), 10);
        assert!(list.is_empty());
    }
}
