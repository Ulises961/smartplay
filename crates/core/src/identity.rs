//! Artist identity: which spellings in a library are one artist.
//!
//! A library names one band many ways (`Patricio Rey y sus Redonditos de Ricota`, `Los Redonditos de Ricota`,
//! `Los Redondos`). Text folding alone joins only the spellings that differ in case, accents or a joiner. The rest
//! needs evidence, and a wrong merge is worse than a missed one: it poisons taste and playlists. So the evidence is
//! layered by strength:
//!
//! 1. the user said they are the same, or different (always wins);
//! 2. the same MusicBrainz artist id (the strongest outside evidence; nicknames resolve this way);
//! 3. a shared album, or three shared songs, together with overlapping names;
//! 4. overlapping names alone: only a suggestion for the user to review, never merged on its own.
//!
//! The result is an overlay. No name in the library is changed.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::normalize::{content_tokens, edit_distance, fold, strip_remaster};

/// Names must overlap at least this much, besides sharing an album or songs, to be merged.
const MERGE_LIKENESS: f64 = 0.6;
/// Names that overlap at least this much are worth showing to the user.
const SUGGEST_LIKENESS: f64 = 0.5;
/// Songs two spellings must share, with no shared album, to count as evidence.
const MIN_SHARED_SONGS: usize = 3;

/// One song of the library, as the server holds it.
pub struct Track {
    pub artist: String,
    pub album: String,
    pub title: String,
    /// The MusicBrainz id of the artist, when a tag or a lookup supplied one.
    pub artist_mbid: Option<String>,
}

/// What the user decided about pairs of artist names. Decisions beat every other evidence.
#[derive(Default)]
pub struct Decisions {
    same: BTreeSet<(String, String)>,
    different: BTreeSet<(String, String)>,
}

impl Decisions {
    pub fn same(&mut self, a: &str, b: &str) {
        self.same.insert(pair(a, b));
    }

    pub fn different(&mut self, a: &str, b: &str) {
        self.different.insert(pair(a, b));
    }
}

fn pair(a: &str, b: &str) -> (String, String) {
    let (a, b) = (fold(a), fold(b));
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Spellings of one artist that fold to the same text.
struct Spelling {
    key: String,
    shown: BTreeMap<String, usize>,
    tracks: usize,
    albums: BTreeSet<String>,
    titles: BTreeSet<String>,
    mbids: BTreeSet<String>,
    tokens: Vec<String>,
}

impl Spelling {
    /// The form most tracks use; the longer one on a tie.
    fn name(&self) -> String {
        self.shown
            .iter()
            .max_by_key(|(name, count)| (**count, name.len()))
            .map(|(name, _)| name.clone())
            .unwrap_or_default()
    }
}

/// Spellings taken to be one artist.
#[derive(Debug, PartialEq)]
pub struct Cluster {
    /// The spelling most tracks use.
    pub name: String,
    /// Every distinct spelling as it appears in the library, most used first.
    pub spellings: Vec<String>,
    pub tracks: usize,
    /// Why the spellings were joined; empty when the cluster is a single folded spelling.
    pub reasons: Vec<&'static str>,
}

/// Two clusters that may be one artist, for the user to confirm or reject.
#[derive(Debug, PartialEq)]
pub struct Suggestion {
    pub a: String,
    pub b: String,
    pub score: f64,
    pub reason: String,
}

#[derive(Debug, Default, PartialEq)]
pub struct Resolution {
    pub clusters: Vec<Cluster>,
    pub suggestions: Vec<Suggestion>,
}

/// How much of the shorter name's words the longer one holds, allowing a typo in a word.
/// A one-word name counts half: `Queen` is inside `Queen Latifah` and is not her.
fn likeness(a: &[String], b: &[String]) -> f64 {
    let numbers = |t: &[String]| {
        t.iter()
            .filter(|w| is_number(w))
            .cloned()
            .collect::<BTreeSet<_>>()
    };
    if a.is_empty() || b.is_empty() || numbers(a) != numbers(b) {
        return 0.0;
    }
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let mut free: Vec<&String> = large.iter().collect();
    let mut matched = 0;
    for word in small {
        if let Some(at) = free.iter().position(|other| same_word(word, other)) {
            free.remove(at);
            matched += 1;
        }
    }
    let held = matched as f64 / small.len() as f64;
    if small.len() >= 2 {
        held
    } else {
        held * 0.5
    }
}

fn is_number(word: &str) -> bool {
    word.chars().all(|c| c.is_ascii_digit())
}

/// Equal, or a typo apart: one letter for words of four or more, two for seven or more.
/// Numbers must be equal (`Maroon 5` is not `Maroon 6`).
fn same_word(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    if is_number(a) || is_number(b) {
        return false;
    }
    let shortest = a.chars().count().min(b.chars().count());
    let distance = edit_distance(a, b);
    (shortest >= 4 && distance <= 1) || (shortest >= 7 && distance <= 2)
}

/// Spellings whose names could overlap: they share the first four letters of a content word.
fn candidate_pairs(spellings: &[Spelling]) -> BTreeSet<(usize, usize)> {
    let mut buckets: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, s) in spellings.iter().enumerate() {
        for token in &s.tokens {
            buckets
                .entry(token.chars().take(4).collect())
                .or_default()
                .push(i);
        }
    }
    let mut pairs = BTreeSet::new();
    for members in buckets.values() {
        for (n, &a) in members.iter().enumerate() {
            for &b in &members[n + 1..] {
                if a != b {
                    pairs.insert((a.min(b), a.max(b)));
                }
            }
        }
    }
    pairs
}

/// Groups of spellings, merged one link at a time.
struct Groups {
    owner: Vec<usize>,
    members: Vec<Vec<usize>>,
    reasons: Vec<BTreeSet<&'static str>>,
}

impl Groups {
    fn new(n: usize) -> Self {
        Self {
            owner: (0..n).collect(),
            members: (0..n).map(|i| vec![i]).collect(),
            reasons: vec![BTreeSet::new(); n],
        }
    }

    fn merge(&mut self, a: usize, b: usize, reason: &'static str) {
        let (ga, gb) = (self.owner[a], self.owner[b]);
        if ga == gb {
            return;
        }
        let (keep, gone) = if self.members[ga].len() >= self.members[gb].len() {
            (ga, gb)
        } else {
            (gb, ga)
        };
        let moved = std::mem::take(&mut self.members[gone]);
        for &m in &moved {
            self.owner[m] = keep;
        }
        self.members[keep].extend(moved);
        let reasons = std::mem::take(&mut self.reasons[gone]);
        self.reasons[keep].extend(reasons);
        self.reasons[keep].insert(reason);
    }
}

/// Decides which spellings in `tracks` are one artist, and which pairs the user should review.
pub fn resolve(tracks: &[Track], decisions: &Decisions) -> Resolution {
    let mut by_key: BTreeMap<String, Spelling> = BTreeMap::new();
    for track in tracks {
        let key = fold(&track.artist);
        if key.is_empty() {
            continue;
        }
        let s = by_key.entry(key.clone()).or_insert_with(|| Spelling {
            tokens: content_tokens(&key),
            key,
            shown: BTreeMap::new(),
            tracks: 0,
            albums: BTreeSet::new(),
            titles: BTreeSet::new(),
            mbids: BTreeSet::new(),
        });
        *s.shown.entry(track.artist.trim().to_string()).or_default() += 1;
        s.tracks += 1;
        let (album, title) = (fold(&track.album), fold(&strip_remaster(&track.title)));
        if !album.is_empty() {
            s.albums.insert(album);
        }
        if !title.is_empty() {
            s.titles.insert(title);
        }
        if let Some(id) = track.artist_mbid.as_deref().filter(|id| !id.is_empty()) {
            s.mbids.insert(id.to_lowercase());
        }
    }
    let spellings: Vec<Spelling> = by_key.into_values().collect();
    let index: HashMap<&str, usize> = spellings
        .iter()
        .enumerate()
        .map(|(i, s)| (s.key.as_str(), i))
        .collect();
    let mut groups = Groups::new(spellings.len());

    // 1. What the user said is the same.
    for (a, b) in &decisions.same {
        if let (Some(&a), Some(&b)) = (index.get(a.as_str()), index.get(b.as_str())) {
            groups.merge(a, b, "you said they are the same");
        }
    }

    let blocked = |groups: &Groups, a: usize, b: usize| {
        groups.members[groups.owner[a]].iter().any(|&x| {
            groups.members[groups.owner[b]].iter().any(|&y| {
                decisions
                    .different
                    .contains(&pair(&spellings[x].key, &spellings[y].key))
            })
        })
    };

    // 2. The same MusicBrainz artist.
    let mut by_mbid: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, s) in spellings.iter().enumerate() {
        for id in &s.mbids {
            by_mbid.entry(id).or_default().push(i);
        }
    }
    for members in by_mbid.values() {
        for &other in &members[1..] {
            if !blocked(&groups, members[0], other) {
                groups.merge(members[0], other, "same MusicBrainz artist");
            }
        }
    }

    // 3 and 4. Library evidence, and what is only worth a suggestion.
    let mut weak: Vec<(usize, usize, f64)> = vec![];
    for (a, b) in candidate_pairs(&spellings) {
        let score = likeness(&spellings[a].tokens, &spellings[b].tokens);
        if score < SUGGEST_LIKENESS {
            continue;
        }
        let shared_albums = spellings[a]
            .albums
            .intersection(&spellings[b].albums)
            .count();
        let shared_songs = spellings[a]
            .titles
            .intersection(&spellings[b].titles)
            .count();
        let reason = if shared_albums >= 1 {
            Some("shares an album and the names overlap")
        } else if shared_songs >= MIN_SHARED_SONGS {
            Some("shares songs and the names overlap")
        } else {
            None
        };
        match reason {
            Some(reason) if score >= MERGE_LIKENESS && !blocked(&groups, a, b) => {
                groups.merge(a, b, reason)
            }
            _ => weak.push((a, b, score)),
        }
    }

    let mut clusters: Vec<Cluster> = vec![];
    let mut cluster_of: HashMap<usize, usize> = HashMap::new();
    for (group, members) in groups
        .members
        .iter()
        .enumerate()
        .filter(|(_, m)| !m.is_empty())
    {
        let mut ordered: Vec<&Spelling> = members.iter().map(|&i| &spellings[i]).collect();
        ordered.sort_by(|x, y| y.tracks.cmp(&x.tracks).then_with(|| x.key.cmp(&y.key)));
        // Every form as written, not one per folded text: the user reviews what the library really holds.
        let mut forms: Vec<(&String, usize)> = ordered
            .iter()
            .flat_map(|s| s.shown.iter().map(|(form, count)| (form, *count)))
            .collect();
        forms.sort_by(|x, y| y.1.cmp(&x.1).then_with(|| x.0.cmp(y.0)));
        cluster_of.insert(group, clusters.len());
        clusters.push(Cluster {
            name: ordered[0].name(),
            spellings: forms.into_iter().map(|(form, _)| form.clone()).collect(),
            tracks: ordered.iter().map(|s| s.tracks).sum(),
            reasons: groups.reasons[group].iter().copied().collect(),
        });
    }

    // The best pair of spellings stands for each pair of clusters.
    let mut best: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    for (a, b, score) in weak {
        if groups.owner[a] == groups.owner[b] || blocked(&groups, a, b) {
            continue;
        }
        let (ca, cb) = (cluster_of[&groups.owner[a]], cluster_of[&groups.owner[b]]);
        let entry = best.entry((ca.min(cb), ca.max(cb))).or_insert(0.0);
        *entry = entry.max(score);
    }
    let mut suggestions: Vec<Suggestion> = best
        .into_iter()
        .map(|((a, b), score)| Suggestion {
            a: clusters[a].name.clone(),
            b: clusters[b].name.clone(),
            score,
            reason: format!("names overlap ({:.0}%)", score * 100.0),
        })
        .collect();
    suggestions.sort_by(|x, y| {
        y.score
            .total_cmp(&x.score)
            .then_with(|| x.a.cmp(&y.a))
            .then_with(|| x.b.cmp(&y.b))
    });
    clusters.sort_by(|x, y| y.tracks.cmp(&x.tracks).then_with(|| x.name.cmp(&y.name)));
    Resolution {
        clusters,
        suggestions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(artist: &str, album: &str, title: &str) -> Track {
        Track {
            artist: artist.into(),
            album: album.into(),
            title: title.into(),
            artist_mbid: None,
        }
    }

    fn songs(artist: &str, album: &str, count: usize) -> Vec<Track> {
        (1..=count)
            .map(|n| song(artist, album, &format!("{album} {n}")))
            .collect()
    }

    /// One band under seven spellings, shaped like a real library: joiners and case differ, a short form shares
    /// one album with the full name, a shortened name has an album of its own, and a nickname shares no word.
    fn redondos() -> Vec<Track> {
        let mut t = vec![];
        t.extend(songs(
            "Patricio Rey y sus Redonditos de Ricota",
            "Cordero atado",
            12,
        ));
        t.extend(songs(
            "Patricio Rey y sus Redonditos de Ricota",
            "Último Bondi a Finisterre",
            9,
        ));
        t.extend(songs(
            "Patricio Rey y sus Redonditos de Ricota",
            "Oktubre",
            9,
        ));
        t.extend(songs(
            "Patricio Rey & Sus Redonditos De Ricota",
            "Lobo Suelto - Cordero Atado",
            13,
        ));
        t.extend(songs(
            "Patricio Rey Y Sus Redonditos De Ricota",
            "Un Baión Para El Ojo Idiota",
            8,
        ));
        t.extend(songs("Patricio Rey y sus redonditos", "Momo Sampler", 11));
        t.extend(songs(
            "Patricio Rey y sus redonditos",
            "La mosca y la sopa",
            10,
        ));
        t.extend(songs(
            "Patricio Rey y sus Redonditos",
            "Ultimo Bondi a Finisterre",
            1,
        ));
        t.extend(songs(
            "Los Redonditos de Ricota",
            "Bang! Bang!... Estas liquidado",
            9,
        ));
        t.extend(songs("Los Redondos", "Gulp!", 12));
        t.extend(songs("Los Redondos", "Luzbelito", 11));
        t
    }

    fn cluster_of<'a>(r: &'a Resolution, name: &str) -> &'a Cluster {
        r.clusters
            .iter()
            .find(|c| c.spellings.iter().any(|s| s == name))
            .expect(name)
    }

    #[test]
    fn text_and_a_shared_album_join_most_spellings() {
        let r = resolve(&redondos(), &Decisions::default());
        let full = cluster_of(&r, "Patricio Rey y sus Redonditos de Ricota");
        assert_eq!(full.spellings.len(), 5, "{:?}", full.spellings);
        for spelling in [
            "Patricio Rey & Sus Redonditos De Ricota",
            "Patricio Rey y sus redonditos",
        ] {
            assert!(full.spellings.iter().any(|s| s == spelling), "{spelling}");
        }
        assert_eq!(full.tracks, 12 + 9 + 9 + 13 + 8 + 11 + 10 + 1);
        assert_eq!(full.reasons, ["shares an album and the names overlap"]);
    }

    #[test]
    fn a_shortened_name_and_a_nickname_are_suggested_never_merged() {
        let r = resolve(&redondos(), &Decisions::default());
        assert_eq!(r.clusters.len(), 3);
        let suggested = |name: &str| {
            r.suggestions
                .iter()
                .find(|s| s.a == name || s.b == name)
                .expect(name)
                .score
        };
        assert!(suggested("Los Redonditos de Ricota") >= 0.9);
        assert!((suggested("Los Redondos") - 0.5).abs() < 1e-9);
        assert!(
            suggested("Los Redonditos de Ricota") > suggested("Los Redondos"),
            "best first"
        );
    }

    #[test]
    fn one_musicbrainz_artist_joins_every_spelling() {
        let mut tracks = redondos();
        for t in &mut tracks {
            t.artist_mbid = Some("956301D0-0000-0000-0000-000000000000".into());
        }
        let r = resolve(&tracks, &Decisions::default());
        assert_eq!(r.clusters.len(), 1);
        assert_eq!(r.clusters[0].spellings.len(), 7);
        assert!(r.clusters[0].reasons.contains(&"same MusicBrainz artist"));
        assert!(r.suggestions.is_empty());
    }

    #[test]
    fn the_user_can_join_a_nickname() {
        let mut decisions = Decisions::default();
        decisions.same("los redondos", "PATRICIO REY Y SUS REDONDITOS DE RICOTA");
        let r = resolve(&redondos(), &decisions);
        assert_eq!(r.clusters.len(), 2);
        assert!(cluster_of(&r, "Los Redondos")
            .reasons
            .contains(&"you said they are the same"));
    }

    #[test]
    fn a_number_makes_a_different_artist() {
        let mut tracks = songs("Maroon 5", "Songs About Jane", 5);
        tracks.extend(songs("Maroon 6", "Songs About Jane", 5));
        let r = resolve(&tracks, &Decisions::default());
        assert_eq!(r.clusters.len(), 2);
        assert!(r.suggestions.is_empty());
    }

    #[test]
    fn a_one_word_name_inside_a_longer_one_is_never_merged() {
        let mut tracks = songs("Queen", "Greatest Hits", 5);
        tracks.extend(songs("Queen Latifah", "Greatest Hits", 5));
        let r = resolve(&tracks, &Decisions::default());
        assert_eq!(r.clusters.len(), 2, "one word is too little to merge on");
        assert_eq!(r.suggestions.len(), 1);
    }

    #[test]
    fn a_typo_with_a_shared_album_is_merged_and_without_one_suggested() {
        let mut tracks = songs("Lenny Kravitz", "Mama Said", 5);
        tracks.extend(songs("Lenny Kraviz", "Mama Said", 2));
        assert_eq!(resolve(&tracks, &Decisions::default()).clusters.len(), 1);

        let mut apart = songs("Lenny Kravitz", "Mama Said", 5);
        apart.extend(songs("Lenny Kraviz", "Circus", 2));
        let r = resolve(&apart, &Decisions::default());
        assert_eq!(r.clusters.len(), 2);
        assert_eq!(r.suggestions.len(), 1);
    }

    #[test]
    fn songs_shared_without_an_album_count_after_three() {
        let mut tracks = vec![];
        for n in 1..=3 {
            tracks.push(song("Lenny Kravitz", "Mama Said", &format!("Song {n}")));
            tracks.push(song("Lenny Kraviz", "Hits", &format!("Song {n}")));
        }
        let r = resolve(&tracks, &Decisions::default());
        assert_eq!(r.clusters.len(), 1);
        assert_eq!(
            r.clusters[0].reasons,
            ["shares songs and the names overlap"]
        );
    }

    #[test]
    fn a_decision_that_they_differ_blocks_a_merge_and_silences_the_suggestion() {
        let mut tracks = songs("Lenny Kravitz", "Mama Said", 5);
        tracks.extend(songs("Lenny Kraviz", "Mama Said", 2));
        let mut decisions = Decisions::default();
        decisions.different("Lenny Kraviz", "lenny kravitz");
        let r = resolve(&tracks, &decisions);
        assert_eq!(r.clusters.len(), 2);
        assert!(r.suggestions.is_empty());
    }

    #[test]
    fn the_result_does_not_depend_on_the_order_of_the_tracks() {
        let forward = resolve(&redondos(), &Decisions::default());
        let mut reversed = redondos();
        reversed.reverse();
        assert_eq!(forward, resolve(&reversed, &Decisions::default()));
    }

    #[test]
    fn an_empty_library_has_nothing_to_resolve() {
        assert_eq!(resolve(&[], &Decisions::default()), Resolution::default());
        assert_eq!(
            resolve(&[song("", "x", "y")], &Decisions::default())
                .clusters
                .len(),
            0
        );
    }
}
