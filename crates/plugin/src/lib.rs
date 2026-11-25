use extism_pdk::*;
use nd_pdk::host::{http, kvstore, scheduler, subsonicapi, users};
use nd_pdk::lifecycle::{Error as LifecycleError, InitProvider};
use nd_pdk::scheduler::{CallbackProvider, Error as SchedulerError, SchedulerCallbackRequest};
use nd_pdk::scrobbler::{
    Error as ScrobbleError, IsAuthorizedRequest, NowPlayingRequest, PlaybackReportRequest,
    ScrobbleRequest, Scrobbler, SCROBBLER_ERROR_NOT_AUTHORIZED, SCROBBLER_ERROR_RETRY_LATER,
    SCROBBLER_ERROR_UNRECOVERABLE,
};
use smartplay_core::{library, playlist, plays::Play, protocol::*, Subsonic};

nd_pdk::register_lifecycle_init!(Smartplay);
nd_pdk::register_scheduler_callback!(Smartplay);
nd_pdk::register_scrobbler!(Smartplay);

const TOKEN_KEY: &str = "engine_token";
/// Albums asked for to learn the artists played lately.
const RECENT_ALBUMS: usize = 20;

#[derive(Default)]
struct Smartplay;

/// Navidrome's in-process Subsonic API, as seen from the plugin sandbox.
struct HostApi;

impl Subsonic for HostApi {
    fn call(&self, request: &str) -> Result<String, String> {
        subsonicapi::call(request).map_err(|e| e.to_string())
    }
}

impl InitProvider for Smartplay {
    fn on_init(&self) -> Result<(), LifecycleError> {
        scheduler::schedule_recurring("@every 1m", "tick", "smartplay-tick")
            .map_err(|e| LifecycleError::new(e.to_string()))?;
        info!("smartplay plugin loaded");
        Ok(())
    }
}

/// POSTs JSON to the engine; returns the status and body.
fn post(base: &str, path: &str, token: Option<&str>, body: &str) -> Result<(i32, String), String> {
    let mut headers = std::collections::HashMap::new();
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    if let Some(t) = token {
        headers.insert("Authorization".to_string(), format!("Bearer {t}"));
    }
    let request = http::HTTPRequest {
        method: "POST".into(),
        url: format!("{}{}", base.trim_end_matches('/'), path),
        headers,
        body: body.as_bytes().to_vec(),
        timeout_ms: 8000,
        ..Default::default()
    };
    let response = http::send(request)
        .map_err(|e| e.to_string())?
        .ok_or("no response")?;
    Ok((
        response.status_code,
        String::from_utf8_lossy(&response.body).into_owned(),
    ))
}

/// The stored token, or a new one from the one-time enrollment code in the settings.
fn engine_token(base: &str) -> Result<Option<String>, String> {
    if let Some(token) = stored_token()? {
        return Ok(Some(token));
    }
    let Some(code) = config::get("enrollment_code")
        .ok()
        .flatten()
        .filter(|c| !c.is_empty())
    else {
        return Ok(None);
    };
    let body = serde_json::to_string(&EnrollRequest { code }).unwrap();
    let (status, reply) = post(base, "/enroll", None, &body)?;
    if status != 200 {
        return Err(format!("enrollment refused ({status}): {reply}"));
    }
    let token = serde_json::from_str::<EnrollResponse>(&reply)
        .map_err(|e| e.to_string())?
        .token;
    kvstore::set(TOKEN_KEY, token.clone().into_bytes()).map_err(|e| e.to_string())?;
    info!("enrolled with the engine");
    Ok(Some(token))
}

/// POSTs to the engine with the token and returns the body of a 200 answer.
fn engine_post(base: &str, path: &str, token: &str, body: &str) -> Result<String, EngineError> {
    let (status, reply) = post(base, path, Some(token), body).map_err(EngineError::Unreachable)?;
    match status {
        200 => Ok(reply),
        401 => {
            let _ = kvstore::delete(TOKEN_KEY); // revoked on the engine: ask for a new code
            Err(EngineError::Rejected)
        }
        _ => Err(EngineError::Refused(status)),
    }
}

/// Why a call to the engine failed. The scrobbler tells the server which failures are worth retrying.
enum EngineError {
    /// No answer: the engine is down or the network failed.
    Unreachable(String),
    /// The token was revoked on the engine; it has been dropped.
    Rejected,
    /// The engine answered with this status.
    Refused(i32),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Unreachable(e) => write!(f, "{e}"),
            EngineError::Rejected => {
                write!(f, "engine rejected the token; enroll again with a new code")
            }
            EngineError::Refused(status) => write!(f, "engine answered {status}"),
        }
    }
}

impl From<EngineError> for String {
    fn from(e: EngineError) -> String {
        e.to_string()
    }
}

fn ask(base: &str, token: &str, request: &ComputeRequest) -> Result<ComputeResponse, String> {
    let reply = engine_post(
        base,
        "/compute",
        token,
        &serde_json::to_string(request).unwrap(),
    )?;
    serde_json::from_str(&reply).map_err(|e| e.to_string())
}

/// Sends the user's library to the engine, one page at a time. Returns the number of songs.
fn upload_library(base: &str, token: &str, user: &str, fingerprint: &str) -> Result<usize, String> {
    let mut sent = 0;
    loop {
        let songs = library::songs(&HostApi, user, sent)?;
        let chunk = LibraryChunk {
            user: user.into(),
            fingerprint: fingerprint.into(),
            first: sent == 0,
            last: songs.len() < library::PAGE,
            songs,
        };
        engine_post(
            base,
            "/library",
            token,
            &serde_json::to_string(&chunk).unwrap(),
        )?;
        sent += chunk.songs.len();
        if chunk.last {
            return Ok(sent);
        }
    }
}

fn run_user(base: &str, token: &str, user: &str) -> Result<String, String> {
    let fingerprint = library::fingerprint(&HostApi, user)?;
    // The mixes are an extra: a server that cannot tell what was played lately still gets the main playlist.
    let recent = library::recent_artists(&HostApi, user, RECENT_ALBUMS).unwrap_or_else(|e| {
        warn!("smartplay: {user}: recent albums unavailable: {e}");
        vec![]
    });
    let request = ComputeRequest {
        user: user.into(),
        starred: smartplay_core::starred_ids(&HostApi, user)?,
        recent,
        fingerprint: fingerprint.clone(),
    };
    let mut answer = ask(base, token, &request)?;
    if answer == ComputeResponse::NeedLibrary {
        let sent = upload_library(base, token, user, &fingerprint)?;
        info!("smartplay: {user}: library sent to the engine ({sent} songs)");
        answer = ask(base, token, &request)?;
    }
    match answer {
        ComputeResponse::Playlist {
            playlist,
            songs,
            mixes,
        } => {
            let mut outcomes = vec![format!(
                "{playlist}: {:?}",
                playlist::write_playlist(&HostApi, user, &playlist, &songs)?
            )];
            for mix in mixes {
                // One failing mix does not stop the others.
                outcomes.push(
                    match playlist::write_playlist(&HostApi, user, &mix.playlist, &mix.songs) {
                        Ok(outcome) => format!("{}: {outcome:?}", mix.playlist),
                        Err(e) => format!("{}: {e}", mix.playlist),
                    },
                );
            }
            Ok(outcomes.join(", "))
        }
        ComputeResponse::NeedLibrary => Err("the engine asks for the library again".into()),
    }
}

/// The engine address from the plugin settings, when one is set.
fn engine_url() -> Option<String> {
    config::get("engine_url")
        .ok()
        .flatten()
        .filter(|u| !u.is_empty())
}

/// The token kept after enrolling, without trying to enroll: the scrobbler calls must not depend on the engine being up.
fn stored_token() -> Result<Option<String>, String> {
    let bytes = kvstore::get(TOKEN_KEY).map_err(|e| e.to_string())?;
    Ok(bytes.map(|b| String::from_utf8_lossy(&b).into_owned()))
}

/// The scrobbler capability tells the engine about each finished play, so the mixes follow what is played right now.
/// Only the finished play matters: a song that is playing, paused or skipped says little about taste.
impl Scrobbler for Smartplay {
    /// The server only sends plays of users the admin granted, and only once the plugin is enrolled.
    fn is_authorized(&self, req: IsAuthorizedRequest) -> Result<bool, ScrobbleError> {
        let granted = match users::get_users() {
            Ok(users) => users.iter().any(|u| u.user_name == req.username),
            Err(e) => {
                warn!("smartplay: cannot list the granted users: {e}");
                false
            }
        };
        // Without a token there is nowhere to send a play yet: the plugin enrolls on its next tick, and the albums the
        // server lists as recent cover the plays of that gap.
        let enrolled = stored_token().unwrap_or_else(|e| {
            warn!("smartplay: cannot read the engine token: {e}");
            None
        });
        Ok(granted && enrolled.is_some())
    }

    fn now_playing(&self, _req: NowPlayingRequest) -> Result<(), ScrobbleError> {
        Ok(())
    }

    fn playback_report(&self, _req: PlaybackReportRequest) -> Result<(), ScrobbleError> {
        Ok(())
    }

    /// An engine that cannot be reached now, or that failed, is "retry later": the server keeps the play and sends it
    /// again, and the engine counts a play it receives twice once. A report the engine refuses will never be accepted,
    /// and a revoked token needs a new enrollment: the server is told so and does not keep retrying.
    fn scrobble(&self, req: ScrobbleRequest) -> Result<(), ScrobbleError> {
        let retry_later = || ScrobbleError::new(SCROBBLER_ERROR_RETRY_LATER);
        let token = stored_token().map_err(|e| {
            warn!("smartplay: cannot read the engine token: {e}");
            retry_later()
        })?;
        let (Some(base), Some(token)) = (engine_url(), token) else {
            return Err(retry_later());
        };
        let credited: Vec<String> = req.track.artists.iter().map(|a| a.name.clone()).collect();
        let mut play = Play::new(&req.track.id, &credited, &req.track.artist, req.timestamp);
        play.title = req.track.title.clone();
        play.album = req.track.album.clone();
        // The server scrobbles a finished play: how much was heard is not told, but how long the song is, is.
        play.track_ms = (req.track.duration > 0.0).then(|| (req.track.duration * 1000.0) as u64);
        play.source = "scrobbler".into();
        let report = PlayReport {
            user: req.username.clone(),
            play,
        };
        engine_post(
            &base,
            "/play",
            &token,
            &serde_json::to_string(&report).unwrap(),
        )
        .map(|_| ())
        .map_err(|e| {
            warn!("smartplay: {}: play not delivered: {e}", req.username);
            match e {
                EngineError::Rejected => ScrobbleError::new(SCROBBLER_ERROR_NOT_AUTHORIZED),
                EngineError::Refused(status) if (400..500).contains(&status) => {
                    ScrobbleError::new(SCROBBLER_ERROR_UNRECOVERABLE)
                }
                _ => retry_later(),
            }
        })
    }
}

impl CallbackProvider for Smartplay {
    fn on_callback(&self, _req: SchedulerCallbackRequest) -> Result<(), SchedulerError> {
        let Some(base) = engine_url() else {
            warn!("smartplay: engine_url is not set");
            return Ok(());
        };
        let token = match engine_token(&base) {
            Ok(Some(t)) => t,
            Ok(None) => {
                warn!("smartplay: not enrolled; paste an enrollment code in the plugin settings");
                return Ok(());
            }
            Err(e) => {
                error!("smartplay: {e}");
                return Ok(());
            }
        };
        let granted = users::get_users().unwrap_or_default();
        for user in granted {
            match run_user(&base, &token, &user.user_name) {
                Ok(outcome) => info!("smartplay: {} -> {outcome}", user.user_name),
                Err(e) => error!("smartplay: {}: {e}", user.user_name),
            }
        }
        Ok(())
    }
}
