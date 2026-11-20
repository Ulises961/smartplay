//! Mixes from the artists a user played lately.
//!
//! Each mix is built on those artists (the base, 60%) and widened with songs by other artists whose tags are close to
//! the base (the similar part, 40%). Tags are the genres and moods of the songs; a tag shared by few songs of the
//! library says more than one shared by most of them, so rare tags weigh more. What the user played together says
//! more still: an artist heard in the same sittings as the base artists is close to them, whatever its tags.
//!
//! Pure functions over a snapshot of the library, like `taste`: nothing is read from a server and nothing is kept.

use std::collections::{HashMap, HashSet};

use crate::enrich::Enrichment;
use crate::identity::{resolve, Decisions, Track};
use crate::normalize::fold;
use crate::plays::{self, Play};
use crate::taste::Song;

/// Of every [`ROUND`] songs of a mix, this many come from the artists played and the rest are similar songs: 60%.
const BASE_PER_ROUND: usize = 3;
const ROUND: usize = 5;
/// A mix is not worth making from fewer artists than this, so fewer than twice as many recent artists give one mix.
const MIN_ARTISTS_PER_MIX: usize = 2;

/// How much an artist played in the same sittings as the base adds to the score of its songs, next to the tags' cosine
/// (both lie between 0 and 1).
const CO_LISTENING_WEIGHT: f64 = 1.0;

/// How much an artist that outside services call close to the base artists adds to the score of its songs, times how close
/// they say it is (between 0 and 1).
const SIMILAR_WEIGHT: f64 = 0.5;
/// An artist's crowd tag counts as a tag of each of its songs when it is at least this strong (between 0 and 1): the weak
/// ones describe a few listeners' habits and not the artist.
const MIN_CROWD_TAG: f64 = 0.3;

type Profile = HashMap<String, f64>;

/// At most two mixes of up to `size` songs each, built from `recent` (artist names, most recent first).
///
/// The recent artists are told apart by identity, not spelling. They are split in two groups by how close their tags
/// are (or in halves by recency when the songs carry no tags), and each group gets its own mix. A song is in at most one
/// mix. When a mix cannot fill one of its two parts, the other part fills the gap; a mix with no songs is left out.
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
    if size == 0 {
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
    let clusters = resolve(&tracks, decisions).clusters;
    let mut artist_of_name: HashMap<String, usize> = HashMap::new();
    for (i, cluster) in clusters.iter().enumerate() {
        for spelling in &cluster.spellings {
            artist_of_name.insert(fold(spelling), i);
        }
    }
    let artist = |name: &str| artist_of_name.get(&fold(name)).copied();

    let mut recent_artists: Vec<usize> = vec![];
    for a in recent.iter().filter_map(|name| artist(name)) {
        if !recent_artists.contains(&a) {
            recent_artists.push(a);
        }
    }
    if recent_artists.is_empty() {
        return vec![];
    }

    let tags: Vec<HashSet<String>> = library.iter().map(|s| tags_of(s, enrichment)).collect();
    let weight = idf(&tags);
    let songs: Vec<(&Song, Option<usize>)> = library
        .iter()
        .map(|s| (s, artist(&s.artist).filter(|_| !s.artist.trim().is_empty())))
        .collect();

    let mut profiles: HashMap<usize, Profile> = HashMap::new();
    for ((_, a), song_tags) in songs.iter().zip(&tags) {
        if let Some(a) = a.filter(|a| recent_artists.contains(a)) {
            let profile = profiles.entry(a).or_default();
            for t in song_tags {
                *profile.entry(t.clone()).or_default() += weight[t];
            }
        }
    }

    // The artists of the library heard in each sitting (the main artist of each play).
    let sittings: Vec<HashSet<usize>> = plays::sessions(played)
        .iter()
        .map(|sitting| {
            sitting
                .iter()
                .filter_map(|p| p.artists.iter().map(|a| a.trim()).find(|a| !a.is_empty()))
                .filter_map(artist)
                .collect()
        })
        .collect();

    let mut used: HashSet<&str> = HashSet::new();
    let mut mixes = vec![];
    for group in split(&recent_artists, &profiles) {
        let mut profile = Profile::new();
        for a in &group {
            for (t, v) in profiles.get(a).into_iter().flatten() {
                *profile.entry(t.clone()).or_default() += v;
            }
        }
        let together = co_listening(&sittings, &group);
        let close = close_artists(&group, &clusters, enrichment, &artist);
        let rank = |a: usize| recent_artists.iter().position(|&r| r == a).unwrap_or(0);

        let mut seen: HashSet<&str> = HashSet::new();
        let (mut base, mut similar) = (vec![], vec![]);
        for ((song, a), song_tags) in songs.iter().zip(&tags) {
            if used.contains(song.id.as_str()) || !seen.insert(song.id.as_str()) {
                continue;
            }
            match a {
                Some(a) if group.contains(a) => base.push(Candidate {
                    id: &song.id,
                    artist: Some(*a),
                    // Between 1 and 2: the latest artist leads, but not by so much that it crowds the others out.
                    score: 1.0 + 1.0 / (1 + rank(*a)) as f64,
                }),
                Some(a) if recent_artists.contains(a) => {}
                _ => {
                    let score = cosine(song_tags, &weight, &profile)
                        + CO_LISTENING_WEIGHT
                            * a.and_then(|a| together.get(&a)).copied().unwrap_or(0.0)
                        + SIMILAR_WEIGHT * a.and_then(|a| close.get(&a)).copied().unwrap_or(0.0);
                    if score > 0.0 {
                        similar.push(Candidate {
                            id: &song.id,
                            artist: *a,
                            score,
                        });
                    }
                }
            }
        }
        let base = pick(base, size);
        let similar = pick(similar, size);

        let want_base = (size * BASE_PER_ROUND).div_ceil(ROUND);
        let mut take_base = base.len().min(want_base);
        let take_similar = similar.len().min(size - take_base);
        take_base = base.len().min(size - take_similar);

        let list = interleave(&base[..take_base], &similar[..take_similar]);
        used.extend(
            library
                .iter()
                .map(|s| s.id.as_str())
                .filter(|id| list.iter().any(|l| l == id)),
        );
        if !list.is_empty() {
            mixes.push(list);
        }
    }
    mixes
}

/// How often each other artist was heard in the sittings of `group`, between 0 and 1: the sittings holding both, over
/// the geometric mean of the sittings holding the group and those holding the artist. An artist always heard alone
/// scores 0, and one the group's artists are heard with every time scores 1.
fn co_listening(sittings: &[HashSet<usize>], group: &[usize]) -> HashMap<usize, f64> {
    let with_group = |s: &&HashSet<usize>| group.iter().any(|a| s.contains(a));
    let group_sittings = sittings.iter().filter(with_group).count();
    let mut both: HashMap<usize, usize> = HashMap::new();
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
) -> HashMap<usize, f64> {
    let mut close: HashMap<usize, f64> = HashMap::new();
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
fn tags_of(song: &Song, enrichment: &Enrichment) -> HashSet<String> {
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
fn idf(tags: &[HashSet<String>]) -> HashMap<String, f64> {
    let mut count: HashMap<&str, usize> = HashMap::new();
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
fn cosine(song: &HashSet<String>, weight: &HashMap<String, f64>, profile: &Profile) -> f64 {
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

/// One group, or two when there are enough artists: the two least alike (by tags) are the seeds and every other artist
/// joins the seed it is closer to. Without tags to tell them apart, or if that leaves a group too small, the artists
/// are split in halves by recency instead. Each group keeps the recency order.
fn split(recent: &[usize], profiles: &HashMap<usize, Profile>) -> Vec<Vec<usize>> {
    if recent.len() < 2 * MIN_ARTISTS_PER_MIX {
        return vec![recent.to_vec()];
    }
    let halves = || {
        let (first, second) = recent.split_at(recent.len().div_ceil(2));
        vec![first.to_vec(), second.to_vec()]
    };
    let tagged: Vec<usize> = recent
        .iter()
        .copied()
        .filter(|a| profiles.get(a).is_some_and(|p| !p.is_empty()))
        .collect();
    let mut seeds: Option<(usize, usize, f64)> = None;
    for (i, &x) in tagged.iter().enumerate() {
        for &y in &tagged[i + 1..] {
            let alike = cosine_profiles(&profiles[&x], &profiles[&y]);
            if seeds.is_none_or(|(_, _, best)| alike < best) {
                seeds = Some((x, y, alike));
            }
        }
    }
    let Some((x, y, _)) = seeds else {
        return halves();
    };
    let empty = Profile::new();
    let profile = |a: &usize| profiles.get(a).unwrap_or(&empty);
    let (mut first, mut second) = (vec![], vec![]);
    for &a in recent {
        let to_x = cosine_profiles(profile(&a), profile(&x));
        let to_y = cosine_profiles(profile(&a), profile(&y));
        if a == y || (a != x && to_y > to_x) {
            second.push(a);
        } else {
            first.push(a);
        }
    }
    if first.len() < MIN_ARTISTS_PER_MIX || second.len() < MIN_ARTISTS_PER_MIX {
        return halves();
    }
    vec![first, second]
}

struct Candidate<'a> {
    id: &'a str,
    artist: Option<usize>,
    score: f64,
}

/// Up to `limit` ids, best first. Each pick lowers the next song of the same artist (its score is divided by one plus
/// the songs already picked from that artist), so the list mixes artists instead of playing one out.
fn pick(mut candidates: Vec<Candidate>, limit: usize) -> Vec<String> {
    let mut picked_from: HashMap<usize, usize> = HashMap::new();
    let mut list = vec![];
    while list.len() < limit && !candidates.is_empty() {
        let weight = |c: &Candidate| {
            let taken = c
                .artist
                .map_or(0, |a| picked_from.get(&a).copied().unwrap_or(0));
            c.score / (1 + taken) as f64
        };
        let best = (0..candidates.len())
            .max_by(|&x, &y| {
                weight(&candidates[x])
                    .total_cmp(&weight(&candidates[y]))
                    .then_with(|| candidates[y].id.cmp(candidates[x].id))
            })
            .expect("candidates is not empty");
        let chosen = candidates.swap_remove(best);
        if let Some(a) = chosen.artist {
            *picked_from.entry(a).or_default() += 1;
        }
        list.push(chosen.id.to_string());
    }
    list
}

/// Rounds of [`BASE_PER_ROUND`] base songs then the rest similar ones; whichever runs out first, the other carries on.
fn interleave(base: &[String], similar: &[String]) -> Vec<String> {
    let (mut base, mut similar) = (base.iter(), similar.iter());
    let mut list = vec![];
    loop {
        let before = list.len();
        list.extend(base.by_ref().take(BASE_PER_ROUND).cloned());
        list.extend(similar.by_ref().take(ROUND - BASE_PER_ROUND).cloned());
        if list.len() == before {
            return list;
        }
    }
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
    fn three_of_every_five_songs_are_the_artists_played_and_two_are_similar() {
        let mix = &mixes(&sample(), &["Band A"], 5)[0];
        let from_base = |id: &String| id.starts_with('a');
        assert_eq!(mix.len(), 5);
        assert_eq!(
            mix.iter().map(from_base).collect::<Vec<_>>(),
            [true, true, true, false, false],
            "{mix:?}"
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
        let mix = &mixes(&library, &["Fan"], 5)[0];
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
            library.push(song(&format!("a{i}"), "Band A", &[], &[]));
            library.push(song(&format!("b{i}"), "Band B", &[], &[]));
        }
        let mix = &mixes(&library, &["Band A", "Band B"], 4)[0];
        let a = mix.iter().filter(|id| id.starts_with('a')).count();
        assert_eq!((mix.len(), a), (4, 2), "{mix:?}");
    }

    #[test]
    fn the_more_recent_artist_comes_first() {
        let library = vec![
            song("a1", "Band A", &[], &[]),
            song("b1", "Band B", &[], &[]),
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
            for i in 0..3 {
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
            assert_eq!(mix.len(), 6);
        }
    }

    #[test]
    fn without_tags_the_artists_are_split_in_halves_by_recency() {
        let library: Vec<Song> = ["A", "B", "C", "D", "E"]
            .iter()
            .map(|a| song(&format!("{a}1"), a, &[], &[]))
            .collect();
        let all = mixes(&library, &["A", "B", "C", "D", "E"], 50);
        assert_eq!(all, [names(&["A1", "B1", "C1"]), names(&["D1", "E1"])]);
    }

    #[test]
    fn too_few_recent_artists_make_one_mix() {
        let all = mixes(&rock_and_jazz(), &["R1", "J1", "R2"], 50);
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn no_song_is_in_two_mixes_or_twice() {
        let mut library = rock_and_jazz();
        library.extend(sample().into_iter().filter(|s| s.artist != "Band A"));
        library.push(library[0].clone()); // the same id twice
        let all = mixes(&library, &["J1", "R1", "R2", "J2"], 50);
        let flat: Vec<&String> = all.iter().flatten().collect();
        let unique: HashSet<&&String> = flat.iter().collect();
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
        // Neither candidate shares a tag with Band A, so tags alone choose nothing; the one heard beside it is chosen.
        let library = vec![
            song("a1", "Band A", &["Rock"], &[]),
            song("n1", "Neighbour", &["Jazz"], &[]),
            song("s1", "Stranger", &["Folk"], &[]),
        ];
        assert_eq!(
            mixes(&library, &["Band A"], 5)[0],
            ["a1"],
            "no tags in common"
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
        let sittings: Vec<HashSet<usize>> =
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
            song("n1", "Neighbour", &["Jazz"], &[]),
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
        let known = known(&[("Band A", &[], &[("close", 0.8), ("Band A", 1.0)])]);
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
        let known = known(&[("Band A", &[], &[("Close", 0.4)])]);
        // Tagged shares a tag (cosine 1); Close is only called close (0.5 * 0.4): Tagged comes first.
        assert_eq!(
            mixes_enriched(&library, &["Band A"], &known)[0],
            ["a1", "t1", "c1"]
        );
    }
}
