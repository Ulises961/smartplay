//! Mixes named after a mood: `Focus`, `Classical`, `Hits`.
//!
//! A mood is a recipe: the tags that make a song belong to it, each with a weight, the tags that rule a song out, and
//! optionally a window of years and a band of beats per minute. A song's fit is the sum of the weights of the recipe's tags
//! it has (at most 1). The tags are the ones the mixes already use: the genres and moods of the song and the crowd tags of
//! its artist (see `enrich`), so a library with more tags, or one enriched by Last.fm, fits more songs.
//!
//! The title is the contract: a mix is made of the songs that fit the recipe and of nothing else, so there is no similar
//! part as in the mixes of artists. Within the songs that fit, the ones of artists the user plays more come first, and no
//! artist takes more than its share of the mix (the same picking as the mixes of artists).
//!
//! The recipes below are generic. The engine can read others from a file (see [`parse`]), because the tags of a library
//! are its own.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::enrich::Enrichment;
use crate::identity::Decisions;
use crate::mix::{pick, song_key, Basis, Candidate, Chosen, Tuning, MIN_MIX_SONGS};
use crate::normalize::{fold, work_title};
use crate::taste::Song;

/// What the artists a user plays more add to the fit of their songs, at most: the fit says what belongs in the mood, this
/// only puts the familiar ones first among the songs that do.
const FAMILIARITY_WEIGHT: f64 = 0.3;

/// One mood.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Mood {
    /// The title: the playlist is `smartplay: NAME`.
    pub name: String,
    /// Tags that make a song fit, each with how much (above 0, at most 1 is enough on its own). A tag matches a song's tag
    /// when it is the same or one of its words (`classical` matches `modern classical`), ignoring case and accents.
    pub tags: BTreeMap<String, f64>,
    /// A song with any of these tags is never in the mood.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub avoid: Vec<String>,
    /// A song fits when its fit is at least this much.
    #[serde(default = "default_min")]
    pub min: f64,
    /// Only songs from this many years back, counting the current year as one: `2` is this year and last year. A song with
    /// no year is left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub years_back: Option<u16>,
    /// Only songs whose beats per minute are within these (both ends included). A song that carries no BPM is not left
    /// out for it: most libraries carry none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bpm: Option<(u16, u16)>,
    /// Only songs that Last.fm says are played a lot: how many times the song was played there, as a place among the songs
    /// of the library that it is counted for (0 is the least played, 1 the most), must be at least this. `0.8` is the most
    /// played fifth. It also puts the more played first. A song it has no count for (an artist not asked about yet, or a
    /// song not among the artist's most played) is left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_popularity: Option<f64>,
}

fn default_min() -> f64 {
    0.5
}

fn mood(name: &str, tags: &[(&str, f64)], avoid: &[&str]) -> Mood {
    Mood {
        name: name.into(),
        tags: tags.iter().map(|(t, w)| (t.to_string(), *w)).collect(),
        avoid: avoid.iter().map(|t| t.to_string()).collect(),
        min: default_min(),
        years_back: None,
        bpm: None,
        min_popularity: None,
    }
}

/// The recipes the engine uses when it is given none. They go by genres (and the crowd tags of Last.fm when the engine has
/// them), because genres are what most libraries carry.
pub fn defaults() -> Vec<Mood> {
    vec![
        Mood {
            // More than a bare `pop` (0.5): that one takes in the ballads of a pop artist.
            min: 0.6,
            bpm: Some((115, 220)),
            ..mood(
                "Active",
                &[
                    ("workout", 1.0),
                    ("dance pop", 1.0),
                    ("dance", 0.8),
                    ("upbeat", 0.8),
                    ("party", 0.8),
                    ("disco", 0.7),
                    ("funk", 0.6),
                    ("electronic", 0.5),
                    ("electropop", 0.7),
                    ("synthpop", 0.6),
                    ("pop", 0.5),
                    ("ska", 0.5),
                    ("reggaeton", 0.6),
                ],
                &[
                    "classical",
                    "clasica",
                    "ambient",
                    "ballad",
                    "acoustic",
                    "singer songwriter",
                    "tango",
                    "bolero",
                    "soundtrack",
                    "instrumental",
                ],
            )
        },
        mood(
            "Classical",
            &[
                ("classical", 1.0),
                ("clasica", 1.0),
                ("baroque", 1.0),
                ("orchestral", 0.7),
                ("symphony", 0.7),
                ("chamber", 0.7),
                ("opera", 0.6),
                ("composer", 0.6),
                ("piano", 0.3),
            ],
            &["pop", "rock", "metal", "hip hop", "rap", "reggaeton"],
        ),
        Mood {
            bpm: Some((40, 100)),
            ..mood(
                "Focus",
                &[
                    ("ambient", 1.0),
                    ("lo fi", 1.0),
                    ("classical", 0.8),
                    ("clasica", 0.8),
                    ("new age", 0.8),
                    ("instrumental", 0.8),
                    ("post rock", 0.6),
                    ("piano", 0.5),
                    ("chillout", 0.6),
                    ("soundtrack", 0.5),
                ],
                &[
                    "metal",
                    "punk",
                    "hip hop",
                    "rap",
                    "dance",
                    "reggaeton",
                    "party",
                    "workout",
                ],
            )
        },
        Mood {
            // The files' years are of no use for this: most carry none, and the rest are often the year of a reissue.
            min_popularity: Some(0.8),
            ..mood("Hits", &[("pop", 1.0)], &["classical", "clasica"])
        },
    ]
}

/// Recipes from JSON: a list of [`Mood`]s. A recipe with no name or no tags, a weight that is not above 0, a minimum that
/// is not above 0 and at most 1, a band that is not low to high, or a name another recipe has, is an error that names it,
/// so that a typo is not a mood that quietly never fits.
pub fn parse(json: &str) -> Result<Vec<Mood>, String> {
    let moods: Vec<Mood> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let mut names = std::collections::BTreeSet::new();
    for m in &moods {
        let name = m.name.trim();
        if name.is_empty() {
            return Err("a mood has no name".into());
        }
        if !names.insert(fold(name)) {
            return Err(format!("two moods are called '{name}'"));
        }
        if m.tags.is_empty() {
            return Err(format!("the mood '{name}' has no tags"));
        }
        if let Some((tag, _)) = m
            .tags
            .iter()
            .find(|(t, w)| fold(t).is_empty() || !w.is_finite() || **w <= 0.0)
        {
            return Err(format!(
                "the mood '{name}' has the tag '{tag}' with no text or a weight that is not above 0"
            ));
        }
        if !(m.min > 0.0 && m.min <= 1.0) {
            return Err(format!("the mood '{name}' has a minimum outside (0, 1]"));
        }
        if m.bpm.is_some_and(|(low, high)| low >= high) {
            return Err(format!(
                "the mood '{name}' has a band that is not low to high"
            ));
        }
        if m.min_popularity.is_some_and(|p| !(p > 0.0 && p <= 1.0)) {
            return Err(format!(
                "the mood '{name}' has a min_popularity outside (0, 1]"
            ));
        }
        if m.years_back == Some(0) {
            return Err(format!("the mood '{name}' has years_back 0"));
        }
    }
    Ok(moods)
}

/// Whether the tag of a song (folded, with the `g:`, `m:` or `l:` that says where it came from) is `wanted` (folded) or has
/// it as whole words.
fn matches(tag: &str, wanted: &str) -> bool {
    let tag = tag.split_once(':').map_or(tag, |(_, t)| t);
    format!(" {tag} ").contains(&format!(" {wanted} "))
}

/// How much each song of the library is played on Last.fm, as a place between 0 and 1 among the songs that have a count
/// (see `Mood::min_popularity`). A song's count is the one of the title in the most played tracks of its artist.
struct Popularity {
    place: Vec<Option<f64>>,
}

impl Popularity {
    fn new(library: &[Song], enrichment: &Enrichment) -> Popularity {
        let counts: Vec<Option<u64>> = library
            .iter()
            .map(|song| {
                let top = enrichment.get(&song.artist)?.top.as_ref()?;
                let title = work_title(&song.title);
                top.iter().find(|(t, _)| *t == title).map(|(_, n)| *n)
            })
            .collect();
        let mut known: Vec<u64> = counts.iter().flatten().copied().collect();
        known.sort_unstable();
        let span = known.len().saturating_sub(1).max(1) as f64;
        Popularity {
            place: counts
                .into_iter()
                .map(|n| n.map(|n| known.partition_point(|&k| k < n) as f64 / span))
                .collect(),
        }
    }
}

/// What the moods are made with besides the library.
pub struct Context<'a> {
    /// How many times each artist was played (see `plays::artist_counts`).
    pub heard: &'a BTreeMap<String, usize>,
    pub decisions: &'a Decisions,
    pub enrichment: &'a Enrichment,
    /// The current year.
    pub year: u16,
}

/// A mood's mix: the songs that fit it, best first, as [`Chosen`] with the fit in `tags`.
///
/// A mix with fewer songs than a mix of artists may have (`MIN_MIX_SONGS`) is left out: it would be a playlist of a few, for a
/// title that promises more.
pub fn mood_mixes(
    library: &[Song],
    moods: &[Mood],
    context: &Context,
    size: usize,
    tuning: &Tuning,
) -> Vec<(String, Vec<Chosen>)> {
    let Context {
        heard,
        decisions,
        enrichment,
        year,
    } = *context;
    if size == 0 || library.is_empty() {
        return vec![];
    }
    let basis = Basis::new(library, &[], decisions, enrichment);
    let artists: Vec<Option<usize>> = library
        .iter()
        .map(|s| {
            basis
                .artist(&s.artist)
                .filter(|_| !s.artist.trim().is_empty())
        })
        .collect();
    // How much each artist is played, over every way the library spells it.
    let plays_of: Vec<usize> = basis
        .clusters
        .iter()
        .map(|c| {
            c.spellings
                .iter()
                .map(|s| heard.get(&fold(s)).copied().unwrap_or(0))
                .sum()
        })
        .collect();
    let most = plays_of.iter().copied().max().unwrap_or(0) as f64;
    let familiarity = |a: Option<usize>| match a {
        Some(a) if most > 0.0 => (1.0 + plays_of[a] as f64).ln() / (1.0 + most).ln(),
        _ => 0.0,
    };
    let cap = ((size as f64 * tuning.max_artist_share).ceil() as usize).max(1);
    let popularity = Popularity::new(library, enrichment);

    let mut mixes = vec![];
    for m in moods {
        let wanted: Vec<(String, f64)> = m.tags.iter().map(|(t, w)| (fold(t), *w)).collect();
        let avoided: Vec<String> = m.avoid.iter().map(|t| fold(t)).collect();
        let first_year = m.years_back.map(|n| year.saturating_sub(n - 1));
        let mut seen = std::collections::BTreeSet::new();
        let mut candidates = vec![];
        for (((song, a), tags), played) in library
            .iter()
            .zip(&artists)
            .zip(&basis.tags)
            .zip(&popularity.place)
        {
            if !seen.insert(song.id.as_str()) {
                continue;
            }
            if m.min_popularity
                .is_some_and(|min| played.is_none_or(|p| p < min))
            {
                continue;
            }
            if first_year.is_some_and(|from| song.year.is_none_or(|y| y < from)) {
                continue;
            }
            if let (Some((low, high)), Some(bpm)) = (m.bpm, song.bpm) {
                if bpm < low || bpm > high {
                    continue;
                }
            }
            if avoided
                .iter()
                .any(|bad| tags.iter().any(|t| matches(t, bad)))
            {
                continue;
            }
            let fit: f64 = wanted
                .iter()
                .filter(|(w, _)| tags.iter().any(|t| matches(t, w)))
                .map(|(_, weight)| weight)
                .sum::<f64>()
                .min(1.0);
            if fit < m.min {
                continue;
            }
            candidates.push(Candidate {
                chosen: Chosen {
                    id: song.id.clone(),
                    base: false,
                    tags: fit,
                    together: 0.0,
                    close: 0.0,
                },
                artist: *a,
                song: song_key(song),
                score: fit
                    + FAMILIARITY_WEIGHT * familiarity(*a)
                    + m.min_popularity.and(*played).unwrap_or(0.0),
            });
        }
        let (list, _) = pick(candidates, size, cap);
        if list.len() >= MIN_MIX_SONGS.min(size) {
            mixes.push((m.name.trim().to_string(), list));
        }
    }
    mixes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str, artist: &str, genres: &[&str], year: Option<u16>) -> Song {
        Song {
            id: id.into(),
            artist: artist.into(),
            album: format!("Album of {artist}"),
            title: format!("Song {id}"),
            genres: genres.iter().map(|g| g.to_string()).collect(),
            year,
            ..Default::default()
        }
    }

    fn context(heard: &BTreeMap<String, usize>) -> Context<'_> {
        static NONE: std::sync::OnceLock<(Decisions, Enrichment)> = std::sync::OnceLock::new();
        let (decisions, enrichment) = NONE.get_or_init(Default::default);
        Context {
            heard,
            decisions,
            enrichment,
            year: 2026,
        }
    }

    fn mixes_of(library: &[Song], moods: &[Mood]) -> Vec<(String, Vec<String>)> {
        mood_mixes(
            library,
            moods,
            &context(&BTreeMap::new()),
            50,
            &Tuning::default(),
        )
        .into_iter()
        .map(|(name, list)| (name, list.into_iter().map(|c| c.id).collect()))
        .collect()
    }

    /// `n` songs of `genres`, by `n` artists (so that no artist share holds the mix short).
    fn many(prefix: &str, genres: &[&str], year: Option<u16>, n: usize) -> Vec<Song> {
        (0..n)
            .map(|i| {
                song(
                    &format!("{prefix}{i:02}"),
                    &format!("{prefix} {i}"),
                    genres,
                    year,
                )
            })
            .collect()
    }

    #[test]
    fn a_mood_holds_the_songs_that_fit_its_title_and_no_others() {
        let mut library = many("c", &["Classical"], None, 12);
        library.extend(many("s", &["Clásica"], None, 12));
        library.extend(many("r", &["Rock"], None, 12));
        let mixes = mixes_of(&library, &defaults());
        let classical = &mixes.iter().find(|(n, _)| n == "Classical").unwrap().1;
        assert_eq!(
            classical.len(),
            24,
            "Clásica is classical too: {classical:?}"
        );
        assert!(classical.iter().all(|id| !id.starts_with('r')));
    }

    #[test]
    fn a_tag_matches_whole_words_only() {
        assert!(matches("g:modern classical", "classical"));
        assert!(matches("l:classical", "classical"));
        assert!(!matches("g:neoclassical", "classical"));
        assert!(!matches("g:pop rock", "pop rocks"));
    }

    #[test]
    fn a_song_with_a_tag_the_mood_avoids_is_left_out_whatever_else_it_has() {
        let mut library = many("c", &["Classical"], None, 12);
        library.extend(many("x", &["Classical", "Pop"], None, 12));
        let mixes = mixes_of(&library, &defaults());
        let classical = &mixes.iter().find(|(n, _)| n == "Classical").unwrap().1;
        assert!(
            classical.iter().all(|id| id.starts_with('c')),
            "{classical:?}"
        );
    }

    #[test]
    fn a_window_of_years_keeps_the_recent_songs_and_drops_the_ones_with_no_year() {
        let mut library = many("n", &["Pop"], Some(2025), 6);
        library.extend(many("m", &["Pop"], Some(2026), 6));
        library.extend(many("o", &["Pop"], Some(2019), 12));
        library.extend(many("u", &["Pop"], None, 12));
        let recent = Mood {
            years_back: Some(2),
            ..mood("Recent", &[("pop", 1.0)], &[])
        };
        let mixes = mixes_of(&library, &[recent]);
        let recent = &mixes[0].1;
        assert_eq!(recent.len(), 12, "{recent:?}");
        assert!(recent
            .iter()
            .all(|id| id.starts_with('n') || id.starts_with('m')));
    }

    #[test]
    fn hits_are_the_pop_songs_lastfm_says_are_played_most_and_the_most_played_come_first() {
        let mut library = many("p", &["Pop"], None, 60);
        library.extend(many("q", &["Pop"], None, 20)); // no count: an artist not asked about
        library.extend(many("r", &["Rock"], None, 20));
        let mut enrichment = Enrichment::default();
        for (i, s) in library
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.id.starts_with('q'))
        {
            let weight = if s.id.starts_with('r') { 1 } else { 100 };
            let top = vec![(format!("song {}", s.id), (i as u64 + 1) * weight)];
            enrichment.insert(
                &s.artist,
                crate::enrich::ArtistInfo {
                    top: Some(top),
                    ..Default::default()
                },
            );
        }
        let heard = BTreeMap::new();
        let decisions = Decisions::default();
        let mixes = mood_mixes(
            &library,
            &defaults(),
            &Context {
                heard: &heard,
                decisions: &decisions,
                enrichment: &enrichment,
                year: 2026,
            },
            50,
            &Tuning::default(),
        );
        let (_, hits) = mixes.iter().find(|(n, _)| n == "Hits").unwrap();
        let ids: Vec<&str> = hits.iter().map(|c| c.id.as_str()).collect();
        // 60 pop songs (played 100 to 6000 times) and 20 rock ones (played less) have a count: the most played fifth of those
        // 80 are pop songs, and they are the hits.
        assert_eq!(ids[0], "p59", "{ids:?}");
        assert!(
            ids.len() >= 10 && ids.iter().all(|id| id.starts_with('p')),
            "{ids:?}"
        );
        assert!(
            ids.iter().all(|id| id[1..].parse::<usize>().unwrap() >= 44),
            "{ids:?}"
        );
    }

    #[test]
    fn a_mood_with_fewer_songs_than_a_mix_may_have_is_left_out() {
        let mut library = many("c", &["Classical"], None, 9);
        library.extend(many("r", &["Rock"], None, 30));
        assert!(mixes_of(&library, &defaults())
            .iter()
            .all(|(n, _)| n != "Classical"));
    }

    #[test]
    fn a_bpm_band_leaves_out_songs_outside_it_and_keeps_the_ones_with_no_bpm() {
        let mut library = many("f", &["Ambient"], None, 12);
        library.extend(many("q", &["Ambient"], None, 12).into_iter().map(|mut s| {
            s.bpm = Some(170);
            s
        }));
        let mixes = mixes_of(&library, &defaults());
        let focus = &mixes.iter().find(|(n, _)| n == "Focus").unwrap().1;
        assert_eq!(focus.len(), 12, "{focus:?}");
        assert!(focus.iter().all(|id| id.starts_with('f')));
    }

    #[test]
    fn the_artists_played_more_come_first_among_songs_that_fit_equally() {
        let library = many("c", &["Classical"], None, 30);
        let heard: BTreeMap<String, usize> = [("c 17".to_string(), 50)].into();
        let mixes = mood_mixes(
            &library,
            &defaults(),
            &context(&heard),
            5,
            &Tuning::default(),
        );
        let (_, classical) = mixes.iter().find(|(n, _)| n == "Classical").unwrap();
        assert_eq!(classical[0].id, "c17");
    }

    #[test]
    fn one_library_gives_the_same_moods_every_time() {
        let mut library = many("c", &["Classical"], None, 50);
        library.extend(many("a", &["Ambient"], None, 50));
        let first = mixes_of(&library, &defaults());
        for _ in 0..6 {
            assert_eq!(mixes_of(&library, &defaults()), first);
        }
    }

    #[test]
    fn recipes_are_checked_when_they_are_read() {
        let error = |json: &str| parse(json).unwrap_err();
        assert!(error(r#"[{"name":"","tags":{"pop":1}}]"#).contains("no name"));
        assert!(error(r#"[{"name":"A","tags":{}}]"#).contains("no tags"));
        assert!(error(r#"[{"name":"A","tags":{"pop":0}}]"#).contains("weight"));
        assert!(error(r#"[{"name":"A","tags":{"pop":1},"min":0}]"#).contains("minimum"));
        assert!(error(r#"[{"name":"A","tags":{"pop":1},"bpm":[120,80]}]"#).contains("band"));
        assert!(
            error(r#"[{"name":"A","tags":{"pop":1},"min_popularity":0}]"#)
                .contains("min_popularity")
        );
        assert!(
            error(r#"[{"name":"A","tags":{"pop":1}},{"name":"a","tags":{"rock":1}}]"#)
                .contains("two moods")
        );
        assert!(!error("not json").is_empty());
        let one = parse(r#"[{"name":"Calm","tags":{"ambient":1,"piano":0.5}}]"#).unwrap();
        assert_eq!(one[0].min, 0.5);
    }

    #[test]
    fn the_built_in_recipes_are_valid_and_survive_a_round_trip_through_json() {
        let json = serde_json::to_string(&defaults()).unwrap();
        assert_eq!(parse(&json).unwrap(), defaults());
    }
}
