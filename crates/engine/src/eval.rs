//! `eval`: what the mixes would be for a user's stored library and history, and how well they would have predicted what the
//! user played next. Nothing here changes any data: it reads what the engine holds and prints.
//!
//! The hold-out check hides the last days of plays, builds the mixes from the plays before, and counts how many of the hidden
//! plays are of a song in a mix or of an artist in one. A mix of random songs would hit by chance, so the hits are set beside
//! what chance gives (the share of the library the mixes hold); a lift of 1 means the mixes are no better than random.

use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use smartplay_core::{
    identity::Decisions,
    mix,
    normalize::fold,
    plays::{self, Baseline, Ended, Play},
    taste::Song,
};

const DAY: i64 = 24 * 60 * 60;

/// What an evaluation found.
#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub songs: usize,
    pub with_tags: usize,
    pub with_year: usize,
    pub plays: usize,
    /// Plays that are of a song of the library (by its id, or by its artist and title).
    pub matched_plays: usize,
    pub skips: usize,
    pub seeds: Vec<String>,
    /// The mixes now, as lines of `artist - title`.
    pub mixes: Vec<Vec<String>>,
    pub holdout: Option<Holdout>,
    /// The share of the library that is in the mixes.
    pub coverage: f64,
    /// The share of the songs of the mixes that are of the artist with most songs in them.
    pub top_artist_share: f64,
}

/// The hold-out check.
#[derive(Debug, PartialEq)]
pub struct Holdout {
    pub days: i64,
    /// Plays of the hidden days that are of a song of the library.
    pub tested: usize,
    pub song_hits: usize,
    pub artist_hits: usize,
    /// How many songs and artists the mixes built without the hidden plays hold, as a share of the library, which is what
    /// random mixes would hit.
    pub song_chance: f64,
    pub artist_chance: f64,
}

impl Holdout {
    pub fn song_lift(&self) -> f64 {
        lift(self.song_hits, self.tested, self.song_chance)
    }
    pub fn artist_lift(&self) -> f64 {
        lift(self.artist_hits, self.tested, self.artist_chance)
    }
}

fn lift(hits: usize, tested: usize, chance: f64) -> f64 {
    if tested == 0 || chance == 0.0 {
        return 0.0;
    }
    (hits as f64 / tested as f64) / chance
}

/// Songs of the library by id, and by artist and title, to tell which plays are of songs the library holds.
struct Index<'a> {
    by_id: HashMap<&'a str, &'a Song>,
    by_name: HashMap<(String, String), &'a Song>,
}

impl<'a> Index<'a> {
    fn new(library: &'a [Song]) -> Index<'a> {
        let mut by_id = HashMap::new();
        let mut by_name = HashMap::new();
        for song in library {
            by_id.insert(song.id.as_str(), song);
            by_name
                .entry((fold(&song.artist), fold(&song.title)))
                .or_insert(song);
        }
        Index { by_id, by_name }
    }

    fn find(&self, play: &Play) -> Option<&'a Song> {
        if let Some(song) = self.by_id.get(play.song.as_str()) {
            return Some(song);
        }
        let artist = play
            .artists
            .iter()
            .map(|a| a.trim())
            .find(|a| !a.is_empty())?;
        self.by_name
            .get(&(fold(artist), fold(&play.title)))
            .copied()
    }
}

/// The mixes for `library` and the history `played` (in time order), and the check against the last `holdout_days` of it.
pub fn evaluate(
    library: &[Song],
    played: &[Play],
    baseline: Option<&Baseline>,
    recent: &[String],
    decisions: &Decisions,
    size: usize,
    holdout_days: i64,
) -> Report {
    let index = Index::new(library);
    let name_of: HashMap<&str, String> = library
        .iter()
        .map(|s| (s.id.as_str(), format!("{} - {}", s.artist, s.title)))
        .collect();

    let seeds = plays::seed_artists(played, baseline, recent);
    let mixes = mix::recent_artist_mixes(library, &seeds, played, decisions, size);

    let in_mixes: HashSet<&str> = mixes.iter().flatten().map(String::as_str).collect();
    let mut per_artist: HashMap<String, usize> = HashMap::new();
    for id in &in_mixes {
        if let Some(song) = index.by_id.get(id) {
            *per_artist.entry(fold(&song.artist)).or_default() += 1;
        }
    }

    let mut report = Report {
        songs: library.len(),
        with_tags: library
            .iter()
            .filter(|s| !s.genres.is_empty() || s.genre.is_some() || !s.moods.is_empty())
            .count(),
        with_year: library.iter().filter(|s| s.year.is_some()).count(),
        plays: played.len(),
        matched_plays: played.iter().filter(|p| index.find(p).is_some()).count(),
        skips: played
            .iter()
            .filter(|p| p.ended() == Some(Ended::Skip))
            .count(),
        coverage: ratio(in_mixes.len(), library.len()),
        top_artist_share: ratio(
            per_artist.values().copied().max().unwrap_or(0),
            in_mixes.len(),
        ),
        seeds,
        mixes: mixes
            .iter()
            .map(|m| {
                m.iter()
                    .map(|id| name_of.get(id.as_str()).cloned().unwrap_or(id.clone()))
                    .collect()
            })
            .collect(),
        holdout: None,
    };

    if let Some(latest) = played.last().map(|p| p.at) {
        let cutoff = latest - holdout_days * DAY;
        let split = played.partition_point(|p| p.at < cutoff);
        let (before, hidden) = played.split_at(split);
        if !before.is_empty() && !hidden.is_empty() {
            let seeds = plays::seed_artists(before, baseline, &[]);
            let mixes = mix::recent_artist_mixes(library, &seeds, before, decisions, size);
            let songs: HashSet<&str> = mixes.iter().flatten().map(String::as_str).collect();
            let artists: HashSet<String> = songs
                .iter()
                .filter_map(|id| index.by_id.get(id))
                .map(|s| fold(&s.artist))
                .collect();
            let artist_count = library
                .iter()
                .map(|s| fold(&s.artist))
                .collect::<HashSet<_>>()
                .len();
            let (mut tested, mut song_hits, mut artist_hits) = (0, 0, 0);
            for song in hidden.iter().filter_map(|p| index.find(p)) {
                tested += 1;
                song_hits += usize::from(songs.contains(song.id.as_str()));
                artist_hits += usize::from(artists.contains(&fold(&song.artist)));
            }
            report.holdout = Some(Holdout {
                days: holdout_days,
                tested,
                song_hits,
                artist_hits,
                song_chance: ratio(songs.len(), library.len()),
                artist_chance: ratio(artists.len(), artist_count),
            });
        }
    }
    report
}

fn ratio(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

impl Report {
    pub fn render(&self) -> String {
        let mut out = String::new();
        let pct = |part: usize, whole: usize| format!("{:.0}%", 100.0 * ratio(part, whole));
        let _ = writeln!(
            out,
            "library: {} songs; {} have tags, {} have a year",
            self.songs,
            pct(self.with_tags, self.songs),
            pct(self.with_year, self.songs)
        );
        let _ = writeln!(
            out,
            "history: {} plays, {} of songs the library holds; {} skips",
            self.plays,
            pct(self.matched_plays, self.plays),
            self.skips
        );
        let _ = writeln!(out, "seeds: {}", self.seeds.join(", "));
        for (i, mix) in self.mixes.iter().enumerate() {
            let _ = writeln!(out, "\nmix {} ({} songs)", i + 1, mix.len());
            for song in mix {
                let _ = writeln!(out, "  {song}");
            }
        }
        let _ = writeln!(
            out,
            "\ncoverage: {:.1}% of the library is in the mixes; the artist with most has {:.0}% of their songs",
            100.0 * self.coverage,
            100.0 * self.top_artist_share
        );
        match &self.holdout {
            Some(h) => {
                let _ = writeln!(
                    out,
                    "hold-out (the last {} days hidden, {} plays of songs the library holds):\n  \
                     songs:   {} hit, {:.1}% against {:.1}% by chance, lift {:.1}\n  \
                     artists: {} hit, {:.1}% against {:.1}% by chance, lift {:.1}",
                    h.days,
                    h.tested,
                    h.song_hits,
                    100.0 * ratio(h.song_hits, h.tested),
                    100.0 * h.song_chance,
                    h.song_lift(),
                    h.artist_hits,
                    100.0 * ratio(h.artist_hits, h.tested),
                    100.0 * h.artist_chance,
                    h.artist_lift()
                );
            }
            None => {
                let _ = writeln!(out, "hold-out: not enough history on both sides of the cut");
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str, artist: &str, genre: &str) -> Song {
        Song {
            id: id.into(),
            artist: artist.into(),
            album: format!("Album of {artist}"),
            title: format!("Title {id}"),
            genres: vec![genre.into()],
            ..Song::default()
        }
    }

    fn play(song: &Song, at: i64) -> Play {
        Play {
            song: song.id.clone(),
            artists: vec![song.artist.clone()],
            at,
            ..Play::default()
        }
    }

    /// Twenty songs of two rock artists and twenty of two jazz artists.
    fn library() -> Vec<Song> {
        let mut songs = vec![];
        for (artist, genre) in [
            ("R1", "Rock"),
            ("R2", "Rock"),
            ("J1", "Jazz"),
            ("J2", "Jazz"),
        ] {
            for i in 0..10 {
                songs.push(song(&format!("{artist}-{i}"), artist, genre));
            }
        }
        songs
    }

    #[test]
    fn a_user_who_keeps_to_one_style_is_predicted_better_than_chance() {
        let library = library();
        // Rock all the time: R1 and R2 for 30 days, then (hidden) more of the same.
        let mut played = vec![];
        for day in 0..40 {
            let artist = if day % 2 == 0 { "R1" } else { "R2" };
            played.push(play(
                library.iter().find(|s| s.artist == artist).unwrap(),
                day * DAY,
            ));
        }
        let report = evaluate(&library, &played, None, &[], &Decisions::default(), 10, 14);
        let h = report
            .holdout
            .as_ref()
            .expect("history on both sides of the cut");
        // The latest play is on day 39, so the days from 25 on (fifteen plays) are hidden.
        assert_eq!(h.tested, 15);
        assert_eq!(h.artist_hits, 15);
        assert!(h.artist_lift() > 1.5, "{h:?}");
        assert!(report.render().contains("lift"));
    }

    #[test]
    fn with_no_history_there_is_nothing_to_predict_and_the_report_says_so() {
        let report = evaluate(&library(), &[], None, &[], &Decisions::default(), 10, 14);
        assert!(report.holdout.is_none());
        assert!(report.mixes.is_empty());
        assert!(report.render().contains("not enough history"));
    }

    #[test]
    fn plays_of_songs_the_library_does_not_hold_are_counted_apart() {
        let library = library();
        let mut stranger = play(&library[0], 100);
        stranger.song = String::new();
        stranger.artists = vec!["Nobody".into()];
        stranger.title = "Unknown".into();
        let report = evaluate(
            &library,
            &[play(&library[0], 50), stranger],
            None,
            &[],
            &Decisions::default(),
            10,
            14,
        );
        assert_eq!((report.plays, report.matched_plays), (2, 1));
    }

    #[test]
    fn the_library_is_described_by_its_tags_and_years() {
        let mut library = library();
        library[0].year = Some(1994);
        library[1].genres.clear();
        let report = evaluate(&library, &[], None, &[], &Decisions::default(), 10, 14);
        assert_eq!(
            (report.songs, report.with_tags, report.with_year),
            (40, 39, 1)
        );
    }
}
