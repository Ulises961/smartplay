//! Engine: registered servers, one-time enrollment codes, bearer tokens. Answers each granted user with a playlist.
//! State is a JSON file so the `servers` commands (separate processes) and `serve` share it.

use serde::{Deserialize, Serialize};
use smartplay_core::protocol::*;
use std::{
    fs,
    io::Read,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

const CODE_LIFETIME_SECS: u64 = 15 * 60;

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
            println!("removed '{name}': its token no longer works");
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
        "/compute" => {
            let known = bearer.is_some() && servers.iter().any(|s| s.token == bearer);
            if !known {
                return (401, "unauthorized".into());
            }
            let Ok(req) = serde_json::from_str::<ComputeRequest>(body) else {
                return (400, "bad request".into());
            };
            // The playlist is the starred songs in reverse order.
            let songs = req.starred.into_iter().rev().collect();
            (
                200,
                serde_json::to_string(&ComputeResponse {
                    playlist: "smartplay".into(),
                    songs,
                })
                .unwrap(),
            )
        }
        _ => (404, "not found".into()),
    }
}
