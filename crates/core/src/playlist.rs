//! Idempotent playlist writing: one playlist per name, updated by id, untouched when already right.

use crate::{encode, Subsonic};
use serde::Deserialize;

#[derive(Deserialize)]
struct Listing {
    #[serde(rename = "subsonic-response")]
    response: ListingBody,
}

#[derive(Deserialize)]
struct ListingBody {
    #[serde(default)]
    playlists: Playlists,
    #[serde(default)]
    playlist: Option<Detail>,
}

#[derive(Deserialize, Default)]
struct Playlists {
    #[serde(default)]
    playlist: Vec<Summary>,
}

#[derive(Deserialize)]
struct Summary {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct Detail {
    #[serde(default)]
    entry: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    id: String,
}

/// What a write did.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Created,
    Updated,
    Unchanged,
    /// Nothing to write: an empty result never replaces an existing playlist.
    Skipped,
}

fn parse(body: &str) -> Result<ListingBody, String> {
    serde_json::from_str::<Listing>(body)
        .map(|l| l.response)
        .map_err(|e| format!("bad response: {e}"))
}

fn songs_query(ids: &[String]) -> String {
    ids.iter()
        .map(|id| format!("&songId={}", encode(id)))
        .collect()
}

/// Makes the playlist called `name` hold exactly `ids`, in order.
pub fn write_playlist(
    api: &dyn Subsonic,
    user: &str,
    name: &str,
    ids: &[String],
) -> Result<Outcome, String> {
    if ids.is_empty() {
        return Ok(Outcome::Skipped);
    }
    let u = encode(user);
    let all = parse(&api.call(&format!("getPlaylists?u={u}"))?)?;
    let Some(found) = all.playlists.playlist.into_iter().find(|p| p.name == name) else {
        api.call(&format!(
            "createPlaylist?u={u}&name={}{}",
            encode(name),
            songs_query(ids)
        ))?;
        return Ok(Outcome::Created);
    };
    let current = parse(&api.call(&format!("getPlaylist?u={u}&id={}", encode(&found.id)))?)?;
    let current: Vec<String> = current
        .playlist
        .map(|d| d.entry.into_iter().map(|e| e.id).collect())
        .unwrap_or_default();
    if current == ids {
        return Ok(Outcome::Unchanged);
    }
    api.call(&format!(
        "createPlaylist?u={u}&playlistId={}{}",
        encode(&found.id),
        songs_query(ids)
    ))?;
    Ok(Outcome::Updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A one-playlist server: remembers what was written.
    #[derive(Default)]
    struct Server(RefCell<Option<Vec<String>>>);

    impl Subsonic for Server {
        fn call(&self, request: &str) -> Result<String, String> {
            let ok = |inner: &str| {
                Ok(format!(
                    r#"{{"subsonic-response":{{"status":"ok"{inner}}}}}"#
                ))
            };
            if request.starts_with("getPlaylists") {
                return match &*self.0.borrow() {
                    Some(_) => ok(r#","playlists":{"playlist":[{"id":"p1","name":"Mix"}]}"#),
                    None => ok(r#","playlists":{}"#),
                };
            }
            if request.starts_with("getPlaylist?") {
                let entries: Vec<String> = self
                    .0
                    .borrow()
                    .iter()
                    .flatten()
                    .map(|id| format!(r#"{{"id":"{id}"}}"#))
                    .collect();
                return ok(&format!(
                    r#","playlist":{{"entry":[{}]}}"#,
                    entries.join(",")
                ));
            }
            let ids = request
                .split("&songId=")
                .skip(1)
                .map(str::to_string)
                .collect();
            *self.0.borrow_mut() = Some(ids);
            ok("")
        }
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn create_then_update_then_unchanged() {
        let s = Server::default();
        assert_eq!(
            write_playlist(&s, "me", "Mix", &ids(&["a", "b"])).unwrap(),
            Outcome::Created
        );
        assert_eq!(
            write_playlist(&s, "me", "Mix", &ids(&["a", "b"])).unwrap(),
            Outcome::Unchanged
        );
        assert_eq!(
            write_playlist(&s, "me", "Mix", &ids(&["b", "c"])).unwrap(),
            Outcome::Updated
        );
        assert_eq!(s.0.borrow().clone().unwrap(), ids(&["b", "c"]));
    }

    #[test]
    fn empty_result_never_wipes() {
        let s = Server::default();
        write_playlist(&s, "me", "Mix", &ids(&["a"])).unwrap();
        assert_eq!(
            write_playlist(&s, "me", "Mix", &[]).unwrap(),
            Outcome::Skipped
        );
        assert_eq!(s.0.borrow().clone().unwrap(), ids(&["a"]));
    }
}
