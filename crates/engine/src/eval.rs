//! `eval`: what the mixes would be for a user's stored library and history, and how well they would have predicted what the
//! user played next. Nothing here changes any data: it reads what the engine holds and prints.
//!
//! The hold-out check hides the last days of plays, builds the mixes from the plays before, and counts how many of the hidden
//! plays are of a song in a mix or of an artist in one. A mix of random songs would hit by chance, so the hits are set beside
//! what chance gives (the share of the library the mixes hold); a lift of 1 means the mixes are no better than random.

use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use smartplay_core::{
    enrich::Enrichment,
    identity::Decisions,
    mix::{self, Tuning},
    moods::{self, Mood},
    normalize::fold,
    plays::{self, Baseline, Ended, Play},
    taste::Song,
    PLAYLIST_PREFIX,
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
    /// The mixes now, as lines of `artist - title`, and the playlist each is written to.
    pub mixes: Vec<Vec<String>>,
    pub mix_titles: Vec<String>,
    /// The mixes of the moods, in the same form.
    pub mood_mixes: Vec<(String, Vec<String>)>,
    /// What each of the mixes is made of, in the same order.
    pub mix_stats: Vec<MixStats>,
    pub holdout: Option<Holdout>,
    /// The same check made without what outside services say, when something is known of the artists: what they add.
    pub holdout_without_enrichment: Option<Holdout>,
    /// The share of the library that is in the mixes.
    pub coverage: f64,
    /// The share of the songs of the mixes that are of the artist with most songs in them.
    pub top_artist_share: f64,
}

/// How a mix holds together.
#[derive(Debug, Default, PartialEq)]
pub struct MixStats {
    /// How well its artists fit each other (see `mix::mix_fit`); none for a mix of one artist.
    pub fit: Option<f64>,
    /// Its artists with the most songs in it, and how many each has.
    pub top_artists: Vec<(String, usize)>,
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

/// What the mixes are made from.
pub struct Input<'a> {
    pub library: &'a [Song],
    /// The history, in time order.
    pub played: &'a [Play],
    pub baseline: Option<&'a Baseline>,
    /// The artists of the albums the server lists as recent; none when it cannot be asked.
    pub recent: &'a [String],
    pub decisions: &'a Decisions,
    pub enrichment: &'a Enrichment,
    pub tuning: Tuning,
    pub moods: &'a [Mood],
    /// The year it is.
    pub year: u16,
}

/// The mixes for `input`, and the check against the last `holdout_days` of its history.
pub fn evaluate(input: &Input, size: usize, holdout_days: i64) -> Report {
    let Input {
        library,
        played,
        baseline,
        recent,
        decisions,
        enrichment,
        tuning,
        moods,
        year,
    } = *input;
    let index = Index::new(library);
    let name_of: HashMap<&str, String> = library
        .iter()
        .map(|s| (s.id.as_str(), format!("{} - {}", s.artist, s.title)))
        .collect();

    let seeds = plays::seed_artists(played, baseline, recent);
    let explained = mix::recent_artist_mixes_explained(
        library, &seeds, played, decisions, enrichment, size, &tuning,
    );
    let mixes: Vec<Vec<String>> = explained
        .iter()
        .map(|m| m.iter().map(|c| c.id.clone()).collect())
        .collect();

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
        mix_stats: mixes
            .iter()
            .map(|ids| {
                let mut count: HashMap<String, (String, usize)> = HashMap::new();
                for song in ids.iter().filter_map(|id| index.by_id.get(id.as_str())) {
                    count
                        .entry(fold(&song.artist))
                        .or_insert((song.artist.clone(), 0))
                        .1 += 1;
                }
                let mut top_artists: Vec<(String, usize)> = count.into_values().collect();
                top_artists.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                top_artists.truncate(4);
                MixStats {
                    fit: mix::mix_fit(library, ids, played, decisions, enrichment),
                    top_artists,
                }
            })
            .collect(),
        mixes: explained.iter().map(|m| lines(m, &name_of, why)).collect(),
        mix_titles: explained
            .iter()
            .map(|m| format!("{PLAYLIST_PREFIX}{}", mix::mix_title(library, m)))
            .collect(),
        mood_mixes: moods::mood_mixes(
            library,
            moods,
            &moods::Context {
                heard: &plays::artist_counts(played, baseline),
                decisions,
                enrichment,
                year,
            },
            size,
            &tuning,
        )
        .into_iter()
        .map(|(name, chosen)| {
            (
                format!("{PLAYLIST_PREFIX}{name}"),
                lines(&chosen, &name_of, mood_why),
            )
        })
        .collect(),
        holdout: None,
        holdout_without_enrichment: None,
    };

    report.holdout = holdout(input, enrichment, size, holdout_days);
    if !enrichment.is_empty() {
        report.holdout_without_enrichment =
            holdout(input, &Enrichment::default(), size, holdout_days);
    }
    report
}

/// Hides the plays of the last `days` days, builds the mixes from the plays before and counts the hidden plays that land on
/// a song or an artist of the mixes. `None` when there is no history on both sides of the cut.
fn holdout(input: &Input, enrichment: &Enrichment, size: usize, days: i64) -> Option<Holdout> {
    let Input {
        library,
        played,
        baseline,
        decisions,
        ..
    } = *input;
    let index = Index::new(library);
    let latest = played.last()?.at;
    let split = played.partition_point(|p| p.at < latest - days * DAY);
    let (before, hidden) = played.split_at(split);
    if before.is_empty() || hidden.is_empty() {
        return None;
    }
    let seeds = plays::seed_artists(before, baseline, &[]);
    let mixes = mix::recent_artist_mixes_explained(
        library,
        &seeds,
        before,
        decisions,
        enrichment,
        size,
        &input.tuning,
    );
    let mixes: Vec<Vec<String>> = mixes
        .into_iter()
        .map(|m| m.into_iter().map(|c| c.id).collect())
        .collect();
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
    Some(Holdout {
        days,
        tested,
        song_hits,
        artist_hits,
        song_chance: ratio(songs.len(), library.len()),
        artist_chance: ratio(artists.len(), artist_count),
    })
}

/// The lines of a mix: `artist - title   [why]`.
fn lines(
    mix: &[mix::Chosen],
    name_of: &HashMap<&str, String>,
    reason: fn(&mix::Chosen) -> String,
) -> Vec<String> {
    mix.iter()
        .map(|c| {
            let name = name_of.get(c.id.as_str()).cloned();
            format!("{}   [{}]", name.unwrap_or(c.id.clone()), reason(c))
        })
        .collect()
}

/// The reason a song is in a mix, for its line: an artist played lately, or the parts of a similar song's score.
fn why(song: &mix::Chosen) -> String {
    if song.base {
        "played lately".to_string()
    } else {
        format!(
            "tags {:.2} + heard with {:.2} + close {:.2} x 0.5 = {:.2}",
            song.tags,
            song.together,
            song.close,
            song.tags + song.together + 0.5 * song.close
        )
    }
}

/// The reason a song is in a mood's mix: how much of the recipe it has.
fn mood_why(song: &mix::Chosen) -> String {
    format!("fits {:.2}", song.tags)
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
            let title = self.mix_titles.get(i).map_or("mix", String::as_str);
            let _ = writeln!(out, "\n{title} ({} songs)", mix.len());
            if let Some(stats) = self.mix_stats.get(i) {
                let fit = stats
                    .fit
                    .map_or("one artist".to_string(), |f| format!("{f:.2}"));
                let artists: Vec<String> = stats
                    .top_artists
                    .iter()
                    .map(|(name, n)| format!("{name} {n} ({:.0}%)", 100.0 * ratio(*n, mix.len())))
                    .collect();
                let _ = writeln!(
                    out,
                    "  fit of its artists: {fit}; most songs: {}",
                    artists.join(", ")
                );
            }
            for song in mix {
                let _ = writeln!(out, "  {song}");
            }
        }
        for (title, mix) in &self.mood_mixes {
            let _ = writeln!(out, "\n{title} ({} songs)", mix.len());
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
        if let (Some(with), Some(without)) = (&self.holdout, &self.holdout_without_enrichment) {
            let _ = writeln!(
                out,
                "without what Last.fm says: songs lift {:.1} (with it {:.1}), artists lift {:.1} (with it {:.1})",
                without.song_lift(),
                with.song_lift(),
                without.artist_lift(),
                with.artist_lift()
            );
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

    /// The report for a library and a history, with nothing else known.
    fn report(library: &[Song], played: &[Play]) -> Report {
        evaluate(
            &Input {
                library,
                played,
                baseline: None,
                recent: &[],
                decisions: &Decisions::default(),
                enrichment: &Enrichment::default(),
                tuning: Tuning::default(),
                moods: &moods::defaults(),
                year: 2026,
            },
            10,
            14,
        )
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
        let report = report(&library, &played);
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
    fn each_mix_says_how_well_its_artists_fit_and_who_has_most_songs() {
        let library = library();
        let mut played = vec![];
        for (day, artist) in [(0, "J1"), (1, "R1"), (2, "R2"), (3, "J2")] {
            let first = library.iter().find(|s| s.artist == artist).unwrap();
            played.push(play(first, day * DAY));
        }
        let report = report(&library, &played);
        assert_eq!(report.mixes.len(), 2, "rock and jazz are told apart");
        assert_eq!(report.mix_stats.len(), 2);
        for stats in &report.mix_stats {
            assert!(stats.fit.is_some_and(|f| f > 0.9), "{stats:?}");
            assert_eq!(stats.top_artists.iter().map(|a| a.1).sum::<usize>(), 10);
        }
        assert!(report.render().contains("fit of its artists: 1.00"));
    }

    #[test]
    fn with_no_history_there_is_nothing_to_predict_and_the_report_says_so() {
        let report = report(&library(), &[]);
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
        let report = report(&library, &[play(&library[0], 50), stranger]);
        assert_eq!((report.plays, report.matched_plays), (2, 1));
    }

    #[test]
    fn the_library_is_described_by_its_tags_and_years() {
        let mut library = library();
        library[0].year = Some(1994);
        library[1].genres.clear();
        let report = report(&library, &[]);
        assert_eq!(
            (report.songs, report.with_tags, report.with_year),
            (40, 39, 1)
        );
    }
}
