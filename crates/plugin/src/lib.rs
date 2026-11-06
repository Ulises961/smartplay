use extism_pdk::*;
use nd_pdk::host::{http, kvstore, scheduler, subsonicapi, users};
use nd_pdk::lifecycle::{Error as LifecycleError, InitProvider};
use nd_pdk::scheduler::{CallbackProvider, Error as SchedulerError, SchedulerCallbackRequest};
use smartplay_core::{library, playlist, protocol::*, Subsonic};

nd_pdk::register_lifecycle_init!(Smartplay);
nd_pdk::register_scheduler_callback!(Smartplay);

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
    if let Some(bytes) = kvstore::get(TOKEN_KEY).map_err(|e| e.to_string())? {
        return Ok(Some(String::from_utf8_lossy(&bytes).into_owned()));
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
fn engine_post(base: &str, path: &str, token: &str, body: &str) -> Result<String, String> {
    let (status, reply) = post(base, path, Some(token), body)?;
    match status {
        200 => Ok(reply),
        401 => {
            let _ = kvstore::delete(TOKEN_KEY); // revoked on the engine: ask for a new code
            Err("engine rejected the token; enroll again with a new code".into())
        }
        _ => Err(format!("engine answered {status}")),
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

impl CallbackProvider for Smartplay {
    fn on_callback(&self, _req: SchedulerCallbackRequest) -> Result<(), SchedulerError> {
        let Some(base) = config::get("engine_url")
            .ok()
            .flatten()
            .filter(|u| !u.is_empty())
        else {
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
