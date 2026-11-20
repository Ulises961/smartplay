//! Asking Last.fm what it knows of the artists of a library: opt-in, slow on purpose, and resumable.
//!
//! Only artist names leave the engine: no user name, no plays, no song titles. What comes back is kept in one file shared by
//! every user (it is about artists, not about anyone), so an artist is asked once. An answer that says the artist is not
//! known is kept for a while; a service that could not answer is not an answer, so nothing is kept for that artist and the
//! run stops, to be repeated later from where it was.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use smartplay_core::{
    enrich::{self, ArtistInfo, Enrichment, Failure},
    normalize::fold,
    plays::{Baseline, Play},
    scrobbles::{self, Page},
    taste::Song,
};

const API: &str = "https://ws.audioscrobbler.com/2.0/";

/// Last.fm allows five requests a second; this is a fraction of that.
const GAP: Duration = Duration::from_millis(350);

/// An artist Last.fm did not know is asked about again after this long: ninety days.
const UNKNOWN_FOR: i64 = 90 * 24 * 60 * 60;

/// Asking about one artist.
pub trait Lookup {
    fn info(&self, artist: &str) -> Result<ArtistInfo, Failure>;
}

/// Last.fm over HTTP, one request at a time, at least `gap` apart.
pub struct Client {
    key: String,
    base: String,
    gap: Duration,
    agent: ureq::Agent,
    last: std::cell::Cell<Option<Instant>>,
}

impl Client {
    pub fn new(key: &str) -> Client {
        Client::with(key, API, GAP)
    }

    fn with(key: &str, base: &str, gap: Duration) -> Client {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            // Last.fm tells why it refuses in the body of a 4xx or 5xx: the body is what is read, not the status.
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .user_agent("smartplay")
            .build()
            .into();
        Client {
            key: key.into(),
            base: base.into(),
            gap,
            agent,
            last: std::cell::Cell::new(None),
        }
    }

    fn get(&self, method: &str, params: &[(&str, &str)]) -> Result<String, Failure> {
        if let Some(last) = self.last.get() {
            if let Some(wait) = self.gap.checked_sub(last.elapsed()) {
                std::thread::sleep(wait);
            }
        }
        self.last.set(Some(Instant::now()));
        let mut request = self
            .agent
            .get(&self.base)
            .query("method", method)
            .query("api_key", &self.key)
            .query("format", "json");
        for (name, value) in params {
            request = request.query(name, value);
        }
        let mut response = request
            .call()
            .map_err(|e| Failure::Temporary(e.to_string()))?;
        response
            .body_mut()
            .read_to_string()
            .map_err(|e| Failure::Temporary(e.to_string()))
    }
}

impl Lookup for Client {
    fn info(&self, artist: &str) -> Result<ArtistInfo, Failure> {
        let about = [("artist", artist), ("autocorrect", "1"), ("limit", "30")];
        let tags = enrich::parse_top_tags(&self.get("artist.getTopTags", &about)?)?;
        let similar = enrich::parse_similar(&self.get("artist.getSimilar", &about)?)?;
        Ok(ArtistInfo { tags, similar })
    }
}

/// Asking for the history of a user's plays, a page at a time, the newest first.
pub trait History {
    fn page(&self, user: &str, page: u32) -> Result<Page, Failure>;
}

impl History for Client {
    fn page(&self, user: &str, page: u32) -> Result<Page, Failure> {
        let page = page.to_string();
        let text = self.get(
            "user.getRecentTracks",
            &[("user", user), ("page", &page), ("limit", "200")],
        )?;
        scrobbles::parse_recent_tracks(&text)
    }
}

/// The plays of the first `max_pages` pages of a history (all of them, when it is shorter). When Last.fm cannot answer
/// part-way the plays of the pages before are still returned, with why it stopped.
pub fn fetch_history(
    source: &dyn History,
    user: &str,
    max_pages: u32,
) -> (Vec<Play>, Option<String>) {
    let mut plays = vec![];
    for page in 1..=max_pages {
        match source.page(user, page) {
            Ok(got) => {
                plays.extend(got.plays);
                if got.page >= got.total_pages {
                    break;
                }
            }
            Err(Failure::Unknown) => {
                return (plays, Some("Last.fm does not know that user".into()))
            }
            Err(Failure::Temporary(why)) => return (plays, Some(why)),
        }
    }
    (plays, None)
}

/// What has been asked so far: the artists Last.fm knows, and when it was told that it does not know one.
#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
pub struct Cache {
    pub known: Enrichment,
    /// Folded names, and when the answer "not known" came, in seconds since the epoch.
    pub unknown: HashMap<String, i64>,
}

impl Cache {
    fn needs_asking(&self, artist: &str, now: i64) -> bool {
        self.known.get(artist).is_none()
            && self
                .unknown
                .get(&fold(artist))
                .is_none_or(|&at| now - at > UNKNOWN_FOR)
    }
}

/// How a run went.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    pub asked: usize,
    pub known: usize,
    pub unknown: usize,
    /// Why the run stopped before its end, when it did.
    pub stopped: Option<String>,
}

/// Asks about the artists in `order` that the cache does not hold, at most `limit` of them, and keeps what comes back in
/// `cache`. A temporary failure ends the run at once.
pub fn fill(
    cache: &mut Cache,
    order: &[String],
    lookup: &dyn Lookup,
    limit: usize,
    now: i64,
) -> Outcome {
    let mut outcome = Outcome::default();
    for artist in order {
        if outcome.asked >= limit {
            break;
        }
        if !cache.needs_asking(artist, now) {
            continue;
        }
        match lookup.info(artist) {
            Ok(info) => {
                cache.unknown.remove(&fold(artist));
                cache.known.insert(artist, info);
                outcome.known += 1;
            }
            Err(Failure::Unknown) => {
                cache.unknown.insert(fold(artist), now);
                outcome.unknown += 1;
            }
            Err(Failure::Temporary(why)) => {
                outcome.stopped = Some(why);
                break;
            }
        }
        outcome.asked += 1;
    }
    outcome
}

/// The artists of `library`, the ones heard most first (what the history and the baseline count, then the number of songs),
/// each once, named as the library names them.
pub fn artists_to_ask(
    library: &[Song],
    played: &[Play],
    baseline: Option<&Baseline>,
) -> Vec<String> {
    let mut heard: HashMap<String, usize> = HashMap::new();
    for play in played {
        if let Some(artist) = play
            .artists
            .iter()
            .map(|a| a.trim())
            .find(|a| !a.is_empty())
        {
            *heard.entry(fold(artist)).or_default() += 1;
        }
    }
    for tally in baseline.iter().flat_map(|b| &b.tallies) {
        *heard.entry(fold(&tally.artist)).or_default() += tally.count as usize;
    }
    let mut songs: HashMap<String, (usize, &str)> = HashMap::new();
    for song in library.iter().filter(|s| !s.artist.trim().is_empty()) {
        let entry = songs
            .entry(fold(&song.artist))
            .or_insert((0, song.artist.trim()));
        entry.0 += 1;
    }
    let mut ranked: Vec<(&String, &(usize, &str))> = songs.iter().collect();
    ranked.sort_by(|(ka, (na, _)), (kb, (nb, _))| {
        let heard_of = |k: &String| heard.get(k).copied().unwrap_or(0);
        heard_of(kb)
            .cmp(&heard_of(ka))
            .then(nb.cmp(na))
            .then_with(|| ka.cmp(kb))
    });
    ranked
        .into_iter()
        .map(|(_, (_, name))| name.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Answers from a table; an artist that is not in it is not known, and `"Busy"` makes the service fail.
    struct Fake(RefCell<Vec<String>>);
    impl Lookup for Fake {
        fn info(&self, artist: &str) -> Result<ArtistInfo, Failure> {
            self.0.borrow_mut().push(artist.to_string());
            match artist {
                "Busy" => Err(Failure::Temporary("Rate limit exceeded".into())),
                "Nobody" => Err(Failure::Unknown),
                _ => Ok(ArtistInfo {
                    tags: vec![("rock".into(), 1.0)],
                    similar: vec![],
                }),
            }
        }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn what_is_known_is_kept_and_not_asked_again() {
        let mut cache = Cache::default();
        let fake = Fake(RefCell::new(vec![]));
        let order = names(&["A", "B"]);
        let first = fill(&mut cache, &order, &fake, 10, 1000);
        assert_eq!((first.asked, first.known), (2, 2));
        let again = fill(&mut cache, &order, &fake, 10, 1001);
        assert_eq!(again.asked, 0);
        assert_eq!(fake.0.borrow().len(), 2);
        assert!(cache.known.get("a").is_some());
    }

    #[test]
    fn a_run_asks_at_most_the_limit_and_the_next_one_goes_on() {
        let mut cache = Cache::default();
        let fake = Fake(RefCell::new(vec![]));
        let order = names(&["A", "B", "C"]);
        assert_eq!(fill(&mut cache, &order, &fake, 2, 1).asked, 2);
        assert_eq!(fill(&mut cache, &order, &fake, 2, 2).asked, 1);
        assert_eq!(cache.known.len(), 3);
    }

    #[test]
    fn an_unknown_artist_is_remembered_for_ninety_days_and_then_asked_again() {
        let mut cache = Cache::default();
        let fake = Fake(RefCell::new(vec![]));
        let order = names(&["Nobody"]);
        assert_eq!(fill(&mut cache, &order, &fake, 10, 1000).unknown, 1);
        assert_eq!(fill(&mut cache, &order, &fake, 10, 2000).asked, 0);
        assert_eq!(
            fill(&mut cache, &order, &fake, 10, 1000 + UNKNOWN_FOR + 1).asked,
            1
        );
    }

    #[test]
    fn a_service_that_cannot_answer_ends_the_run_and_nothing_is_kept_for_that_artist() {
        let mut cache = Cache::default();
        let fake = Fake(RefCell::new(vec![]));
        let order = names(&["A", "Busy", "C"]);
        let run = fill(&mut cache, &order, &fake, 10, 1000);
        assert_eq!((run.asked, run.known), (1, 1));
        assert!(run.stopped.unwrap().contains("Rate limit"));
        assert_eq!(*fake.0.borrow(), ["A", "Busy"], "C was not asked");
        assert!(cache.known.get("Busy").is_none());
        assert!(cache.unknown.is_empty(), "a failure is not an absence");
    }

    #[test]
    fn the_artists_heard_most_are_asked_first() {
        let song = |id: &str, artist: &str| Song {
            id: id.into(),
            artist: artist.into(),
            ..Song::default()
        };
        let library = vec![
            song("1", "Many Songs"),
            song("2", "Many Songs"),
            song("3", "Played"),
            song("4", "Counted"),
            song("5", "Neither"),
            song("6", " "),
        ];
        let played = vec![Play {
            song: "3".into(),
            artists: vec!["played".into()],
            at: 1,
            ..Play::default()
        }];
        let baseline = Baseline {
            at: 0,
            tallies: vec![smartplay_core::plays::Tally {
                song: "4".into(),
                artist: "Counted".into(),
                title: "T".into(),
                count: 9,
                last_played: None,
            }],
        };
        assert_eq!(
            artists_to_ask(&library, &played, Some(&baseline)),
            ["Counted", "Played", "Many Songs", "Neither"]
        );
    }

    /// Pages of a history of five plays, two a page, in the order Last.fm gives them: the newest first.
    struct Pages {
        fail_at: Option<u32>,
    }
    impl History for Pages {
        fn page(&self, _: &str, page: u32) -> Result<Page, Failure> {
            if self.fail_at == Some(page) {
                return Err(Failure::Temporary("Service offline".into()));
            }
            let plays = (0..2)
                .map(|i| Play::new("", &["A".to_string()], "", 1000 - (page as i64 * 2 + i)))
                .take(if page == 3 { 1 } else { 2 })
                .collect();
            Ok(Page {
                plays,
                page,
                total_pages: 3,
            })
        }
    }

    #[test]
    fn a_history_is_read_page_by_page_to_its_end() {
        let (plays, stopped) = fetch_history(&Pages { fail_at: None }, "u", 100);
        assert_eq!((plays.len(), stopped), (5, None));
        let (plays, _) = fetch_history(&Pages { fail_at: None }, "u", 2);
        assert_eq!(plays.len(), 4, "the pages asked for and no more");
    }

    #[test]
    fn a_history_cut_short_keeps_the_pages_before_and_says_why() {
        let (plays, stopped) = fetch_history(&Pages { fail_at: Some(2) }, "u", 100);
        assert_eq!(plays.len(), 2);
        assert!(stopped.unwrap().contains("offline"));
    }

    /// One answer for the whole run, over a real socket, to see what is asked and how the answer is read.
    fn serve(answers: Vec<(u16, &'static str)>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}/", server.server_addr().to_ip().unwrap());
        let handle = std::thread::spawn(move || {
            let mut urls = vec![];
            for (status, body) in answers {
                let request = server.recv().unwrap();
                urls.push(request.url().to_string());
                let response = tiny_http::Response::from_string(body).with_status_code(status);
                request.respond(response).unwrap();
            }
            urls
        });
        (base, handle)
    }

    #[test]
    fn the_client_asks_for_the_tags_and_the_close_artists_and_sends_only_the_artist() {
        let (base, handle) = serve(vec![
            (
                200,
                r#"{"toptags":{"tag":[{"count":100,"name":"trip-hop"}]}}"#,
            ),
            (
                200,
                r#"{"similarartists":{"artist":[{"name":"Massive Attack","match":"0.8"}]}}"#,
            ),
        ]);
        let client = Client::with("the-key", &base, Duration::ZERO);
        let info = client.info("Portishead").unwrap();
        assert_eq!(info.tags, [("trip hop".to_string(), 1.0)]);
        assert_eq!(info.similar[0].0, "Massive Attack");
        let urls = handle.join().unwrap();
        assert!(
            urls[0].contains("method=artist.getTopTags") && urls[0].contains("artist=Portishead")
        );
        assert!(urls[1].contains("method=artist.getSimilar"));
        assert!(
            urls.iter()
                .all(|u| u.contains("api_key=the-key") && !u.contains("user")),
            "{urls:?}"
        );
    }

    #[test]
    fn the_reason_in_the_body_of_an_error_status_is_what_counts() {
        let (base, handle) = serve(vec![(
            404,
            r#"{"error":6,"message":"The artist you supplied could not be found"}"#,
        )]);
        let client = Client::with("k", &base, Duration::ZERO);
        assert_eq!(client.info("Nobody"), Err(Failure::Unknown));
        handle.join().unwrap();
    }
}
