//! Mixes from the artists a user played lately.
//!
//! Each mix is built on those artists (the base, 40%) and widened with songs by other artists whose tags are close to
//! the base (the similar part, 60%). Tags are the genres and moods of the songs; a tag shared by few songs of the
//! library says more than one shared by most of them, so rare tags weigh more. What the user played together says
//! more still: an artist heard in the same sittings as the base artists is close to them, whatever its tags.
//!
//! Pure functions over a snapshot of the library, like `taste`: nothing is read from a server and nothing is kept.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::enrich::Enrichment;
use crate::identity::{resolve, Decisions, Track};
use crate::normalize::{fold, work_title};
use crate::plays::{self, Play};
use crate::taste::Song;

/// A mix shorter than this is left out when a longer one exists (or is the only one left when none is that long).
pub(crate) const MIN_MIX_SONGS: usize = 10;

/// What decides who shares a mix and what gets in. `eval` takes them as options to try values without a new build, and the
/// engine reads them from `tuning.json` in its data folder (every field is optional there).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Tuning {
    /// The share of a mix that comes from the artists played lately (the base), between 0 and 1; the rest are similar songs.
    /// Whichever part cannot give its share, the other fills the gap.
    pub base_share: f64,
    /// Two groups of recent artists share a mix when the average of how close their artists are (see `affinities`)
    /// reaches this.
    pub min_aligned: f64,
    /// A similar song must score at least this to be in a mix: a mix is left short rather than filled with weak matches.
    pub min_fit: f64,
    /// A similar song whose own tags or whose artist's closeness to the base are known must also have at least this much
    /// from them (the tags' cosine plus half the closeness), whatever the sittings add: what was heard in the same sittings
    /// says nothing of style, so it cannot make up for a song that does not sound like the mix. A song with nothing known
    /// of it but the sittings is judged by the sittings alone.
    pub min_content: f64,
    /// The share of a mix one artist may take while others can still fill it; it is lifted only to fill what is left.
    pub max_artist_share: f64,
    /// At most this many mixes are made; the groups with the most recent artists come first.
    pub max_mixes: usize,
}

impl Default for Tuning {
    fn default() -> Tuning {
        Tuning {
            base_share: 0.4,
            min_aligned: 0.4,
            min_fit: 0.5,
            min_content: 0.45,
            max_artist_share: 0.2,
            max_mixes: 3,
        }
    }
}

impl Tuning {
    /// The tuning in a JSON object whose fields are all optional (what is left out keeps its default), or why it is not
    /// usable: an unknown field is an error, so that a typo does not quietly tune nothing.
    pub fn parse(json: &str) -> Result<Tuning, String> {
        let tuning: Tuning = serde_json::from_str(json).map_err(|e| e.to_string())?;
        tuning.check()?;
        Ok(tuning)
    }

    /// Whether every value is one the mixes can work with.
    pub fn check(&self) -> Result<(), String> {
        let share = |name: &str, v: f64| {
            if (0.0..=1.0).contains(&v) {
                Ok(())
            } else {
                Err(format!("{name} must be between 0 and 1, not {v}"))
            }
        };
        share("base_share", self.base_share)?;
        share("max_artist_share", self.max_artist_share)?;
        for (name, v) in [
            ("min_aligned", self.min_aligned),
            ("min_fit", self.min_fit),
            ("min_content", self.min_content),
        ] {
            if !v.is_finite() || v < 0.0 {
                return Err(format!("{name} must be 0 or more, not {v}"));
            }
        }
        if self.max_mixes == 0 {
            return Err("max_mixes must be at least 1".into());
        }
        Ok(())
    }
}

/// How much an artist played in the same sittings as the base adds to the score of its songs, next to the tags' cosine
/// (both lie between 0 and 1).
const CO_LISTENING_WEIGHT: f64 = 1.0;

/// How much an artist that outside services call close to the base artists adds to the score of its songs, times how close
/// they say it is (between 0 and 1).
const SIMILAR_WEIGHT: f64 = 0.5;
/// An artist's crowd tag counts as a tag of each of its songs when it is at least this strong (between 0 and 1): the weak
/// ones describe a few listeners' habits and not the artist.
const MIN_CROWD_TAG: f64 = 0.3;

type Profile = BTreeMap<String, f64>;

/// At most `Tuning::max_mixes` mixes of up to `size` songs each, built from `recent` (artist names, most recent first).
///
/// The recent artists are told apart by identity, not spelling. They are grouped by how well they fit each other (their
/// tags, what was heard in the same sittings, what outside services call close): artists that fit share a mix, and one
/// that fits none makes a mix of its own or, when that is too short, is left out. A song is in at most one mix. When a
/// mix cannot fill one of its two parts, the other part fills the gap; a mix with no songs is left out.
///
/// No artist takes more than `Tuning::max_artist_share` of a mix, unless that is the only way to fill it. A similar song
/// that scores under `Tuning::min_fit` is not used: the mix is left short.
///
/// `played` is the user's history of plays, in time order: the artists heard in the same sittings as a mix's base artists
/// make its similar songs likelier. It may be empty. So may `enrichment`, what outside services say of the artists: their
/// crowd tags are added to the tags of their songs, and the artists they call close to a mix's base artists make its
/// similar songs likelier.
pub fn recent_artist_mixes(
    library: &[Song],
    recent: &[String],
    played: &[Play],
    decisions: &Decisions,
    enrichment: &Enrichment,
    size: usize,
) -> Vec<Vec<String>> {
    let tuning = Tuning::default();
    recent_artist_mixes_explained(
        library, recent, played, decisions, enrichment, size, &tuning,
    )
    .into_iter()
    .map(|mix| mix.into_iter().map(|song| song.id).collect())
    .collect()
}

/// A song of a mix, and why it is there.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chosen {
    pub id: String,
    /// It is of one of the artists played lately (the base), and not a similar song.
    pub base: bool,
    /// For a similar song, the three things that bring it in, which add up to its score: the cosine of its tags with
    /// the base, how often its artist was heard in the sittings of the base, and how close outside services call that
    /// artist (the last counts half). All zero for a song of the base.
    pub tags: f64,
    pub together: f64,
    pub close: f64,
}

/// The same mixes as [`recent_artist_mixes`] (which uses the default `tuning`), each song with the reason it is there.
pub fn recent_artist_mixes_explained(
    library: &[Song],
    recent: &[String],
    played: &[Play],
    decisions: &Decisions,
    enrichment: &Enrichment,
    size: usize,
    tuning: &Tuning,
) -> Vec<Vec<Chosen>> {
    if size == 0 {
        return vec![];
    }
    let basis = Basis::new(library, played, decisions, enrichment);
    let artist = |name: &str| basis.artist(name);
    let clusters = &basis.clusters;
    let (tags, weight, sittings) = (&basis.tags, &basis.weight, &basis.sittings);

    let mut recent_artists: Vec<usize> = vec![];
    for a in recent.iter().filter_map(|name| artist(name)) {
        if !recent_artists.contains(&a) {
            recent_artists.push(a);
        }
    }
    if recent_artists.is_empty() {
        return vec![];
    }

    let songs: Vec<(&Song, Option<usize>)> = library
        .iter()
        .map(|s| (s, artist(&s.artist).filter(|_| !s.artist.trim().is_empty())))
        .collect();
    let profiles = basis.profiles(&songs, &recent_artists);

    let affinity = affinities(
        &recent_artists,
        &profiles,
        sittings,
        clusters,
        enrichment,
        &artist,
    );
    let minimum = MIN_MIX_SONGS.min(size);
    let cap = ((size as f64 * tuning.max_artist_share).ceil() as usize).max(1);

    let mut used: BTreeSet<&str> = BTreeSet::new();
    let mut mixes: Vec<Vec<Chosen>> = vec![];
    let mut short: Vec<Vec<Chosen>> = vec![];
    for group in align(&recent_artists, &affinity, tuning.min_aligned) {
        if mixes.len() == tuning.max_mixes {
            break;
        }
        let mut profile = Profile::new();
        for a in &group {
            for (t, v) in profiles.get(a).into_iter().flatten() {
                *profile.entry(t.clone()).or_default() += v;
            }
        }
        let together = co_listening(sittings, &group);
        let close = close_artists(&group, clusters, enrichment, &artist);
        let rank = |a: usize| recent_artists.iter().position(|&r| r == a).unwrap_or(0);

        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let (mut base, mut similar) = (vec![], vec![]);
        for ((song, a), song_tags) in songs.iter().zip(tags) {
            if used.contains(song.id.as_str()) || !seen.insert(song.id.as_str()) {
                continue;
            }
            match a {
                Some(a) if group.contains(a) => base.push(Candidate {
                    chosen: Chosen {
                        id: song.id.clone(),
                        base: true,
                        ..Chosen::default()
                    },
                    artist: Some(*a),
                    song: song_key(song),
                    // Between 1 and 2: the latest artist leads, but not by so much that it crowds the others out.
                    score: 1.0 + 1.0 / (1 + rank(*a)) as f64,
                }),
                Some(a) if recent_artists.contains(a) => {}
                _ => {
                    let by_tags = cosine(song_tags, weight, &profile);
                    let heard = a.and_then(|a| together.get(&a)).copied().unwrap_or(0.0);
                    let named = a.and_then(|a| close.get(&a)).copied().unwrap_or(0.0);
                    let score = by_tags + CO_LISTENING_WEIGHT * heard + SIMILAR_WEIGHT * named;
                    let content = by_tags + SIMILAR_WEIGHT * named;
                    let content_known = !song_tags.is_empty() || named > 0.0;
                    if score >= tuning.min_fit && (!content_known || content >= tuning.min_content)
                    {
                        similar.push(Candidate {
                            chosen: Chosen {
                                id: song.id.clone(),
                                base: false,
                                tags: by_tags,
                                together: heard,
                                close: named,
                            },
                            artist: *a,
                            song: song_key(song),
                            score,
                        });
                    }
                }
            }
        }
        let (base, base_within_cap) = pick(base, size, cap);
        let (similar, similar_within_cap) = pick(similar, size, cap);

        // First fill the mix with what stays within the share of an artist; lift the share only if that leaves it short.
        let list = {
            let within = fill(
                &base[..base_within_cap],
                &similar[..similar_within_cap],
                size,
                tuning.base_share,
            );
            if within.len() < size {
                fill(&base, &similar, size, tuning.base_share)
            } else {
                within
            }
        };
        if list.is_empty() {
            continue;
        }
        if list.len() < minimum {
            short.push(list);
            continue;
        }
        let in_list: BTreeSet<&str> = list.iter().map(|c| c.id.as_str()).collect();
        used.extend(
            library
                .iter()
                .map(|s| s.id.as_str())
                .filter(|id| in_list.contains(id)),
        );
        mixes.push(list);
    }
    if mixes.is_empty() {
        // Nothing reached the minimum: the longest of the short ones is better than no mix.
        mixes.extend(short.into_iter().rev().max_by_key(Vec::len)); // the earliest of the longest
    }
    mixes
}

/// The name of the playlist of a mix of artists: the artists it is built on, the first two of them by how lately they were
/// played, and how many more there are (`Queen, David Bowie +1 - Smartplay`). Two mixes never share one (their artists are
/// different). An artist that is called like a mood (a band called `Focus`) would give the mood's name and each would rewrite
/// the other's playlist every minute, so the title then says what it is: `Focus (artists) - Smartplay`.
pub fn artist_mix_playlist(library: &[Song], mix: &[Chosen], mood_names: &[&str]) -> String {
    let title = mix_title(library, mix);
    if mood_names.iter().any(|m| fold(m) == fold(&title)) {
        crate::playlist_name(&format!("{title} (artists)"))
    } else {
        crate::playlist_name(&title)
    }
}

/// The artists a mix is built on, as they name its playlist: `Queen, David Bowie +1`.
fn mix_title(library: &[Song], mix: &[Chosen]) -> String {
    let artist_of: BTreeMap<&str, &str> = library
        .iter()
        .map(|s| (s.id.as_str(), s.artist.trim()))
        .collect();
    let mut seen = BTreeSet::new();
    let artists: Vec<&str> = mix
        .iter()
        .filter(|c| c.base)
        .filter_map(|c| artist_of.get(c.id.as_str()).copied())
        .filter(|a| !a.is_empty() && seen.insert(fold(a)))
        .collect();
    match artists.as_slice() {
        [] => "Mix".to_string(),
        [first, second, rest @ ..] if !rest.is_empty() => {
            format!("{first}, {second} +{}", rest.len())
        }
        some => some.join(", "),
    }
}

/// The mix of up to `size` songs from the base and the similar songs, `base_share` of them from the base: whichever part
/// cannot give its share, the other fills the gap.
fn fill(base: &[Chosen], similar: &[Chosen], size: usize, base_share: f64) -> Vec<Chosen> {
    let want_base = (size as f64 * base_share).ceil() as usize;
    let mut take_base = base.len().min(want_base);
    let take_similar = similar.len().min(size - take_base);
    take_base = base.len().min(size - take_similar);
    interleave(&base[..take_base], &similar[..take_similar], base_share)
}

/// What the mixes and their checks are worked out from: who is who, the tags of the songs and what was heard together.
pub(crate) struct Basis {
    pub(crate) clusters: Vec<crate::identity::Cluster>,
    artist_of_name: BTreeMap<String, usize>,
    pub(crate) tags: Vec<BTreeSet<String>>,
    pub(crate) weight: BTreeMap<String, f64>,
    sittings: Vec<BTreeSet<usize>>,
}

impl Basis {
    pub(crate) fn new(
        library: &[Song],
        played: &[Play],
        decisions: &Decisions,
        enrichment: &Enrichment,
    ) -> Basis {
        let tracks: Vec<Track> = library
            .iter()
            .map(|s| Track {
                artist: s.artist.clone(),
                album: s.album.clone(),
                title: s.title.clone(),
                artist_mbid: s.artist_mbid.clone(),
            })
            .collect();
        let clusters = resolve(&tracks, decisions).clusters;
        let mut artist_of_name: BTreeMap<String, usize> = BTreeMap::new();
        for (i, cluster) in clusters.iter().enumerate() {
            for spelling in &cluster.spellings {
                artist_of_name.insert(fold(spelling), i);
            }
        }
        let tags: Vec<BTreeSet<String>> = library.iter().map(|s| tags_of(s, enrichment)).collect();
        let weight = idf(&tags);
        let mut basis = Basis {
            clusters,
            artist_of_name,
            tags,
            weight,
            sittings: vec![],
        };
        // The artists of the library heard in each sitting (the main artist of each play).
        basis.sittings = plays::sessions(played)
            .iter()
            .map(|sitting| {
                sitting
                    .iter()
                    .filter_map(|p| p.artists.iter().map(|a| a.trim()).find(|a| !a.is_empty()))
                    .filter_map(|name| basis.artist(name))
                    .collect()
            })
            .collect();
        basis
    }

    pub(crate) fn artist(&self, name: &str) -> Option<usize> {
        self.artist_of_name.get(&fold(name)).copied()
    }

    /// The weighted tags of each of `wanted` artists, over all their songs.
    fn profiles(
        &self,
        songs: &[(&Song, Option<usize>)],
        wanted: &[usize],
    ) -> BTreeMap<usize, Profile> {
        let mut profiles: BTreeMap<usize, Profile> = BTreeMap::new();
        for ((_, a), song_tags) in songs.iter().zip(&self.tags) {
            if let Some(a) = a.filter(|a| wanted.contains(a)) {
                let profile = profiles.entry(a).or_default();
                for t in song_tags {
                    *profile.entry(t.clone()).or_default() += self.weight[t];
                }
            }
        }
        profiles
    }
}

/// How well the artists of a mix fit each other: the average of the closeness of every two of them (see `affinities`;
/// a pair that nothing ties together counts as 0), or nothing when the mix has fewer than two artists. The mixes are
/// made so that this is high, and `eval` prints it to tell a mix with a clear line from one without.
pub fn mix_fit(
    library: &[Song],
    mix: &[String],
    played: &[Play],
    decisions: &Decisions,
    enrichment: &Enrichment,
) -> Option<f64> {
    let basis = Basis::new(library, played, decisions, enrichment);
    let songs: Vec<(&Song, Option<usize>)> = library
        .iter()
        .map(|s| {
            (
                s,
                basis
                    .artist(&s.artist)
                    .filter(|_| !s.artist.trim().is_empty()),
            )
        })
        .collect();
    let ids: BTreeSet<&str> = mix.iter().map(String::as_str).collect();
    let mut artists: Vec<usize> = vec![];
    for (song, a) in &songs {
        if let Some(a) = a.filter(|a| ids.contains(song.id.as_str()) && !artists.contains(a)) {
            artists.push(a);
        }
    }
    if artists.len() < 2 {
        return None;
    }
    let profiles = basis.profiles(&songs, &artists);
    let affinity = affinities(
        &artists,
        &profiles,
        &basis.sittings,
        &basis.clusters,
        enrichment,
        &|name| basis.artist(name),
    );
    Some(affinity.values().sum::<f64>() / (artists.len() * (artists.len() - 1) / 2) as f64)
}

/// How often each other artist was heard in the sittings of `group`, between 0 and 1: the sittings holding both, over
/// the geometric mean of the sittings holding the group and those holding the artist. An artist always heard alone
/// scores 0, and one the group's artists are heard with every time scores 1.
fn co_listening(sittings: &[BTreeSet<usize>], group: &[usize]) -> BTreeMap<usize, f64> {
    let with_group = |s: &&BTreeSet<usize>| group.iter().any(|a| s.contains(a));
    let group_sittings = sittings.iter().filter(with_group).count();
    let mut both: BTreeMap<usize, usize> = BTreeMap::new();
    for sitting in sittings.iter().filter(with_group) {
        for &other in sitting.iter().filter(|o| !group.contains(o)) {
            *both.entry(other).or_default() += 1;
        }
    }
    both.into_iter()
        .map(|(other, n)| {
            let of_other = sittings.iter().filter(|s| s.contains(&other)).count();
            (
                other,
                n as f64 / ((group_sittings * of_other) as f64).sqrt(),
            )
        })
        .collect()
}

/// How close outside services say each artist is to the artists of `group`, between 0 and 1: the closest of them counts.
fn close_artists(
    group: &[usize],
    clusters: &[crate::identity::Cluster],
    enrichment: &Enrichment,
    artist: &dyn Fn(&str) -> Option<usize>,
) -> BTreeMap<usize, f64> {
    let mut close: BTreeMap<usize, f64> = BTreeMap::new();
    for &a in group {
        let infos = clusters[a]
            .spellings
            .iter()
            .filter_map(|s| enrichment.get(s));
        for (name, matched) in infos.flat_map(|i| &i.similar) {
            if let Some(other) = artist(name).filter(|o| !group.contains(o)) {
                let best = close.entry(other).or_default();
                *best = best.max(*matched);
            }
        }
    }
    close
}

/// The genres (every one the song has), the moods of a song and the crowd tags of its artist, folded. A mood and a genre
/// of the same name are two tags, and so is a crowd tag (`l:`), which comes from another source.
pub(crate) fn tags_of(song: &Song, enrichment: &Enrichment) -> BTreeSet<String> {
    let genres: Vec<&str> = if song.genres.is_empty() {
        song.genre.as_deref().into_iter().collect()
    } else {
        song.genres.iter().map(String::as_str).collect()
    };
    let genres = genres.into_iter().map(|g| format!("g:{}", fold(g)));
    let moods = song.moods.iter().map(|m| format!("m:{}", fold(m)));
    let crowd = enrichment
        .get(&song.artist)
        .into_iter()
        .flat_map(|info| &info.tags)
        .filter(|(_, weight)| *weight >= MIN_CROWD_TAG)
        .map(|(tag, _)| format!("l:{tag}"));
    genres
        .chain(moods)
        .chain(crowd)
        .filter(|t| t.len() > 2) // a tag that folds to nothing, such as "-", is no tag
        .collect()
}

/// How much each tag says: `ln(1 + songs / songs with the tag)`, so a tag on every song weighs least.
fn idf(tags: &[BTreeSet<String>]) -> BTreeMap<String, f64> {
    let mut count: BTreeMap<&str, usize> = BTreeMap::new();
    for t in tags.iter().flatten() {
        *count.entry(t.as_str()).or_default() += 1;
    }
    count
        .into_iter()
        .map(|(t, n)| (t.to_string(), (1.0 + tags.len() as f64 / n as f64).ln()))
        .collect()
}

fn norm<'a>(values: impl Iterator<Item = &'a f64>) -> f64 {
    values.map(|v| v * v).sum::<f64>().sqrt()
}

/// Cosine between a song's weighted tags and a profile: 0 when they share nothing, or when either has no tags.
fn cosine(song: &BTreeSet<String>, weight: &BTreeMap<String, f64>, profile: &Profile) -> f64 {
    let dot: f64 = song
        .iter()
        .map(|t| weight[t] * profile.get(t).copied().unwrap_or(0.0))
        .sum();
    let denominator = norm(song.iter().map(|t| &weight[t])) * norm(profile.values());
    if denominator == 0.0 {
        0.0
    } else {
        dot / denominator
    }
}

fn cosine_profiles(a: &Profile, b: &Profile) -> f64 {
    let dot: f64 = a
        .iter()
        .map(|(t, v)| v * b.get(t).copied().unwrap_or(0.0))
        .sum();
    let denominator = norm(a.values()) * norm(b.values());
    if denominator == 0.0 {
        0.0
    } else {
        dot / denominator
    }
}

/// How well each pair of recent artists fits, between 0 and about 2: the cosine of their tags, plus what the sittings say
/// (as for the songs, see [`co_listening`]), plus what outside services say, half as much. The pair is keyed with the
/// smaller id first.
fn affinities(
    recent: &[usize],
    profiles: &BTreeMap<usize, Profile>,
    sittings: &[BTreeSet<usize>],
    clusters: &[crate::identity::Cluster],
    enrichment: &Enrichment,
    artist: &dyn Fn(&str) -> Option<usize>,
) -> BTreeMap<(usize, usize), f64> {
    let empty = Profile::new();
    let profile = |a: &usize| profiles.get(a).unwrap_or(&empty);
    let mut affinity = BTreeMap::new();
    for (i, &x) in recent.iter().enumerate() {
        let together = co_listening(sittings, &[x]);
        let close = close_artists(&[x], clusters, enrichment, artist);
        for &y in &recent[i + 1..] {
            let reverse = close_artists(&[y], clusters, enrichment, artist);
            let named = close
                .get(&y)
                .copied()
                .unwrap_or(0.0)
                .max(reverse.get(&x).copied().unwrap_or(0.0));
            let fit = cosine_profiles(profile(&x), profile(&y))
                + CO_LISTENING_WEIGHT * together.get(&y).copied().unwrap_or(0.0)
                + SIMILAR_WEIGHT * named;
            affinity.insert((x.min(y), x.max(y)), fit);
        }
    }
    affinity
}

/// The recent artists in groups of artists that fit each other: every artist starts alone, and the two groups that fit
/// best (the average over the pairs between them) join while that average reaches `min_aligned`. An artist that fits
/// no group stays alone. Each group keeps the recency order, and the groups come in the order of their latest artist.
fn align(
    recent: &[usize],
    affinity: &BTreeMap<(usize, usize), f64>,
    min_aligned: f64,
) -> Vec<Vec<usize>> {
    let between = |a: &[usize], b: &[usize]| {
        let total: f64 = a
            .iter()
            .flat_map(|&x| b.iter().map(move |&y| (x.min(y), x.max(y))))
            .map(|pair| affinity.get(&pair).copied().unwrap_or(0.0))
            .sum();
        total / (a.len() * b.len()) as f64
    };
    let mut groups: Vec<Vec<usize>> = recent.iter().map(|&a| vec![a]).collect();
    loop {
        let mut best: Option<(usize, usize, f64)> = None;
        for i in 0..groups.len() {
            for j in i + 1..groups.len() {
                let fit = between(&groups[i], &groups[j]);
                if fit >= min_aligned && best.is_none_or(|(_, _, b)| fit > b) {
                    best = Some((i, j, fit));
                }
            }
        }
        let Some((i, j, _)) = best else {
            return groups;
        };
        let joined = groups.remove(j);
        groups[i].extend(joined);
        let rank = |a: &usize| recent.iter().position(|r| r == a);
        groups[i].sort_by_key(rank);
    }
}

pub(crate) struct Candidate {
    pub(crate) chosen: Chosen,
    pub(crate) artist: Option<usize>,
    /// The song without what makes it a version of itself (see [`song_key`]): a demo and a live take of one song share it.
    pub(crate) song: String,
    pub(crate) score: f64,
}

/// How much a version of a song already picked lowers the next: a second one counts for a fifth, a third for a ninth.
const VARIANT_PENALTY: f64 = 4.0;

/// The artist and title of a song, folded, without the part of the title after ` - ` or in brackets: `Tangled - Demo`,
/// `Maps - Live In Manchester` and `Magic Dance (Remix)` are versions of `Tangled`, `Maps` and `Magic Dance`. They stay
/// different songs; this only tells a mix not to fill itself with the variants of one.
pub(crate) fn song_key(song: &Song) -> String {
    format!("{}\u{1}{}", fold(&song.artist), work_title(&song.title))
}

/// Up to `limit` ids, best first, and how many of them (the first ones) keep every artist within `cap` songs. Each pick
/// lowers the next song of the same artist (its score is divided by one plus the songs already picked from that artist),
/// so the list mixes artists instead of playing one out, and lowers the versions of the same song much more, so that a
/// demo, a live take and a remix of one song do not fill it. Only when no song is left within the cap do the artists
/// that have more songs to give carry on, past it.
pub(crate) fn pick(
    mut candidates: Vec<Candidate>,
    limit: usize,
    cap: usize,
) -> (Vec<Chosen>, usize) {
    let mut picked_from: BTreeMap<usize, usize> = BTreeMap::new();
    let mut picked_versions: BTreeMap<String, usize> = BTreeMap::new();
    let mut list = vec![];
    let mut within_cap = None;
    while list.len() < limit && !candidates.is_empty() {
        let best = {
            let taken = |c: &Candidate| {
                c.artist
                    .map_or(0, |a| picked_from.get(&a).copied().unwrap_or(0))
            };
            let weight = |c: &Candidate| {
                let versions = picked_versions.get(&c.song).copied().unwrap_or(0);
                c.score / (1 + taken(c)) as f64 / (1.0 + VARIANT_PENALTY * versions as f64)
            };
            (0..candidates.len())
                .filter(|&i| within_cap.is_some() || taken(&candidates[i]) < cap)
                .max_by(|&x, &y| {
                    weight(&candidates[x])
                        .total_cmp(&weight(&candidates[y]))
                        .then_with(|| candidates[y].chosen.id.cmp(&candidates[x].chosen.id))
                })
        };
        let Some(best) = best else {
            within_cap = Some(list.len());
            continue;
        };
        let chosen = candidates.swap_remove(best);
        if let Some(a) = chosen.artist {
            *picked_from.entry(a).or_default() += 1;
        }
        *picked_versions.entry(chosen.song.clone()).or_default() += 1;
        list.push(chosen.chosen);
    }
    let within_cap = within_cap.unwrap_or(list.len());
    (list, within_cap)
}

/// The two lists woven so that, at every point, the base songs so far are the share asked for, rounded up (with a share
/// of 0.4 the order is base, similar, base, similar, similar, and so on); whichever runs out first, the other carries on.
fn interleave(base: &[Chosen], similar: &[Chosen], base_share: f64) -> Vec<Chosen> {
    let (mut base, mut similar) = (base.iter().peekable(), similar.iter().peekable());
    let mut list = vec![];
    let mut from_base = 0usize;
    while base.peek().is_some() || similar.peek().is_some() {
        let due = (base_share * (list.len() + 1) as f64 - 1e-9).ceil() as usize;
        let next = if (from_base < due && base.peek().is_some()) || similar.peek().is_none() {
            from_base += 1;
            base.next()
        } else {
            similar.next()
        };
        list.extend(next.cloned());
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str, artist: &str, genres: &[&str], moods: &[&str]) -> Song {
        Song {
            id: id.into(),
            artist: artist.into(),
            album: format!("Album of {artist}"),
            title: format!("Song {id}"),
            genres: genres.iter().map(|g| g.to_string()).collect(),
            moods: moods.iter().map(|m| m.to_string()).collect(),
            ..Default::default()
        }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn mixes(library: &[Song], recent: &[&str], size: usize) -> Vec<Vec<String>> {
        recent_artist_mixes(
            library,
            &names(recent),
            &[],
            &Decisions::default(),
            &Enrichment::default(),
            size,
        )
    }

    #[test]
    fn songs_that_tie_are_picked_by_their_ids_whatever_the_run() {
        // Thirty songs of one artist with the same tags: their scores are equal, and only the order in which the tags are
        // added up (a set has none) could tell them apart in the last digit. Other artists make the tags weigh differently.
        let mut library = vec![];
        for i in 0..30 {
            library.push(song(
                &format!("t{i:02}"),
                "Tied",
                &["Indie", "Pop", "Rock", "Dream"],
                &["Calm", "Night", "Slow", "Sad"],
            ));
        }
        for i in 0..40 {
            let genres: &[&str] = match i % 4 {
                0 => &["Rock"],
                1 => &["Pop", "Indie"],
                2 => &["Dream", "Rock"],
                _ => &["Pop"],
            };
            library.push(song(
                &format!("o{i:02}"),
                &format!("Other {i}"),
                genres,
                &["Night"],
            ));
        }
        let first = mixes(&library, &["Tied"], 50);
        for _ in 0..30 {
            assert_eq!(
                mixes(&library, &["Tied"], 50),
                first,
                "the same library gave another mix"
            );
        }
        let tied: Vec<&String> = first
            .iter()
            .flatten()
            .filter(|id| id.starts_with('t'))
            .collect();
        let mut by_id = tied.clone();
        by_id.sort();
        assert_eq!(
            tied, by_id,
            "songs that tie must come in the order of their ids"
        );
        assert_eq!(tied[0], "t00");
    }

    /// Two recent artists (rock) and, among others, rock and jazz artists they never played.
    fn sample() -> Vec<Song> {
        let mut library = vec![];
        for i in 1..=4 {
            library.push(song(&format!("a{i}"), "Band A", &["Rock"], &["Energetic"]));
            library.push(song(&format!("b{i}"), "Band B", &["Rock", "Indie"], &[]));
            library.push(song(&format!("r{i}"), "Rocker", &["Rock"], &["Energetic"]));
            library.push(song(&format!("i{i}"), "Indie Kid", &["Indie"], &[]));
            library.push(song(&format!("j{i}"), "Jazzman", &["Jazz"], &["Mellow"]));
        }
        library.push(song("u1", "Untagged", &[], &[]));
        library
    }

    #[test]
    fn nothing_recent_nothing_to_mix() {
        assert!(mixes(&sample(), &[], 5).is_empty());
        assert!(mixes(&sample(), &["Nobody"], 5).is_empty());
        assert!(mixes(&sample(), &["Band A"], 0).is_empty());
    }

    #[test]
    fn two_of_every_five_songs_are_the_artists_played_and_three_are_similar() {
        let mix = &mixes(&sample(), &["Band A"], 5)[0];
        let from_base = |id: &String| id.starts_with('a');
        assert_eq!(mix.len(), 5);
        assert_eq!(
            mix.iter().map(from_base).collect::<Vec<_>>(),
            [true, false, true, false, false],
            "{mix:?}"
        );
    }

    #[test]
    fn the_share_of_the_artists_played_is_a_setting() {
        let library: Vec<Song> = (0..20)
            .flat_map(|i| {
                [
                    song(&format!("a{i:02}"), &format!("A{i}"), &["Rock"], &[]),
                    song(&format!("t{i:02}"), &format!("T{i}"), &["Rock"], &[]),
                ]
            })
            .collect();
        let base_songs = |share: f64| {
            let tuning = Tuning {
                base_share: share,
                max_artist_share: 1.0,
                ..Tuning::default()
            };
            let recent: Vec<String> = (0..4).map(|i| format!("A{i}")).collect();
            let mixes = recent_artist_mixes_explained(
                &library,
                &recent,
                &[],
                &Decisions::default(),
                &Enrichment::default(),
                10,
                &tuning,
            );
            assert_eq!(mixes[0].len(), 10);
            mixes[0].iter().filter(|c| c.base).count()
        };
        // The four artists played have four songs each between them (a00-a03 only: one song each), so the base is short of
        // its share when it is large, and the similar songs fill the gap.
        assert_eq!(base_songs(0.0), 0);
        assert_eq!(base_songs(0.2), 2);
        assert_eq!(base_songs(0.4), 4);
        assert_eq!(
            base_songs(0.6),
            4,
            "only four songs are of the artists played"
        );
    }

    #[test]
    fn the_tuning_is_read_from_json_with_what_is_left_out_as_default() {
        assert_eq!(Tuning::parse("{}").unwrap(), Tuning::default());
        let tuning = Tuning::parse(r#"{"base_share": 0.25, "max_mixes": 2}"#).unwrap();
        assert_eq!((tuning.base_share, tuning.max_mixes), (0.25, 2));
        assert_eq!(tuning.min_fit, Tuning::default().min_fit);
        assert!(Tuning::parse(r#"{"base_share": 1.5}"#)
            .unwrap_err()
            .contains("base_share"));
        assert!(Tuning::parse(r#"{"max_mixes": 0}"#).is_err());
        assert!(
            Tuning::parse(r#"{"base_shar": 0.5}"#).is_err(),
            "a typo is an error, not a setting that does nothing"
        );
    }

    #[test]
    fn similar_songs_share_tags_and_never_come_from_the_artists_played_or_from_untagged_songs() {
        let all = mixes(&sample(), &["Band A"], 50);
        let mix = &all[0];
        assert!(mix.iter().any(|id| id.starts_with('r')), "{mix:?}");
        assert!(
            !mix.iter().any(|id| id.starts_with('j')),
            "jazz shares nothing: {mix:?}"
        );
        assert!(!mix.iter().any(|id| id == "u1"));
        let base = mix.iter().filter(|id| id.starts_with('a')).count();
        assert_eq!(base, 4, "every song of the artist played: {mix:?}");
    }

    #[test]
    fn a_tag_most_songs_share_counts_for_less_than_a_rare_one() {
        // Rock is everywhere; Shoegaze is rare. A shoegaze fan's mix reaches the shoegaze song before a plain rock one.
        let mut library = vec![song("base", "Fan", &["Rock", "Shoegaze"], &[])];
        for i in 0..8 {
            library.push(song(&format!("rock{i}"), &format!("R{i}"), &["Rock"], &[]));
        }
        library.push(song("gaze", "Slowdive", &["Shoegaze"], &[]));
        // The order is what is tried, so the floor for a similar song is lowered to let the plain rock ones in.
        let tuning = Tuning {
            min_fit: 0.1,
            min_content: 0.0,
            ..Tuning::default()
        };
        let mix: Vec<String> = recent_artist_mixes_explained(
            &library,
            &names(&["Fan"]),
            &[],
            &Decisions::default(),
            &Enrichment::default(),
            5,
            &tuning,
        )[0]
        .iter()
        .map(|c| c.id.clone())
        .collect();
        assert_eq!(mix[..4], ["base", "gaze", "rock0", "rock1"][..], "{mix:?}");
    }

    #[test]
    fn a_part_that_cannot_be_filled_is_filled_by_the_other() {
        // One song of the artist played: the other four places go to similar songs.
        let library = vec![
            song("a1", "Band A", &["Rock"], &[]),
            song("r1", "R1", &["Rock"], &[]),
            song("r2", "R2", &["Rock"], &[]),
            song("r3", "R3", &["Rock"], &[]),
            song("r4", "R4", &["Rock"], &[]),
            song("r5", "R5", &["Rock"], &[]),
        ];
        let mix = &mixes(&library, &["Band A"], 5)[0];
        assert_eq!(mix.len(), 5);
        assert_eq!(mix[0], "a1");
        // And the other way round: nothing is similar, so the artist alone fills the mix.
        let alone = vec![
            song("a1", "Band A", &["Rock"], &[]),
            song("a2", "Band A", &["Rock"], &[]),
            song("a3", "Band A", &["Rock"], &[]),
            song("a4", "Band A", &["Rock"], &[]),
        ];
        assert_eq!(mixes(&alone, &["Band A"], 4)[0].len(), 4);
    }

    #[test]
    fn a_mix_mixes_artists_instead_of_playing_one_out() {
        let mut library = vec![];
        for i in 0..4 {
            library.push(song(&format!("a{i}"), "Band A", &["Rock"], &[]));
            library.push(song(&format!("b{i}"), "Band B", &["Rock"], &[]));
        }
        let mix = &mixes(&library, &["Band A", "Band B"], 4)[0];
        let a = mix.iter().filter(|id| id.starts_with('a')).count();
        assert_eq!((mix.len(), a), (4, 2), "{mix:?}");
    }

    #[test]
    fn the_more_recent_artist_comes_first() {
        let library = vec![
            song("a1", "Band A", &["Rock"], &[]),
            song("b1", "Band B", &["Rock"], &[]),
        ];
        assert_eq!(mixes(&library, &["Band B", "Band A"], 5)[0], ["b1", "a1"]);
        assert_eq!(mixes(&library, &["Band A", "Band B"], 5)[0], ["a1", "b1"]);
    }

    #[test]
    fn recent_names_match_the_library_by_identity_not_by_spelling() {
        let library = vec![song("s1", "Sigur Rós", &[], &[])];
        assert_eq!(mixes(&library, &["sigur ros"], 5)[0], ["s1"]);
        let library = vec![
            song("s1", "Patricio Rey y sus Redonditos de Ricota", &[], &[]),
            song("v1", "Los Redonditos de Ricota", &[], &[]),
        ];
        // They share an album name only through the helper below, so the user joins them.
        let mut decisions = Decisions::default();
        decisions.same(
            "Los Redonditos de Ricota",
            "Patricio Rey y sus Redonditos de Ricota",
        );
        let mix = recent_artist_mixes(
            &library,
            &names(&["Los Redonditos de Ricota"]),
            &[],
            &decisions,
            &Enrichment::default(),
            5,
        );
        assert_eq!(mix[0].len(), 2, "{mix:?}");
    }

    /// Two rock artists and two jazz artists, all recent, jazz played first and last so that recency alone would not
    /// separate them.
    fn rock_and_jazz() -> Vec<Song> {
        let mut library = vec![];
        for (artist, genre) in [
            ("R1", "Rock"),
            ("R2", "Rock"),
            ("J1", "Jazz"),
            ("J2", "Jazz"),
        ] {
            for i in 0..5 {
                library.push(song(&format!("{artist}-{i}"), artist, &[genre], &[]));
            }
        }
        library
    }

    #[test]
    fn recent_artists_are_split_in_two_mixes_by_their_tags() {
        let all = mixes(&rock_and_jazz(), &["J1", "R1", "R2", "J2"], 50);
        assert_eq!(all.len(), 2);
        for mix in &all {
            let jazz = mix.iter().filter(|id| id.starts_with('J')).count();
            assert!(jazz == 0 || jazz == mix.len(), "a coherent mix: {mix:?}");
            assert_eq!(mix.len(), 10);
        }
    }

    #[test]
    fn artists_that_nothing_ties_together_do_not_share_a_mix() {
        let library: Vec<Song> = ["A", "B", "C", "D", "E"]
            .iter()
            .map(|a| song(&format!("{a}1"), a, &[], &[]))
            .collect();
        // No tags, no sittings, no word from outside: each stands alone and only the latest is kept, being too short to share.
        let all = mixes(&library, &["A", "B", "C", "D", "E"], 50);
        assert_eq!(all, [names(&["A1"])]);
    }

    /// Rock artists `R1` and `R2` with a dozen songs each, and a classical one with `classical` songs.
    fn rock_and_classical(classical: usize) -> Vec<Song> {
        let mut library = vec![];
        for (artist, genre, count) in [
            ("R1", "Rock", 12),
            ("R2", "Rock", 12),
            ("Chopin", "Classical", classical),
        ] {
            for i in 0..count {
                library.push(song(&format!("{artist}-{i}"), artist, &[genre], &[]));
            }
        }
        library
    }

    #[test]
    fn a_recent_artist_that_fits_none_of_the_others_is_not_brought_into_their_mix() {
        let all = mixes(&rock_and_classical(6), &["R1", "Chopin", "R2"], 20);
        assert_eq!(all.len(), 1, "Chopin alone is too short for a mix: {all:?}");
        assert!(all[0].iter().all(|id| id.starts_with('R')), "{all:?}");
        assert_eq!(all[0].len(), 20);
    }

    #[test]
    fn an_artist_with_enough_songs_of_its_own_gets_its_own_mix() {
        let all = mixes(&rock_and_classical(12), &["R1", "Chopin", "R2"], 10);
        assert_eq!(all.len(), 2, "{all:?}");
        assert!(all[0].iter().all(|id| id.starts_with('R')), "{all:?}");
        assert!(all[1].iter().all(|id| id.starts_with('C')), "{all:?}");
    }

    #[test]
    fn what_was_heard_in_the_same_sittings_brings_artists_without_tags_into_one_mix() {
        let library: Vec<Song> = ["A", "B", "C"]
            .iter()
            .flat_map(|a| (0..12).map(move |i| song(&format!("{a}{i}"), a, &[], &[])))
            .collect();
        let mut played = vec![];
        for i in 0..4 {
            let t = i * 10 * 3600;
            played.extend([heard("A", t), heard("B", t + 60)]);
        }
        let all = mixes_with_plays(&library, &["A", "C", "B"], &played);
        assert_eq!(all.len(), 2, "{all:?}");
        let artists = |mix: &Vec<String>| -> BTreeSet<char> {
            mix.iter().map(|id| id.chars().next().unwrap()).collect()
        };
        assert_eq!(artists(&all[0]), ['A', 'B'].into(), "{all:?}");
        assert_eq!(artists(&all[1]), ['C'].into(), "{all:?}");
    }

    #[test]
    fn no_artist_takes_more_than_a_fifth_of_a_mix_while_others_can_fill_it() {
        let mut library = vec![];
        for (artist, count) in [("Big", 30), ("Small", 3), ("C", 10), ("D", 10), ("E", 10)] {
            for i in 0..count {
                library.push(song(&format!("{artist}-{i}"), artist, &["Rock"], &[]));
            }
        }
        let mix = &mixes(&library, &["Big", "Small"], 10)[0];
        let from = |artist: &str| mix.iter().filter(|id| id.starts_with(artist)).count();
        assert_eq!(mix.len(), 10, "{mix:?}");
        assert_eq!((from("Big"), from("Small")), (2, 2), "{mix:?}");
    }

    #[test]
    fn the_share_of_an_artist_is_lifted_when_nobody_else_can_fill_the_mix() {
        let mut library = vec![];
        for i in 0..30 {
            library.push(song(&format!("Big-{i}"), "Big", &["Rock"], &[]));
        }
        library.push(song("Other-0", "Other", &["Rock"], &[]));
        let mix = &mixes(&library, &["Big"], 10)[0];
        assert_eq!(mix.len(), 10, "{mix:?}");
        assert!(mix.contains(&"Other-0".to_string()), "{mix:?}");
    }

    #[test]
    fn no_song_is_in_two_mixes_or_twice() {
        let mut library = rock_and_jazz();
        library.extend(sample().into_iter().filter(|s| s.artist != "Band A"));
        library.push(library[0].clone()); // the same id twice
        let all = mixes(&library, &["J1", "R1", "R2", "J2"], 50);
        let flat: Vec<&String> = all.iter().flatten().collect();
        let unique: BTreeSet<&&String> = flat.iter().collect();
        assert_eq!(flat.len(), unique.len(), "{all:?}");
    }

    #[test]
    fn the_order_of_the_library_does_not_change_the_result() {
        let recent = ["J1", "R1", "R2", "J2"];
        let mut library = rock_and_jazz();
        library.extend(sample().into_iter().filter(|s| s.artist != "Band A"));
        let mut backward = library.clone();
        backward.reverse();
        assert_eq!(mixes(&library, &recent, 7), mixes(&backward, &recent, 7));
    }

    /// A play of the first song of `artist` (as `sample()` ids them) at `at` seconds.
    fn heard(artist: &str, at: i64) -> Play {
        Play {
            song: format!("{artist}-heard"),
            artists: vec![artist.into()],
            at,
            ..Play::default()
        }
    }

    fn mixes_with_plays(library: &[Song], recent: &[&str], played: &[Play]) -> Vec<Vec<String>> {
        recent_artist_mixes(
            library,
            &names(recent),
            played,
            &Decisions::default(),
            &Enrichment::default(),
            50,
        )
    }

    #[test]
    fn an_artist_heard_in_the_same_sittings_leads_the_similar_songs_even_without_tags() {
        // Neither candidate has a tag, so tags alone choose nothing; the one heard beside Band A is chosen.
        let library = vec![
            song("a1", "Band A", &["Rock"], &[]),
            song("n1", "Neighbour", &[], &[]),
            song("s1", "Stranger", &[], &[]),
        ];
        assert_eq!(
            mixes(&library, &["Band A"], 5)[0],
            ["a1"],
            "nothing to go by"
        );
        let played = [
            heard("Band A", 0),
            heard("Neighbour", 300),
            heard("Stranger", 100 * 3600),
            heard("Band A", 200 * 3600),
        ];
        assert_eq!(
            mixes_with_plays(&library, &["Band A"], &played)[0],
            ["a1", "n1"],
            "Stranger was heard in another sitting"
        );
    }

    #[test]
    fn co_listening_orders_artists_by_how_often_they_were_heard_together() {
        let library = vec![
            song("a1", "Band A", &[], &[]),
            song("o1", "Often", &[], &[]),
            song("e1", "Once", &[], &[]),
        ];
        // Often shares three of Band A's three sittings; Once shares one.
        let mut played = vec![];
        for (i, other) in [("Often", "Once"), ("Often", "Nobody"), ("Often", "Nobody")]
            .iter()
            .enumerate()
        {
            let t = i as i64 * 10 * 3600;
            played.extend([heard("Band A", t), heard(other.0, t + 60)]);
            if i == 0 {
                played.push(heard(other.1, t + 120));
            }
        }
        let mix = &mixes_with_plays(&library, &["Band A"], &played)[0];
        assert_eq!(mix, &["a1", "o1", "e1"], "{mix:?}");
    }

    #[test]
    fn an_artist_that_is_heard_alone_most_of_the_time_is_not_close() {
        // Artist 0 is the group. Artist 1 shares one of its four sittings; artist 2 is only ever heard beside it.
        let sittings: Vec<BTreeSet<usize>> =
            vec![[0, 1, 2].into(), [1].into(), [1].into(), [1].into()];
        let score = co_listening(&sittings, &[0]);
        assert!((score[&1] - 0.5).abs() < 1e-9, "1 / sqrt(1 * 4): {score:?}");
        assert!((score[&2] - 1.0).abs() < 1e-9, "1 / sqrt(1 * 1): {score:?}");
        assert!(
            !score.contains_key(&0),
            "the group is not its own neighbour"
        );
    }

    #[test]
    fn plays_of_artists_that_are_not_in_the_library_change_nothing() {
        let library = vec![
            song("a1", "Band A", &["Rock"], &[]),
            song("r1", "R", &["Rock"], &[]),
        ];
        let played = [heard("Band A", 0), heard("Ghost", 60)];
        assert_eq!(
            mixes_with_plays(&library, &["Band A"], &played),
            mixes(&library, &["Band A"], 50)
        );
    }

    #[test]
    fn co_listening_adds_to_the_tags_and_does_not_replace_them() {
        // Both candidates are heard once beside Band A; the one that also shares a tag comes first.
        let library = vec![
            song("a1", "Band A", &["Rock"], &[]),
            song("n1", "Neighbour", &[], &[]),
            song("t1", "Tagged", &["Rock"], &[]),
        ];
        let played = [
            heard("Band A", 0),
            heard("Neighbour", 60),
            heard("Tagged", 120),
        ];
        assert_eq!(
            mixes_with_plays(&library, &["Band A"], &played)[0],
            ["a1", "t1", "n1"]
        );
    }

    #[test]
    fn what_was_heard_beside_the_artist_does_not_bring_in_a_song_of_another_style() {
        // Jazzman is heard in every sitting of Band A, but its tags say it is not rock: the sittings say nothing of style.
        let library = vec![
            song("a1", "Band A", &["Rock"], &[]),
            song("j1", "Jazzman", &["Jazz"], &[]),
            song("t1", "Tagged", &["Rock"], &[]),
        ];
        let mut played = vec![];
        for i in 0..4 {
            let t = i * 10 * 3600;
            played.extend([heard("Band A", t), heard("Jazzman", t + 60)]);
        }
        assert_eq!(
            mixes_with_plays(&library, &["Band A"], &played)[0],
            ["a1", "t1"]
        );
    }

    #[test]
    fn a_song_without_genres_falls_back_to_its_genre_field() {
        let mut old = song("old", "Old Copy", &[], &[]);
        old.genre = Some("Rock".into());
        let library = vec![song("a1", "Band A", &["Rock"], &[]), old];
        assert_eq!(mixes(&library, &["Band A"], 5)[0], ["a1", "old"]);
    }

    /// An artist, its crowd tags and its close artists.
    type Entry<'a> = (&'a str, &'a [(&'a str, f64)], &'a [(&'a str, f64)]);

    fn known(entries: &[Entry]) -> Enrichment {
        use crate::enrich::ArtistInfo;
        let mut known = Enrichment::default();
        for (artist, tags, similar) in entries {
            known.insert(
                artist,
                ArtistInfo {
                    tags: tags.iter().map(|(t, w)| (t.to_string(), *w)).collect(),
                    similar: similar.iter().map(|(a, m)| (a.to_string(), *m)).collect(),
                    top: None,
                },
            );
        }
        known
    }

    fn mixes_enriched(library: &[Song], recent: &[&str], known: &Enrichment) -> Vec<Vec<String>> {
        recent_artist_mixes(
            library,
            &names(recent),
            &[],
            &Decisions::default(),
            known,
            50,
        )
    }

    #[test]
    fn the_crowd_tags_of_an_artist_bring_its_songs_close_when_the_files_carry_no_tags() {
        let library = vec![
            song("a1", "Portishead", &[], &[]),
            song("m1", "Massive Attack", &[], &[]),
            song("r1", "Radiohead", &[], &[]),
        ];
        assert_eq!(
            mixes(&library, &["Portishead"], 5)[0],
            ["a1"],
            "nothing to go by"
        );
        let known = known(&[
            ("Portishead", &[("trip hop", 1.0)], &[]),
            ("Massive Attack", &[("trip hop", 0.9)], &[]),
            ("Radiohead", &[("rock", 1.0)], &[]),
        ]);
        assert_eq!(
            mixes_enriched(&library, &["Portishead"], &known)[0],
            ["a1", "m1"]
        );
    }

    #[test]
    fn a_weak_crowd_tag_is_not_a_tag_of_the_songs() {
        let library = vec![
            song("a1", "Portishead", &[], &[]),
            song("b1", "British Band", &[], &[]),
        ];
        let known = known(&[
            ("Portishead", &[("trip hop", 1.0), ("british", 0.1)], &[]),
            ("British Band", &[("british", 1.0)], &[]),
        ]);
        assert_eq!(mixes_enriched(&library, &["Portishead"], &known)[0], ["a1"]);
    }

    #[test]
    fn an_artist_the_services_call_close_makes_its_songs_likelier_whatever_its_tags() {
        let library = vec![
            song("a1", "Band A", &["Rock"], &[]),
            song("c1", "Close", &["Jazz"], &[]),
            song("s1", "Stranger", &["Folk"], &[]),
        ];
        let known = known(&[("Band A", &[], &[("close", 1.0), ("Band A", 1.0)])]);
        assert_eq!(
            mixes_enriched(&library, &["Band A"], &known)[0],
            ["a1", "c1"]
        );
    }

    #[test]
    fn what_the_services_say_adds_to_the_tags_and_does_not_replace_them() {
        let library = vec![
            song("a1", "Band A", &["Rock"], &[]),
            song("c1", "Close", &["Jazz"], &[]),
            song("t1", "Tagged", &["Rock"], &[]),
        ];
        let known = known(&[("Band A", &[], &[("Close", 1.0)])]);
        // Tagged shares a tag (cosine 1); Close is only called close (0.5 * 1.0, the least a similar song may score).
        assert_eq!(
            mixes_enriched(&library, &["Band A"], &known)[0],
            ["a1", "t1", "c1"]
        );
    }

    #[test]
    fn a_song_that_barely_fits_is_left_out_and_the_mix_stays_short() {
        let library = vec![
            song("a1", "Band A", &["Rock"], &[]),
            song("c1", "Close", &["Jazz"], &[]),
        ];
        // 0.5 * 0.4 is under the floor for a similar song.
        let known = known(&[("Band A", &[], &[("Close", 0.4)])]);
        assert_eq!(mixes_enriched(&library, &["Band A"], &known)[0], ["a1"]);
    }

    #[test]
    fn a_mix_of_artists_that_fit_scores_higher_than_a_mix_of_artists_that_do_not() {
        let library = rock_and_classical(12);
        let ids = |artists: &[&str]| -> Vec<String> {
            library
                .iter()
                .filter(|s| artists.contains(&s.artist.as_str()))
                .map(|s| s.id.clone())
                .collect()
        };
        let fit = |mix: &[String]| {
            mix_fit(
                &library,
                mix,
                &[],
                &Decisions::default(),
                &Enrichment::default(),
            )
        };
        let together = fit(&ids(&["R1", "R2"])).unwrap();
        let apart = fit(&ids(&["R1", "Chopin"])).unwrap();
        assert!(together > 0.9 && apart < 0.1, "{together} {apart}");
        assert_eq!(fit(&ids(&["R1"])), None, "one artist has nobody to fit");
    }

    #[test]
    fn each_song_says_whether_it_is_of_a_played_artist_and_what_brings_a_similar_one_in() {
        let mut library = vec![];
        for i in 0..3 {
            library.push(song(&format!("a{i}"), "Band A", &["Rock"], &[]));
            library.push(song(&format!("t{i}"), "Tagged", &["Rock"], &[]));
        }
        let explained = recent_artist_mixes_explained(
            &library,
            &names(&["Band A"]),
            &[],
            &Decisions::default(),
            &Enrichment::default(),
            5,
            &Tuning::default(),
        );
        let (base, similar): (Vec<&Chosen>, Vec<&Chosen>) =
            explained[0].iter().partition(|c| c.base);
        assert_eq!((base.len(), similar.len()), (2, 3), "{explained:?}");
        assert!(base.iter().all(|c| c.tags == 0.0 && c.together == 0.0));
        assert!(
            similar.iter().all(|c| c.tags > 0.99 && c.close == 0.0),
            "{similar:?}"
        );
    }

    #[test]
    fn a_higher_floor_leaves_the_mix_shorter_and_a_higher_bar_splits_the_artists() {
        let mut library = rock_and_classical(12);
        // Folk shares one of its two tags with Rock: a weak match, in with a low floor and out with a high one.
        for i in 0..12 {
            let mut folk = song(&format!("F-{i}"), "Folk", &["Rock", "Acoustic"], &[]);
            folk.moods = vec![];
            library.push(folk);
        }
        let run = |tuning: Tuning, recent: &[&str]| {
            recent_artist_mixes_explained(
                &library,
                &names(recent),
                &[],
                &Decisions::default(),
                &Enrichment::default(),
                50,
                &tuning,
            )
        };
        let lenient = run(
            Tuning {
                min_fit: 0.1,
                ..Tuning::default()
            },
            &["R1"],
        );
        let strict = run(
            Tuning {
                min_fit: 0.95,
                ..Tuning::default()
            },
            &["R1"],
        );
        assert!(
            strict[0].len() < lenient[0].len(),
            "{} {}",
            strict[0].len(),
            lenient[0].len()
        );
        assert!(strict[0].iter().all(|c| c.base || c.tags >= 0.95));
        let together = run(Tuning::default(), &["R1", "R2"]);
        let apart = run(
            Tuning {
                min_aligned: 1.5,
                ..Tuning::default()
            },
            &["R1", "R2"],
        );
        assert_eq!((together.len(), apart.len()), (1, 2));
    }

    #[test]
    fn the_number_of_mixes_follows_the_groups_of_artists_up_to_the_most_asked_for() {
        let mut library = rock_and_classical(12);
        for i in 0..12 {
            library.push(song(&format!("J1-{i}"), "J1", &["Jazz"], &[]));
        }
        let run = |max_mixes: usize| {
            recent_artist_mixes_explained(
                &library,
                &names(&["R1", "Chopin", "J1"]),
                &[],
                &Decisions::default(),
                &Enrichment::default(),
                10,
                &Tuning {
                    max_mixes,
                    ..Tuning::default()
                },
            )
        };
        assert_eq!(run(Tuning::default().max_mixes).len(), 3);
        assert_eq!(
            run(2).len(),
            2,
            "the groups of the latest artists come first"
        );
        assert_eq!(run(1).len(), 1);
    }

    #[test]
    fn a_track_number_in_front_of_a_title_is_not_part_of_the_song() {
        let key = |title: &str| {
            let mut s = song("x", "Los Fabulosos Cadillacs", &[], &[]);
            s.title = title.into();
            song_key(&s)
        };
        assert_eq!(key("04 Siguiendo la Luna"), key("Siguiendo La Luna"));
        assert_eq!(
            key("03  Manuel Santillan, El León"),
            key("Manuel Santillan, El León")
        );
        assert_ne!(
            key("1979"),
            key(""),
            "a title that is only a number stays whole"
        );
        assert_ne!(
            key("1999 Remix"),
            key("Remix"),
            "four digits are a year, not a track number"
        );
    }

    #[test]
    fn the_versions_of_a_song_are_told_by_what_follows_its_title() {
        let key = |title: &str| {
            let mut s = song("x", "Maroon 5", &[], &[]);
            s.title = title.into();
            song_key(&s)
        };
        assert_eq!(key("Tangled - Demo"), key("Tangled"));
        assert_eq!(key("Maps - Live In Manchester"), key("maps"));
        assert_eq!(key("Magic Dance (Danny S Remix)"), key("Magic Dance"));
        assert_ne!(key("Maps"), key("Tangled"));
        assert_ne!(key("Maps"), {
            let mut s = song("x", "Other Band", &[], &[]);
            s.title = "Maps".into();
            song_key(&s)
        });
    }

    #[test]
    fn a_mix_does_not_fill_itself_with_the_versions_of_one_song() {
        let version = |id: &str, title: &str| {
            let mut s = song(id, "Band A", &[], &[]);
            s.title = title.into();
            s
        };
        let library = vec![
            version("m1", "Maps"),
            version("m2", "Maps - Live"),
            version("m3", "Maps - Demo"),
            version("t1", "Tangled"),
            version("s1", "Shiver"),
        ];
        // Equal scores: without the penalty the three versions of Maps would come first, in the order of their ids.
        let mix = &mixes(&library, &["Band A"], 3)[0];
        assert_eq!(mix, &["m1", "s1", "t1"], "{mix:?}");
    }
}
