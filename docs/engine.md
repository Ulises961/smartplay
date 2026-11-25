# Running the engine, and its commands

The engine is a small HTTP service: it pairs with your server through a one-time code and answers each user the plugin sends
it with playlists. On a Navidrome stack it runs as a container (see [deploy/README.md](../deploy/README.md)). To run it
without Docker:

```bash
cargo build --release -p smartplay-engine
mkdir -p data
./target/release/smartplay-engine serve --bind 0.0.0.0:7788 --data ./data
```

On first start the engine creates a server entry called `default` and prints a banner with a one-time code, valid for 15
minutes. It prints a new one on every start while no server is paired. Do not publish the engine's port to the internet; if
the engine runs on another host than Navidrome, put it behind HTTPS.

```bash
smartplay-engine servers list              # paired or waiting for enrollment
smartplay-engine servers add NAME          # another server entry, with a fresh code
smartplay-engine servers remove NAME       # revokes its token at once; deletes its library copies, decisions, plays and play counts
```

The commands take `--data DIR` for the engine's data folder (`/state` in the container).

## Artist names

A library often names one artist several ways (`Los Redondos`, `Patricio Rey y sus Redonditos de Ricota`). The engine joins
the spellings that differ only in case, accents or `&`/`y`/`and`, and the ones with strong evidence; the rest it only suggests.
You decide the rest, per user, and the decision is kept in the engine's data folder (names in the library are never changed):

```bash
smartplay-engine artists list SERVER USER                    # decisions, joined spellings, suggestions
smartplay-engine artists same SERVER USER "NAME" "NAME"      # one artist
smartplay-engine artists different SERVER USER "NAME" "NAME" # two artists, and stop suggesting them
```

`SERVER` is the name from `servers list`; `USER` is the Navidrome user name. The next playlist uses the decision.

## Plays and data

```bash
smartplay-engine import SERVER USER FILE [--source NAME]   # plays from a file of JSON lines, no network
smartplay-engine purge SERVER USER                          # deletes everything held about one user
```

`import` reads lines like `{"at": seconds, "artist": ..., "title": ...}`; a play already kept is not added again.

## Last.fm (optional)

Two commands use Last.fm, and nothing else does. Both are run by the operator, with the operator's own API key (passed for the
one command in `SMARTPLAY_LASTFM_KEY`, so that it is not kept on the machine), and neither runs on its own:

- `enrich SERVER USER [--limit N]` asks Last.fm for the tags, the close artists and the most played tracks of the artists of a
  library, a few hundred a run (three requests each), the ones the user hears most first. It sends artist names and your API
  key, and nothing else. What it answers widens the mixes for every user, and the play counts rank the `Hits` mood.
- `import-lastfm SERVER USER --lastfm-user NAME [--pages N]` brings the history of a user's Last.fm account in as plays. It
  sends the Last.fm user name and your API key, and nothing of the library.

What Last.fm gives is Last.fm's, and its [API terms](https://www.last.fm/api/tos) apply to your key: non-commercial use, and
credit to Last.fm. smartplay stores the tags, close artists and most played tracks for the artists of your own library, a
small part of the 100 MB the terms allow.

## Using a playlist as a source for a smart playlist

`smartplay` is an ordinary playlist, so Navidrome's smart playlists can use it as their source with `inPlaylist`, for example to
keep only unplayed songs, or songs of one genre, in random order:

```json
{
  "name": "Fresh from smartplay",
  "all": [
    { "inPlaylist": { "id": "<id of the smartplay playlist>" } },
    { "is": { "genre": "Rock" } },
    { "lt": { "playcount": 1 } }
  ],
  "sort": "random",
  "limit": 10
}
```
