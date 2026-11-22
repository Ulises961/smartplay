//! Engine: registered servers, one-time enrollment codes, bearer tokens. Answers each granted user with a playlist.
//! State is a JSON file so the `servers` commands (separate processes) and `serve` share it.

mod eval;
mod history;
mod lastfm;

use serde::{Deserialize, Serialize};
use smartplay_core::{
    identity::{self, Decisions},
    mix,
    plays::{self, Play},
    protocol::*,
    taste,
};
use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

const CODE_LIFETIME_SECS: u64 = 15 * 60;
/// The playlist every granted user gets, and how many songs it holds.
const PLAYLIST: &str = "smartplay";
const PLAYLIST_SIZE: usize = 50;
/// The mixes of the artists a user played lately are `smartplay: mix 1` and `smartplay: mix 2`, this many songs each.
const MIX_PLAYLIST: &str = "smartplay: mix";
const MIX_SIZE: usize = 50;

/// A user's library as the engine holds it, with the fingerprint it had when it was read.
#[derive(Serialize, Deserialize)]
struct StoredLibrary {
    fingerprint: String,
    songs: Vec<taste::Song>,
}

fn hex(text: &str) -> String {
    text.bytes().map(|b| format!("{b:02x}")).collect()
}

#[derive(Serialize, Deserialize, Clone)]
struct Server {
    name: String,
    code: Option<String>,
    code_expires: u64,
    token: Option<String>,
}

struct Store(PathBuf);

impl Store {
    fn load(&self) -> Vec<Server> {
        fs::read_to_string(&self.0)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    fn save(&self, servers: &[Server]) {
        fs::write(&self.0, serde_json::to_string_pretty(servers).unwrap()).expect("write state");
    }

    /// Where a server's copy of a user's library lives. Names are hex-encoded: any user name is a safe file name.
    fn library_path(&self, server: &str, user: &str, extension: &str) -> PathBuf {
        let dir = self.0.parent().map(PathBuf::from).unwrap_or_default();
        dir.join(format!("library-{}-{}.{extension}", hex(server), hex(user)))
    }

    fn library(&self, server: &str, user: &str) -> Option<StoredLibrary> {
        let text = fs::read_to_string(self.library_path(server, user, "json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Adds one chunk to the copy being uploaded; the last chunk makes it the copy in use.
    fn receive(&self, server: &str, chunk: LibraryChunk) -> Result<(), String> {
        let part = self.library_path(server, &chunk.user, "part");
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(!chunk.first)
            .truncate(chunk.first)
            .open(&part)
            .map_err(|e| e.to_string())?;
        for song in &chunk.songs {
            let line = serde_json::to_string(song).map_err(|e| e.to_string())?;
            writeln!(file, "{line}").map_err(|e| e.to_string())?;
        }
        if !chunk.last {
            return Ok(());
        }
        let mut songs: Vec<taste::Song> = fs::read_to_string(&part)
            .map_err(|e| e.to_string())?
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        self.take_baseline(server, &chunk.user, &mut songs)?;
        let stored = StoredLibrary {
            fingerprint: chunk.fingerprint,
            songs,
        };
        let done = self.library_path(server, &chunk.user, "tmp");
        fs::write(&done, serde_json::to_string(&stored).unwrap()).map_err(|e| e.to_string())?;
        fs::rename(&done, self.library_path(server, &chunk.user, "json"))
            .map_err(|e| e.to_string())?;
        let _ = fs::remove_file(part);
        Ok(())
    }

    fn lastfm_path(&self) -> PathBuf {
        let dir = self.0.parent().map(PathBuf::from).unwrap_or_default();
        dir.join("enrichment-lastfm.json")
    }

    /// What Last.fm said of artists, shared by every user (it is about artists, not about anyone); nothing when it was
    /// never asked.
    fn lastfm(&self) -> lastfm::Cache {
        fs::read_to_string(self.lastfm_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save_lastfm(&self, cache: &lastfm::Cache) -> Result<(), String> {
        let path = self.lastfm_path();
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, serde_json::to_string(cache).unwrap()).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    fn recent_path(&self, server: &str, user: &str) -> PathBuf {
        let dir = self.0.parent().map(PathBuf::from).unwrap_or_default();
        dir.join(format!("recent-{}-{}.json", hex(server), hex(user)))
    }

    /// The artists of the albums the server last listed as recent for a user, as the plugin sent them: the engine cannot ask
    /// the server itself, so it keeps the last list to show, in `eval`, the mixes the plugin gets.
    fn recent(&self, server: &str, user: &str) -> Vec<String> {
        fs::read_to_string(self.recent_path(server, user))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn keep_recent(&self, server: &str, user: &str, recent: &[String]) {
        let path = self.recent_path(server, user);
        if self.recent(server, user) != recent {
            let _ = fs::write(path, serde_json::to_string(recent).unwrap());
        }
    }

    fn baseline_path(&self, server: &str, user: &str) -> PathBuf {
        let dir = self.0.parent().map(PathBuf::from).unwrap_or_default();
        dir.join(format!("baseline-{}-{}.json", hex(server), hex(user)))
    }

    /// What the server counted as played before the engine heard of any play; none when it was never taken.
    fn baseline(&self, server: &str, user: &str) -> Option<plays::Baseline> {
        let text = fs::read_to_string(self.baseline_path(server, user)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Keeps the play counts of the first copy of a user's library as their baseline, once, and takes the counts out of
    /// `songs`: they change with every play, and the history is where plays are kept from then on.
    fn take_baseline(
        &self,
        server: &str,
        user: &str,
        songs: &mut [taste::Song],
    ) -> Result<(), String> {
        let path = self.baseline_path(server, user);
        if !path.exists() {
            let tallies: Vec<plays::Tally> = songs
                .iter()
                .filter_map(|s| {
                    Some(plays::Tally {
                        song: s.id.clone(),
                        artist: s.artist.clone(),
                        title: s.title.clone(),
                        count: s.play_count.filter(|&c| c > 0)?,
                        last_played: s.last_played.clone(),
                    })
                })
                .collect();
            let baseline = plays::Baseline {
                at: now() as i64,
                tallies,
            };
            let tmp = path.with_extension("tmp");
            fs::write(&tmp, serde_json::to_string(&baseline).unwrap())
                .map_err(|e| e.to_string())?;
            fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        }
        for song in songs {
            song.play_count = None;
            song.last_played = None;
        }
        Ok(())
    }

    fn decisions_path(&self, server: &str, user: &str) -> PathBuf {
        let dir = self.0.parent().map(PathBuf::from).unwrap_or_default();
        dir.join(format!("decisions-{}-{}.json", hex(server), hex(user)))
    }

    /// What a user decided about artist names; nothing decided when there is no file or it cannot be read.
    fn decisions(&self, server: &str, user: &str) -> Decisions {
        fs::read_to_string(self.decisions_path(server, user))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Changes a user's decisions and keeps them.
    fn remember(&self, server: &str, user: &str, change: impl FnOnce(&mut Decisions)) {
        let mut decisions = self.decisions(server, user);
        change(&mut decisions);
        fs::write(
            self.decisions_path(server, user),
            serde_json::to_string_pretty(&decisions).unwrap(),
        )
        .expect("write decisions");
    }

    fn plays_path(&self, server: &str, user: &str) -> PathBuf {
        let dir = self.0.parent().map(PathBuf::from).unwrap_or_default();
        dir.join(format!("plays-{}-{}.jsonl", hex(server), hex(user)))
    }

    fn play_log(&self, server: &str, user: &str) -> history::PlayLog {
        history::PlayLog::new(self.plays_path(server, user))
    }

    /// The plays kept for a user, oldest first; empty when there are none.
    fn plays(&self, server: &str, user: &str) -> Vec<Play> {
        self.play_log(server, user).load()
    }

    /// Adds a play to the user's history and drops what is past the retention. Returns whether it was new.
    fn record_play(&self, server: &str, user: &str, play: Play) -> Result<bool, String> {
        let log = self.play_log(server, user);
        let new = log.append(&play)?;
        if new {
            // A failed rewrite loses nothing: the next play tries again.
            if let Err(e) = log.compact(now() as i64, plays::RETENTION) {
                eprintln!(
                    "{}: not compacted: {e}",
                    self.plays_path(server, user).display()
                );
            }
        }
        Ok(new)
    }

    /// Deletes everything a server holds about its users: library copies, decisions and plays.
    fn forget_data(&self, server: &str) {
        self.forget(&[
            format!("library-{}-", hex(server)),
            format!("decisions-{}-", hex(server)),
            format!("plays-{}-", hex(server)),
            format!("baseline-{}-", hex(server)),
            format!("recent-{}-", hex(server)),
        ]);
    }

    /// Deletes everything held about one user of a server: the library copy, the decisions and the history of plays.
    fn forget_user(&self, server: &str, user: &str) {
        let name = format!("{}-{}.", hex(server), hex(user));
        self.forget(&[
            format!("library-{name}"),
            format!("decisions-{name}"),
            format!("plays-{name}"),
            format!("baseline-{name}"),
            format!("recent-{name}"),
        ]);
    }

    /// Deletes the files of the data folder whose names start with any of `prefixes`.
    fn forget(&self, prefixes: &[String]) {
        let dir = self.0.parent().map(PathBuf::from).unwrap_or_default();
        let dir = if dir.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            dir
        };
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if prefixes.iter().any(|p| name.starts_with(p)) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    /// Adds the plays told by the lines of an import file (see `plays::from_import_line`) to a user's history.
    fn import_plays(&self, server: &str, user: &str, source: &str, text: &str) -> ImportReport {
        let mut report = ImportReport::default();
        let mut plays = vec![];
        for (n, line) in text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
        {
            match plays::from_import_line(line, source) {
                Ok(play) => plays.push(play),
                Err(e) => report.invalid.push((n + 1, e)),
            }
        }
        report.read = plays.len();
        match self.play_log(server, user).append_many(plays) {
            Ok(added) => report.added = added,
            Err(e) => report.failed = Some(e),
        }
        report
    }
}

/// What an import did.
#[derive(Default, Debug)]
struct ImportReport {
    /// Lines that were plays.
    read: usize,
    /// Of those, the ones that were new.
    added: usize,
    /// Lines that were not, by line number, and why.
    invalid: Vec<(usize, String)>,
    failed: Option<String>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn random_text(len: usize, alphabet: &[u8]) -> String {
    let mut buf = vec![0u8; len];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .expect("urandom");
    buf.iter()
        .map(|b| alphabet[*b as usize % alphabet.len()] as char)
        .collect()
}

fn new_code() -> String {
    // No 0/O/1/I: the code is read off a log and typed or pasted by a person.
    random_text(8, b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789")
}

fn issue_code(server: &mut Server) -> String {
    let code = new_code();
    server.code = Some(code.clone());
    server.code_expires = now() + CODE_LIFETIME_SECS;
    code
}

fn banner(lines: &[String]) {
    let width = lines.iter().map(|l| l.len()).max().unwrap_or(0) + 4;
    let bar = "*".repeat(width);
    println!("\n{bar}");
    for l in lines {
        println!("* {l:<w$} *", w = width - 4);
    }
    println!("{bar}\n");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let store =
        Store(PathBuf::from(arg("--data").unwrap_or_else(|| ".".into())).join("servers.json"));
    match args.first().map(String::as_str) {
        Some("serve") => serve(
            store,
            &arg("--bind").unwrap_or_else(|| "127.0.0.1:7788".into()),
        ),
        Some("servers") => servers_command(store, &args[1..]),
        Some("artists") => artists_command(store, &args[1..]),
        Some("import") => import_command(store, &args[1..]),
        Some("purge") => purge_command(store, &args[1..]),
        Some("eval") => eval_command(store, &args[1..]),
        Some("enrich") => enrich_command(store, &args[1..]),
        Some("import-lastfm") => import_lastfm_command(store, &args[1..]),
        _ => eprintln!(
            "usage: smartplay-engine serve [--bind ADDR] [--data DIR] | servers add|list|remove NAME\n       \
             smartplay-engine artists same|different SERVER USER NAME NAME | artists list SERVER USER\n       \
             smartplay-engine enrich SERVER USER [--limit N] [--data DIR]   (the key is in SMARTPLAY_LASTFM_KEY)\n       \
             smartplay-engine import-lastfm SERVER USER --lastfm-user NAME [--pages N] [--data DIR]\n       \
             smartplay-engine eval SERVER USER [--size N] [--holdout-days N] [--min-fit X] [--min-aligned X] [--max-share X] [--data DIR]\n       \
             smartplay-engine import SERVER USER FILE [--source NAME] [--data DIR] | purge SERVER USER [--data DIR]"
        ),
    }
}

/// The words of a command, without its `--flag VALUE` pairs (every flag the engine has takes a value).
fn positional(args: &[String]) -> Vec<&str> {
    let mut words = vec![];
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
        } else if a.starts_with("--") {
            skip = true;
        } else {
            words.push(a.as_str());
        }
    }
    words
}

/// Adds the plays of a file of JSON lines to a user's history (the lines are described at `plays::from_import_line`).
/// A play already kept is not added again, so running an import twice adds nothing the second time.
fn import_command(store: Store, args: &[String]) {
    let source = args
        .iter()
        .position(|a| a == "--source")
        .and_then(|i| args.get(i + 1))
        .map_or("file", String::as_str);
    let words = positional(args);
    let [server, user, file] = words.as_slice() else {
        eprintln!("usage: import SERVER USER FILE [--source NAME]");
        return;
    };
    let text = match fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) => return eprintln!("{file}: {e}"),
    };
    let report = store.import_plays(server, user, source, &text);
    for (line, why) in report.invalid.iter().take(10) {
        eprintln!("line {line}: {why}");
    }
    if report.invalid.len() > 10 {
        eprintln!("... and {} more lines", report.invalid.len() - 10);
    }
    if let Some(e) = report.failed {
        return eprintln!("the history could not be written: {e}");
    }
    println!(
        "{} plays read, {} new, {} already kept, {} lines that are not plays",
        report.read,
        report.added,
        report.read - report.added,
        report.invalid.len()
    );
}

/// Asks Last.fm for the tags and close artists of the artists of a user's library, the ones heard most first, and keeps the
/// answers for every user. It is opt-in (it needs the key in `SMARTPLAY_LASTFM_KEY`) and sends nothing but artist names. A
/// run asks for `--limit` artists at most (100 by default) and the next run goes on from there.
fn enrich_command(store: Store, args: &[String]) {
    let Some(key) = std::env::var("SMARTPLAY_LASTFM_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
    else {
        return eprintln!(
            "Last.fm is asked only when you give a key: set SMARTPLAY_LASTFM_KEY (the names of the artists of the library are \
             sent to Last.fm, and nothing else)"
        );
    };
    let words = positional(args);
    let [server, user] = words.as_slice() else {
        return eprintln!("usage: enrich SERVER USER [--limit N]");
    };
    let Some(library) = store.library(server, user) else {
        return eprintln!(
            "the engine holds no library for {user} on {server}: run the plugin once"
        );
    };
    let mut left = args
        .iter()
        .position(|a| a == "--limit")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(100usize);
    let order = lastfm::artists_to_ask(
        &library.songs,
        &store.plays(server, user),
        store.baseline(server, user).as_ref(),
    );
    let client = lastfm::Client::new(key.trim());
    let mut cache = store.lastfm();
    let mut total = lastfm::Outcome::default();
    // In steps, saving after each, so that a run that is cut short keeps what it has.
    while left > 0 {
        let step = lastfm::fill(&mut cache, &order, &client, left.min(10), now() as i64);
        if let Err(e) = store.save_lastfm(&cache) {
            return eprintln!("what Last.fm said could not be kept: {e}");
        }
        left -= step.asked;
        total.asked += step.asked;
        total.known += step.known;
        total.unknown += step.unknown;
        total.skipped += step.skipped;
        if step.stopped.is_some() || step.asked == 0 {
            total.stopped = step.stopped;
            break;
        }
    }
    println!(
        "asked about {} artists: {} known, {} not known to Last.fm, {} left for later; {} artists have tags now",
        total.asked,
        total.known,
        total.unknown,
        total.skipped,
        cache.known.len()
    );
    if let Some(why) = total.stopped {
        eprintln!("stopped: Last.fm could not answer ({why}); run it again later to go on");
    }
}

/// Adds the plays of a user's Last.fm history to their history here: what they heard anywhere, not only on this server. It
/// is opt-in (the key in `SMARTPLAY_LASTFM_KEY`, and the Last.fm name is given by the operator) and a play already kept is
/// not added again, so it can be run again to bring in what is newer.
fn import_lastfm_command(store: Store, args: &[String]) {
    let Some(key) = std::env::var("SMARTPLAY_LASTFM_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
    else {
        return eprintln!("set SMARTPLAY_LASTFM_KEY to import a Last.fm history");
    };
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
    };
    let words = positional(args);
    let ([server, user], Some(lastfm_user)) = (words.as_slice(), flag("--lastfm-user")) else {
        return eprintln!("usage: import-lastfm SERVER USER --lastfm-user NAME [--pages N]");
    };
    let pages = flag("--pages")
        .and_then(|v| v.parse().ok())
        .unwrap_or(u32::MAX);
    let client = lastfm::Client::new(key.trim());
    let (plays, stopped) = lastfm::fetch_history(&client, lastfm_user, pages);
    let read = plays.len();
    match store.play_log(server, user).append_many(plays) {
        Ok(added) => println!(
            "{read} plays read from Last.fm, {added} new, {} already kept",
            read - added
        ),
        Err(e) => return eprintln!("the history could not be written: {e}"),
    }
    if let Some(why) = stopped {
        eprintln!("stopped: {why}; run it again later to go on (what was read is kept)");
    }
}

/// Prints the mixes a user would get from the library and history the engine holds, and how well they would have predicted
/// the last days of it (see `eval.rs`). Nothing is changed.
fn eval_command(store: Store, args: &[String]) {
    let number = |flag: &str, default: i64| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    let share = |flag: &str, default: f64| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    let words = positional(args);
    let [server, user] = words.as_slice() else {
        return eprintln!(
            "usage: eval SERVER USER [--size N] [--holdout-days N] [--min-fit X] [--min-aligned X] [--max-share X]"
        );
    };
    let tuning = {
        let base = smartplay_core::mix::Tuning::default();
        smartplay_core::mix::Tuning {
            min_aligned: share("--min-aligned", base.min_aligned),
            min_fit: share("--min-fit", base.min_fit),
            max_artist_share: share("--max-share", base.max_artist_share),
        }
    };
    let Some(library) = store.library(server, user) else {
        return eprintln!(
            "the engine holds no library for {user} on {server}: run the plugin once"
        );
    };
    let (played, baseline) = (store.plays(server, user), store.baseline(server, user));
    let (decisions, cache) = (store.decisions(server, user), store.lastfm());
    let recent = store.recent(server, user);
    let report = eval::evaluate(
        &eval::Input {
            library: &library.songs,
            played: &played,
            baseline: baseline.as_ref(),
            recent: &recent,
            decisions: &decisions,
            enrichment: &cache.known,
            tuning,
        },
        number("--size", MIX_SIZE as i64) as usize,
        number("--holdout-days", 14),
    );
    print!("{}", report.render());
}

/// Deletes everything held about one user of a server: the library copy, the artist decisions and the history of plays.
fn purge_command(store: Store, args: &[String]) {
    match positional(args).as_slice() {
        [server, user] => {
            store.forget_user(server, user);
            println!("deleted what is held about {user} on {server}");
        }
        _ => eprintln!("usage: purge SERVER USER"),
    }
}

/// Artist names the user says are one artist or two, and what the library still leaves open. Names are matched as
/// folded text, so case, accents and `&`/`y`/`and` do not matter. Nothing in the library is renamed.
fn artists_command(store: Store, args: &[String]) {
    match positional(args).as_slice() {
        [verb @ ("same" | "different"), server, user, a, b] => {
            store.remember(server, user, |d| {
                if *verb == "same" {
                    d.same(a, b)
                } else {
                    d.different(a, b)
                }
            });
            println!("kept: '{a}' and '{b}' are {verb} for {user} on {server}");
        }
        ["list", server, user] => {
            let decisions = store.decisions(server, user);
            for (a, b) in decisions.same_pairs() {
                println!("same       {a}  =  {b}");
            }
            for (a, b) in decisions.different_pairs() {
                println!("different  {a}  !=  {b}");
            }
            let Some(copy) = store.library(server, user) else {
                println!("no library copy for {user} on {server} yet, so no suggestions");
                return;
            };
            let tracks: Vec<identity::Track> = copy
                .songs
                .iter()
                .map(|s| identity::Track {
                    artist: s.artist.clone(),
                    album: s.album.clone(),
                    title: s.title.clone(),
                    artist_mbid: s.artist_mbid.clone(),
                })
                .collect();
            let found = identity::resolve(&tracks, &decisions);
            for c in found.clusters.iter().filter(|c| c.spellings.len() > 1) {
                println!(
                    "one artist  {} songs  {}",
                    c.tracks,
                    c.spellings.join("  |  ")
                );
            }
            for s in found.suggestions.iter().take(20) {
                println!(
                    "maybe       {:>3.0}%  '{}'  and  '{}'",
                    s.score * 100.0,
                    s.a,
                    s.b
                );
            }
            if !found.suggestions.is_empty() {
                println!(
                    "to decide: smartplay-engine artists same|different {server} {user} NAME NAME"
                );
            }
        }
        _ => eprintln!(
            "usage: artists same|different SERVER USER NAME NAME | artists list SERVER USER"
        ),
    }
}

fn servers_command(store: Store, args: &[String]) {
    let mut servers = store.load();
    match (args.first().map(String::as_str), args.get(1)) {
        (Some("add"), Some(name)) => {
            servers.retain(|s| &s.name != name);
            let mut s = Server {
                name: name.clone(),
                code: None,
                code_expires: 0,
                token: None,
            };
            let code = issue_code(&mut s);
            servers.push(s);
            store.save(&servers);
            println!("enrollment code for '{name}' (valid 15 minutes): {code}");
        }
        (Some("remove"), Some(name)) => {
            servers.retain(|s| &s.name != name);
            store.save(&servers);
            store.forget_data(name);
            println!(
                "removed '{name}': its token no longer works and its library copies and artist decisions are deleted"
            );
        }
        (Some("list"), _) => {
            for s in &servers {
                println!(
                    "{}  {}",
                    s.name,
                    if s.token.is_some() {
                        "paired"
                    } else {
                        "waiting for enrollment"
                    }
                );
            }
        }
        _ => eprintln!("usage: servers add|list|remove NAME"),
    }
}

fn serve(store: Store, bind: &str) {
    let mut servers = store.load();
    if servers.is_empty() {
        servers.push(Server {
            name: "default".into(),
            code: None,
            code_expires: 0,
            token: None,
        });
    }
    let mut waiting = vec![];
    for s in servers.iter_mut().filter(|s| s.token.is_none()) {
        waiting.push(format!(
            "server '{}'  code: {}",
            s.name.clone(),
            issue_code(s)
        ));
    }
    store.save(&servers);
    if !waiting.is_empty() {
        let mut lines = vec![
            "ENGINE READY - ACTION NEEDED: connect your music server".to_string(),
            String::new(),
        ];
        lines.extend(waiting);
        lines.push(String::new());
        lines.push("Paste the code and this engine's address into the plugin settings".to_string());
        lines.push("in the music server's web UI. The code expires in 15 minutes;".to_string());
        lines.push("restart the engine (or run `servers add NAME`) for a new one.".to_string());
        banner(&lines);
    }
    let http = tiny_http::Server::http(bind).expect("bind");
    println!("listening on {bind}");
    for mut request in http.incoming_requests() {
        let mut body = String::new();
        let _ = request.as_reader().read_to_string(&mut body);
        let bearer = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Authorization"))
            .and_then(|h| h.value.as_str().strip_prefix("Bearer ").map(str::to_string));
        let (status, reply) = route(&store, request.url(), &body, bearer);
        let _ = request.respond(tiny_http::Response::from_string(reply).with_status_code(status));
    }
}

fn route(store: &Store, url: &str, body: &str, bearer: Option<String>) -> (u16, String) {
    let mut servers = store.load();
    match url {
        "/enroll" => {
            let Ok(req) = serde_json::from_str::<EnrollRequest>(body) else {
                return (400, "bad request".into());
            };
            let Some(s) = servers
                .iter_mut()
                .find(|s| s.code.as_deref() == Some(req.code.as_str()) && s.code_expires > now())
            else {
                return (403, "unknown or expired code".into());
            };
            let token = random_text(40, b"abcdefghijklmnopqrstuvwxyz0123456789");
            s.token = Some(token.clone());
            s.code = None; // single use
            store.save(&servers);
            println!("server enrolled");
            (
                200,
                serde_json::to_string(&EnrollResponse { token }).unwrap(),
            )
        }
        "/compute" | "/library" | "/play" => {
            let Some(server) = bearer.and_then(|token| {
                servers
                    .iter()
                    .find(|s| s.token.as_deref() == Some(token.as_str()))
            }) else {
                return (401, "unauthorized".into());
            };
            if url == "/library" {
                let Ok(chunk) = serde_json::from_str::<LibraryChunk>(body) else {
                    return (400, "bad request".into());
                };
                return match store.receive(&server.name, chunk) {
                    Ok(()) => (200, "ok".into()),
                    Err(e) => (500, e),
                };
            }
            if url == "/play" {
                let Ok(report) = serde_json::from_str::<PlayReport>(body) else {
                    return (400, "bad request".into());
                };
                // A play that names no song (by id or by title) or no artist seeds nothing and would only take room.
                if report.user.trim().is_empty() || !report.play.is_usable() {
                    return (400, "a play needs a user, a song and an artist".into());
                }
                return match store.record_play(&server.name, &report.user, report.play) {
                    Ok(_) => (200, "ok".into()),
                    Err(e) => (500, e),
                };
            }
            let Ok(req) = serde_json::from_str::<ComputeRequest>(body) else {
                return (400, "bad request".into());
            };
            store.keep_recent(&server.name, &req.user, &req.recent);
            let answer = match store.library(&server.name, &req.user) {
                Some(copy) if copy.fingerprint == req.fingerprint => {
                    let decisions = store.decisions(&server.name, &req.user);
                    // The artists played most lately, and most of all; the albums the server lists as recent fill the
                    // places the plays leave, for the time before the engine was told about plays.
                    let played = store.plays(&server.name, &req.user);
                    let baseline = store.baseline(&server.name, &req.user);
                    let recent = plays::seed_artists(&played, baseline.as_ref(), &req.recent);
                    let mixes = mix::recent_artist_mixes(
                        &copy.songs,
                        &recent,
                        &played,
                        &decisions,
                        &store.lastfm().known,
                        MIX_SIZE,
                    )
                    .into_iter()
                    .enumerate()
                    .map(|(i, songs)| Mix {
                        playlist: format!("{MIX_PLAYLIST} {}", i + 1),
                        songs,
                    })
                    .collect();
                    ComputeResponse::Playlist {
                        playlist: PLAYLIST.into(),
                        songs: taste::more_of_what_you_star(
                            &copy.songs,
                            &req.starred,
                            &decisions,
                            PLAYLIST_SIZE,
                        ),
                        mixes,
                    }
                }
                _ => ComputeResponse::NeedLibrary,
            };
            (200, serde_json::to_string(&answer).unwrap())
        }
        _ => (404, "not found".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store in a fresh folder, with one paired server whose token is "tok".
    fn paired(name: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("smartplay-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let store = Store(dir.join("servers.json"));
        store.save(&[Server {
            name: "home".into(),
            code: None,
            code_expires: 0,
            token: Some("tok".into()),
        }]);
        store
    }

    fn song(id: &str, artist: &str) -> taste::Song {
        taste::Song {
            id: id.into(),
            artist: artist.into(),
            album: "Album".into(),
            title: format!("Title {id}"),
            ..Default::default()
        }
    }

    /// A song of the one style, so that the artists of a test that need to share a mix fit each other.
    fn rock(id: &str, artist: &str) -> taste::Song {
        taste::Song {
            genres: vec!["Rock".into()],
            ..song(id, artist)
        }
    }

    fn call(store: &Store, url: &str, body: String, token: Option<&str>) -> (u16, String) {
        route(store, url, &body, token.map(String::from))
    }

    fn compute(store: &Store, fingerprint: &str) -> ComputeResponse {
        compute_recent(store, fingerprint, &[])
    }

    fn compute_recent(store: &Store, fingerprint: &str, recent: &[&str]) -> ComputeResponse {
        let body = serde_json::to_string(&ComputeRequest {
            user: "ana".into(),
            starred: vec!["a1".into()],
            recent: recent.iter().map(|a| a.to_string()).collect(),
            fingerprint: fingerprint.into(),
        })
        .unwrap();
        let (status, reply) = call(store, "/compute", body, Some("tok"));
        assert_eq!(status, 200, "{reply}");
        serde_json::from_str(&reply).unwrap()
    }

    fn upload(store: &Store, fingerprint: &str, pages: &[Vec<taste::Song>]) {
        for (i, songs) in pages.iter().enumerate() {
            let body = serde_json::to_string(&LibraryChunk {
                user: "ana".into(),
                fingerprint: fingerprint.into(),
                first: i == 0,
                last: i + 1 == pages.len(),
                songs: songs.clone(),
            })
            .unwrap();
            assert_eq!(call(store, "/library", body, Some("tok")).0, 200);
        }
    }

    #[test]
    fn without_a_library_the_engine_asks_for_one() {
        let store = paired("ask");
        assert_eq!(compute(&store, "1@x"), ComputeResponse::NeedLibrary);
    }

    #[test]
    fn a_library_uploaded_in_pages_gives_a_playlist() {
        let store = paired("pages");
        upload(
            &store,
            "1@x",
            &[
                vec![song("a1", "Queen"), song("a2", "Queen")],
                vec![song("b1", "Portishead"), song("a3", "Queen")],
            ],
        );
        assert_eq!(
            compute(&store, "1@x"),
            ComputeResponse::Playlist {
                playlist: "smartplay".into(),
                songs: vec!["a2".into(), "a3".into()],
                mixes: vec![],
            }
        );
    }

    #[test]
    fn the_artists_played_lately_get_mixes_beside_the_playlist() {
        let store = paired("mixes");
        upload(
            &store,
            "1@x",
            &[vec![
                song("a1", "Queen"),
                song("a2", "Queen"),
                song("p1", "Portishead"),
            ]],
        );
        let ComputeResponse::Playlist { mixes, .. } =
            compute_recent(&store, "1@x", &["Portishead", "Nobody"])
        else {
            panic!("the library is there");
        };
        assert_eq!(
            mixes,
            vec![Mix {
                playlist: "smartplay: mix 1".into(),
                songs: vec!["p1".into()],
            }]
        );
    }

    /// A play `at` seconds after a moment two months ago: the history keeps two years, and a test's plays must be recent.
    fn report_play(store: &Store, user: &str, song: &str, artist: &str, at: i64) -> u16 {
        let body = serde_json::to_string(&PlayReport {
            user: user.into(),
            play: Play {
                song: song.into(),
                artists: vec![artist.into()],
                at: now() as i64 - 60 * 24 * 3600 + at,
                ..Play::default()
            },
        })
        .unwrap();
        call(store, "/play", body, Some("tok")).0
    }

    fn mix_songs(answer: ComputeResponse) -> Vec<Vec<String>> {
        let ComputeResponse::Playlist { mixes, .. } = answer else {
            panic!("the library is there");
        };
        mixes.into_iter().map(|m| m.songs).collect()
    }

    #[test]
    fn plays_the_engine_was_told_about_seed_the_mixes() {
        let store = paired("plays");
        upload(
            &store,
            "1@x",
            &[vec![
                song("a1", "Queen"),
                song("p1", "Portishead"),
                song("p2", "Portishead"),
            ]],
        );
        assert!(mix_songs(compute(&store, "1@x")).is_empty());
        assert_eq!(report_play(&store, "ana", "p1", "Portishead", 100), 200);
        assert_eq!(
            mix_songs(compute(&store, "1@x")),
            vec![vec!["p1".to_string(), "p2".to_string()]]
        );
    }

    #[test]
    fn plays_come_before_the_recent_albums_the_server_lists() {
        let store = paired("order");
        upload(
            &store,
            "1@x",
            &[vec![rock("a1", "Queen"), rock("p1", "Portishead")]],
        );
        report_play(&store, "ana", "p1", "Portishead", 100);
        // The album listing names Queen; the play says Portishead, which is the more recent artist.
        let mixes = mix_songs(compute_recent(&store, "1@x", &["Queen"]));
        assert_eq!(mixes, vec![vec!["p1".to_string(), "a1".to_string()]]);
    }

    #[test]
    fn plays_are_kept_per_user_and_a_repeated_report_counts_once() {
        let store = paired("plays-users");
        report_play(&store, "ana", "p1", "Portishead", 100);
        report_play(&store, "ana", "p1", "Portishead", 100);
        report_play(&store, "bea", "a1", "Queen", 100);
        assert_eq!(store.plays("home", "ana").len(), 1);
        assert_eq!(store.plays("home", "bea")[0].song, "a1");
        assert!(store.plays("home", "cris").is_empty());
    }

    #[test]
    fn a_play_report_that_is_not_one_is_refused() {
        let store = paired("bad-play");
        assert_eq!(call(&store, "/play", "{}".into(), Some("tok")).0, 400);
        // Well formed, but it names no song, no artist or no user: nothing to seed a mix with.
        assert_eq!(report_play(&store, "ana", "", "Queen", 1), 400);
        assert_eq!(report_play(&store, "ana", "a1", " ", 1), 400);
        assert_eq!(report_play(&store, " ", "a1", "Queen", 1), 400);
        assert!(store.plays("home", "ana").is_empty());
    }

    #[test]
    fn a_history_that_cannot_be_read_is_kept_aside_and_not_written_over() {
        let store = paired("corrupt");
        let legacy = store.plays_path("home", "ana").with_extension("json");
        fs::write(&legacy, "{ not a list").unwrap();
        assert_eq!(report_play(&store, "ana", "a1", "Queen", 1), 200);
        assert_eq!(store.plays("home", "ana").len(), 1);
        assert_eq!(
            fs::read_to_string(legacy.with_extension("bad")).unwrap(),
            "{ not a list"
        );
        store.forget_data("home");
        assert!(!legacy.with_extension("bad").exists());
    }

    #[test]
    fn a_history_kept_as_a_list_is_moved_into_the_file_of_lines_when_the_next_play_comes() {
        let store = paired("legacy");
        let legacy = store.plays_path("home", "ana").with_extension("json");
        let old = Play {
            song: "a1".into(),
            artists: vec!["Queen".into()],
            at: now() as i64 - 1000,
            ..Play::default()
        };
        fs::write(&legacy, serde_json::to_string(&[old]).unwrap()).unwrap();
        assert_eq!(report_play(&store, "ana", "p1", "Portishead", 1), 200);
        assert_eq!(store.plays("home", "ana").len(), 2);
        assert!(!legacy.exists());
    }

    #[test]
    fn an_import_adds_the_plays_of_a_file_once_and_names_the_lines_that_are_not_plays() {
        let store = paired("import");
        let at = now() - 3600;
        let text = format!(
            "{{\"at\":{at},\"artist\":\"Portishead\",\"title\":\"Glory Box\"}}\n\
             \n\
             not a play\n\
             {{\"at\":{},\"artist\":\"Portishead\",\"title\":\"Roads\",\"listened_ms\":9000,\"track_ms\":300000}}\n",
            at + 400
        );
        let first = store.import_plays("home", "ana", "spotify", &text);
        assert_eq!((first.read, first.added, first.invalid.len()), (2, 2, 1));
        assert_eq!(
            first.invalid[0].0, 3,
            "the line number counts the blank line"
        );
        let again = store.import_plays("home", "ana", "spotify", &text);
        assert_eq!((again.read, again.added), (2, 0));
        let kept = store.plays("home", "ana");
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[1].source, "import:spotify");
        assert_eq!(kept[1].ended(), Some(plays::Ended::Skip));
    }

    #[test]
    fn the_first_library_copy_gives_a_baseline_of_play_counts_and_later_ones_do_not_change_it() {
        let store = paired("baseline");
        let counted = |id: &str, count: Option<u32>| taste::Song {
            play_count: count,
            last_played: count.map(|_| "2026-11-20T08:33:59Z".to_string()),
            ..song(id, "Queen")
        };
        upload(
            &store,
            "1@x",
            &[vec![counted("a1", Some(7)), counted("a2", None)]],
        );
        let baseline = store.baseline("home", "ana").unwrap();
        assert_eq!(baseline.tallies.len(), 1, "an unplayed song is no tally");
        assert_eq!(
            (baseline.tallies[0].song.as_str(), baseline.tallies[0].count),
            ("a1", 7)
        );
        assert!(
            store.library("home", "ana").unwrap().songs[0]
                .play_count
                .is_none(),
            "the copy the mixes read holds no counts"
        );
        upload(&store, "2@y", &[vec![counted("a1", Some(9))]]);
        assert_eq!(store.baseline("home", "ana").unwrap().tallies[0].count, 7);
        store.forget_user("home", "ana");
        assert!(store.baseline("home", "ana").is_none());
    }

    #[test]
    fn what_last_fm_said_of_the_artists_widens_the_mixes_for_every_user() {
        let store = paired("lastfm");
        upload(
            &store,
            "1@x",
            &[vec![
                song("p1", "Portishead"),
                song("m1", "Massive Attack"),
                song("r1", "Radiohead"),
            ]],
        );
        report_play(&store, "ana", "p1", "Portishead", 100);
        assert_eq!(
            mix_songs(compute(&store, "1@x")),
            vec![vec!["p1".to_string()]]
        );

        let info = |tag: &str| smartplay_core::enrich::ArtistInfo {
            tags: vec![(tag.to_string(), 1.0)],
            similar: vec![],
        };
        let mut cache = lastfm::Cache::default();
        cache.known.insert("Portishead", info("trip hop"));
        cache.known.insert("Massive Attack", info("trip hop"));
        cache.known.insert("Radiohead", info("rock"));
        store.save_lastfm(&cache).unwrap();
        assert_eq!(
            mix_songs(compute(&store, "1@x")),
            vec![vec!["p1".to_string(), "m1".to_string()]]
        );
    }

    #[test]
    fn the_artists_of_the_recent_albums_the_plugin_sent_are_kept_for_eval_and_purged_with_the_user()
    {
        let store = paired("recent-kept");
        upload(&store, "1@x", &[vec![song("a1", "Queen")]]);
        compute_recent(&store, "1@x", &["Queen", "ABBA"]);
        assert_eq!(store.recent("home", "ana"), ["Queen", "ABBA"]);
        compute_recent(&store, "1@x", &[]);
        assert!(store.recent("home", "ana").is_empty());
        compute_recent(&store, "1@x", &["Queen"]);
        store.forget_user("home", "ana");
        assert!(store.recent("home", "ana").is_empty());
    }

    #[test]
    fn a_purge_deletes_one_user_and_leaves_the_others() {
        let store = paired("purge");
        upload(&store, "1@x", &[vec![song("a1", "Queen")]]);
        store.remember("home", "ana", |d| d.same("Queen", "Queen band"));
        report_play(&store, "ana", "a1", "Queen", 100);
        report_play(&store, "anabel", "a1", "Queen", 100);
        store.forget_user("home", "ana");
        assert!(store.library("home", "ana").is_none());
        assert!(store.plays("home", "ana").is_empty());
        assert_eq!(store.decisions("home", "ana"), Decisions::default());
        assert_eq!(
            store.plays("home", "anabel").len(),
            1,
            "a user whose name starts with the same letters is not touched"
        );
    }

    #[test]
    fn plays_of_songs_the_server_does_not_hold_are_kept() {
        let store = paired("elsewhere");
        let body = serde_json::to_string(&PlayReport {
            user: "ana".into(),
            play: Play {
                artists: vec!["Portishead".into()],
                title: "Glory Box".into(),
                at: now() as i64 - 100,
                source: "import:spotify".into(),
                ..Play::default()
            },
        })
        .unwrap();
        assert_eq!(call(&store, "/play", body, Some("tok")).0, 200);
        assert_eq!(store.plays("home", "ana")[0].title, "Glory Box");
    }

    #[test]
    fn an_artist_played_beside_a_recent_one_joins_the_mix_though_it_is_not_recent() {
        let store = paired("co-listening");
        // No tags anywhere: only what was played together can bring a song into the similar part.
        let mut songs = vec![song("old1", "Old Friend"), song("hermit1", "Hermit")];
        for i in 0..19 {
            songs.push(song(&format!("r{i}"), &format!("R{i}")));
        }
        songs.push(song("f1", "Friend"));
        upload(&store, "1@x", &[songs]);
        let day = 24 * 3600;
        report_play(&store, "ana", "hermit1", "Hermit", day); // alone
        report_play(&store, "ana", "old1", "Old Friend", 2 * day); // one sitting with Friend
        report_play(&store, "ana", "f1", "Friend", 2 * day + 60);
        for i in 0..19 {
            report_play(
                &store,
                "ana",
                &format!("r{i}"),
                &format!("R{i}"),
                (3 + i) * day,
            );
        }
        report_play(&store, "ana", "f1", "Friend", 30 * day); // Friend is recent again; Old Friend is the 21st artist
        let mixes = mix_songs(compute_recent(&store, "1@x", &[]));
        let flat: Vec<&String> = mixes.iter().flatten().collect();
        assert!(flat.iter().any(|id| *id == "old1"), "{mixes:?}");
        assert!(!flat.iter().any(|id| *id == "hermit1"), "{mixes:?}");
    }

    #[test]
    fn an_artist_in_the_plays_and_the_albums_takes_one_place_and_the_albums_fill_the_rest() {
        let store = paired("dedup");
        let songs: Vec<taste::Song> = ["P0", "N0", "N1", "N2"]
            .iter()
            // N2 is of no style, so that nothing brings it in as a similar song: it is in only if it is a seed.
            .map(|a| match *a {
                "N2" => song("n2", a),
                _ => rock(&a.to_lowercase(), a),
            })
            .collect();
        upload(&store, "1@x", &[songs]);
        report_play(&store, "ana", "p0", "P0", 100);
        // P0 is in the plays and first in the albums: it takes one of the three places for the artists played lately,
        // N0 and N1 fill the other two, and N2 is left out.
        let mixes = mix_songs(compute_recent(&store, "1@x", &["P0", "N0", "N1", "N2"]));
        let flat: Vec<&str> = mixes.iter().flatten().map(String::as_str).collect();
        assert!(flat.contains(&"n1"), "{mixes:?}");
        assert!(!flat.contains(&"n2"), "{mixes:?}");
    }

    #[test]
    fn the_artists_counted_before_the_engine_heard_of_plays_seed_the_mixes_as_the_most_played_of_all(
    ) {
        let store = paired("all-time");
        let counted = |id: &str, artist: &str, count: u32| taste::Song {
            play_count: Some(count),
            ..rock(id, artist)
        };
        let mut songs = vec![
            counted("fav1", "Old Favourite", 90),
            counted("mid1", "Mid", 50),
            // Not a seed, and of no style: nothing brings it in as a similar song.
            taste::Song {
                genres: vec![],
                ..counted("x1", "Hardly", 1)
            },
        ];
        songs.extend((0..3).map(|i| rock(&format!("r{i}"), &format!("R{i}"))));
        upload(&store, "1@x", &[songs]);
        for i in 0..3 {
            report_play(&store, "ana", &format!("r{i}"), &format!("R{i}"), 100 + i);
        }
        let flat: Vec<String> = mix_songs(compute(&store, "1@x"))
            .into_iter()
            .flatten()
            .collect();
        // The plays are older than the counts (they are in them), so the all-time two are the two most counted.
        assert!(flat.contains(&"fav1".to_string()), "{flat:?}");
        assert!(flat.contains(&"mid1".to_string()), "{flat:?}");
        assert!(!flat.contains(&"x1".to_string()), "{flat:?}");
    }

    #[test]
    fn a_new_fingerprint_asks_again_and_a_new_upload_replaces_the_copy() {
        let store = paired("stale");
        upload(
            &store,
            "1@x",
            &[vec![song("a1", "Queen"), song("a2", "Queen")]],
        );
        assert_eq!(compute(&store, "2@y"), ComputeResponse::NeedLibrary);
        upload(
            &store,
            "2@y",
            &[vec![song("a1", "Queen"), song("a9", "Queen")]],
        );
        assert_eq!(
            compute(&store, "2@y"),
            ComputeResponse::Playlist {
                playlist: "smartplay".into(),
                songs: vec!["a9".into()],
                mixes: vec![],
            }
        );
    }

    #[test]
    fn an_interrupted_upload_is_not_used() {
        let store = paired("partial");
        let body = serde_json::to_string(&LibraryChunk {
            user: "ana".into(),
            fingerprint: "1@x".into(),
            first: true,
            last: false,
            songs: vec![song("a1", "Queen")],
        })
        .unwrap();
        assert_eq!(call(&store, "/library", body, Some("tok")).0, 200);
        assert_eq!(compute(&store, "1@x"), ComputeResponse::NeedLibrary);
    }

    #[test]
    fn libraries_are_kept_per_user() {
        let store = paired("users");
        upload(
            &store,
            "1@x",
            &[vec![song("a1", "Queen"), song("a2", "Queen")]],
        );
        let body = serde_json::to_string(&ComputeRequest {
            user: "bob".into(),
            starred: vec![],
            recent: vec![],
            fingerprint: "1@x".into(),
        })
        .unwrap();
        let (_, reply) = call(&store, "/compute", body, Some("tok"));
        assert_eq!(
            serde_json::from_str::<ComputeResponse>(&reply).unwrap(),
            ComputeResponse::NeedLibrary
        );
    }

    #[test]
    fn requests_without_a_known_token_are_refused() {
        let store = paired("auth");
        for url in ["/compute", "/library", "/play"] {
            assert_eq!(call(&store, url, "{}".into(), None).0, 401);
            assert_eq!(call(&store, url, "{}".into(), Some("nope")).0, 401);
        }
    }

    #[test]
    fn removing_a_server_deletes_its_library_copies_and_decisions() {
        let store = paired("remove");
        upload(&store, "1@x", &[vec![song("a1", "Queen")]]);
        store.remember("home", "ana", |d| d.same("Queen", "Queen band"));
        report_play(&store, "ana", "a1", "Queen", 100);
        assert!(store.library("home", "ana").is_some());
        assert_eq!(store.plays("home", "ana").len(), 1);
        store.forget_data("home");
        assert!(store.library("home", "ana").is_none());
        assert!(store.plays("home", "ana").is_empty());
        assert_eq!(store.decisions("home", "ana"), Decisions::default());
    }

    #[test]
    fn a_decision_joins_two_spellings_in_the_playlist_of_that_user_only() {
        let store = paired("decide");
        upload(
            &store,
            "1@x",
            &[vec![
                song("a1", "Patricio Rey y sus Redonditos de Ricota"),
                song("a2", "Los Redondos"),
            ]],
        );
        let empty = ComputeResponse::Playlist {
            playlist: "smartplay".into(),
            songs: vec![],
            mixes: vec![],
        };
        assert_eq!(compute(&store, "1@x"), empty, "nothing joins them yet");

        store.remember("home", "bea", |d| {
            d.same("Los Redondos", "Patricio Rey y sus Redonditos de Ricota")
        });
        assert_eq!(compute(&store, "1@x"), empty, "another user's decision");

        store.remember("home", "ana", |d| {
            d.same("Los Redondos", "Patricio Rey y sus Redonditos de Ricota")
        });
        assert_eq!(
            compute(&store, "1@x"),
            ComputeResponse::Playlist {
                playlist: "smartplay".into(),
                songs: vec!["a2".into()],
                mixes: vec![],
            }
        );
    }

    #[test]
    fn the_data_flag_and_its_value_are_not_part_of_a_command() {
        let args: Vec<String> = ["same", "home", "ana", "A", "B", "--data", "/d"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            positional(&args),
            vec!["same", "home", "ana", "A", "B"],
            "the value after --data is not a word of the command"
        );
    }

    #[test]
    fn decisions_are_kept_between_runs_and_added_to() {
        let store = paired("keep");
        store.remember("home", "ana", |d| d.same("A", "B"));
        store.remember("home", "ana", |d| d.different("C", "D"));
        let d = store.decisions("home", "ana");
        assert_eq!(d.same_pairs().count(), 1);
        assert_eq!(d.different_pairs().count(), 1);
    }
}
