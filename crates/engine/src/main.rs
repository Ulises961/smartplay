//! Engine: registered servers, one-time enrollment codes, bearer tokens. Answers each granted user with a playlist.
//! State is a JSON file so the `servers` commands (separate processes) and `serve` share it.

use serde::{Deserialize, Serialize};
use smartplay_core::{identity::Decisions, protocol::*, taste};
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
        let songs: Vec<taste::Song> = fs::read_to_string(&part)
            .map_err(|e| e.to_string())?
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
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

    /// Deletes every library copy a server holds.
    fn forget_libraries(&self, server: &str) {
        let prefix = format!("library-{}-", hex(server));
        let dir = self.0.parent().map(PathBuf::from).unwrap_or_default();
        let dir = if dir.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            dir
        };
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
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
        _ => eprintln!(
            "usage: smartplay-engine serve [--bind ADDR] [--data DIR] | servers add|list|remove NAME"
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
            store.forget_libraries(name);
            println!(
                "removed '{name}': its token no longer works and its library copies are deleted"
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
        "/compute" | "/library" => {
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
            let Ok(req) = serde_json::from_str::<ComputeRequest>(body) else {
                return (400, "bad request".into());
            };
            let answer = match store.library(&server.name, &req.user) {
                Some(copy) if copy.fingerprint == req.fingerprint => ComputeResponse::Playlist {
                    playlist: PLAYLIST.into(),
                    songs: taste::more_of_what_you_star(
                        &copy.songs,
                        &req.starred,
                        &Decisions::default(),
                        PLAYLIST_SIZE,
                    ),
                },
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
            genre: None,
            artist_mbid: None,
        }
    }

    fn call(store: &Store, url: &str, body: String, token: Option<&str>) -> (u16, String) {
        route(store, url, &body, token.map(String::from))
    }

    fn compute(store: &Store, fingerprint: &str) -> ComputeResponse {
        let body = serde_json::to_string(&ComputeRequest {
            user: "ana".into(),
            starred: vec!["a1".into()],
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
            }
        );
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
        for url in ["/compute", "/library"] {
            assert_eq!(call(&store, url, "{}".into(), None).0, 401);
            assert_eq!(call(&store, url, "{}".into(), Some("nope")).0, 401);
        }
    }

    #[test]
    fn removing_a_server_deletes_its_library_copies() {
        let store = paired("remove");
        upload(&store, "1@x", &[vec![song("a1", "Queen")]]);
        assert!(store.library("home", "ana").is_some());
        store.forget_libraries("home");
        assert!(store.library("home", "ana").is_none());
    }
}
