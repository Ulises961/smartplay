# smartplay

Playlists for your listeners, written by an engine that runs beside your music server. Works with Navidrome through a
plugin.

- **Engine** (`crates/engine`): a small HTTP service. It pairs with your server through a one-time code and answers each
  user the plugin sends it with a playlist.
- **Plugin** (`crates/plugin`): a Navidrome plugin. It is the only part that talks to the server. It acts only for the
  users you granted it, reads their starred songs, asks the engine and writes the playlist.
- **Core** (`crates/core`): the shared library, with no I/O. The wire protocol, an idempotent playlist writer, text
  normalization and artist identity resolution (one artist spelled several ways), with tests. It compiles into both the
  engine and the plugin (`wasm32-wasip1`).

Each granted user gets a playlist called `smartplay`: 50 songs they have not starred, from the artists and genres they
star most. An artist counts once however the library spells the name, and no artist plays out the whole list. A user
with no stars gets no playlist. The playlist is checked every minute and written by id, so it is updated in place and
left alone when nothing changed. smartplay touches no other playlist.

The engine needs each user's library for this. The plugin sends it once, in pages, and again only when Navidrome's scan
changes the library. The engine keeps one copy per server and user in its data folder.

## Install

You need Rust (with the `wasm32-wasip1` target: `rustup target add wasm32-wasip1`), `git`, `zip` and Navidrome 0.64.2
(the version it was tested with) with plugins enabled (`ND_PLUGINS_ENABLED=true`).

```bash
./scripts/fetch-deps.sh      # Navidrome's plugin SDK (GPL-3.0, not on crates.io) into .deps/
```

### Engine

```bash
cargo build --release -p smartplay-engine
mkdir -p data
./target/release/smartplay-engine serve --bind 0.0.0.0:7788 --data ./data
```

On first start the engine creates a server entry called `default` and prints a banner with a one-time code, valid for
15 minutes. It prints a new one on every start while no server is paired. Do not publish the engine's port to the
internet; if the engine runs on another host than Navidrome, put it behind HTTPS.

```bash
./target/release/smartplay-engine servers list              # paired or waiting for enrollment
./target/release/smartplay-engine servers add NAME          # another server entry, with a fresh code
./target/release/smartplay-engine servers remove NAME       # revokes its token at once; deletes its library copies, decisions, plays and play counts
```

A library often names one artist several ways (`Los Redondos`, `Patricio Rey y sus Redonditos de Ricota`). The engine
joins the spellings that differ only in case, accents or `&`/`y`/`and`, and the ones with strong evidence; the rest it
only suggests. You decide the rest, per user, and the decision is kept in the engine's data folder (names in the
library are never changed):

```bash
./target/release/smartplay-engine artists list SERVER USER                    # decisions, joined spellings, suggestions
./target/release/smartplay-engine artists same SERVER USER "NAME" "NAME"      # one artist
./target/release/smartplay-engine artists different SERVER USER "NAME" "NAME" # two artists, and stop suggesting them
```

`SERVER` is the name from `servers list`; `USER` is the Navidrome user name. The next playlist uses the decision.

### Plugin

```bash
cargo build -p smartplay-plugin --release --target wasm32-wasip1
cp target/wasm32-wasip1/release/smartplay_plugin.wasm plugin.wasm
zip -j smartplay.ndp crates/plugin/manifest.json plugin.wasm
```

The plugin may only call the hosts that `requiredHosts` lists in its manifest, and Navidrome never lets a host name alone
reach a private address. The package ships with only `smartplay-engine` (the engine's name on a compose stack), so before
copying it, add the IP address or range of the engine's network to the manifest inside `smartplay.ndp`, or the name or
address of an engine that runs elsewhere. [deploy/README.md](deploy/README.md), step 3, has the commands and a table of
cases.

Copy `smartplay.ndp` into Navidrome's plugins folder, then in Navidrome's admin pages:

1. Open the plugin's permissions and choose the users it may act for. With the command line, pass user **ids**, not
   user names: a name is accepted and grants nothing.
2. Set **Engine address** (for example `http://172.17.0.1:7788`) and **Enrollment code** (from the engine's banner).
3. Enable the plugin. Changes made with the command line apply after a restart of Navidrome.

The plugin exchanges the code for a token and keeps it in its own storage, so the code is needed once, and updating the
`.ndp` keeps the pairing. Replacing the `.ndp` leaves the plugin disabled: enable it again. If you remove the server on
the engine, the plugin drops its token and logs `engine rejected the token`; enter a new code to pair again.

## Use

Star songs in any Subsonic client. Within a minute each granted user finds a playlist named `smartplay`. Grant or
withdraw users in Navidrome's plugin permissions; nothing runs for a user who is not granted.

Play some songs and each user also finds up to three playlists named after the artists they are built on, such as
`smartplay: mix of Queen, David Bowie +1`, of up to 50 songs each, built from the artists they played last. The recent
artists are grouped by how well they fit each other, so artists that sound unlike do not share a mix, and one artist does
not take a mix over. About 40% of a mix are songs of those artists, and 60% are songs of other artists: the ones whose
genres and moods are close, the ones that outside services (see Last.fm below) call close, and the ones the user tends to
play in the same sitting (plays less than an hour apart) as the artists of the mix. A song only gets in if it fits well
enough, so a mix may be shorter than 50 songs rather than filled with weak matches. They work for a user who has starred
nothing, and they are ordinary playlists too. A mix is named after the artists it is built on, so when the artists played
lately change, a new playlist appears and the old one stays as it was: delete the ones you do not want.

Beside them, each user gets a playlist for each **mood**: `smartplay: Active`, `smartplay: Classical`, `smartplay: Focus`
and `smartplay: Hits`. A mood is a recipe of tags (the genres and moods of the songs and what Last.fm says of their
artists), and its playlist holds the songs that fit it and nothing else, the artists the user plays more first, with no
artist taking more than a fifth of it. A mood with fewer than 10 songs that fit gets no playlist. The built-in recipes go by
genre, because that is what most libraries have: `Classical` is classical music (`Clásica` too), `Focus` is classical,
instrumental, ambient and the like, `Active` is pop, dance and the like (and leaves out slow genres), and `Hits` is pop that
Last.fm says is played a lot: the most played fifth of the songs of the library it has a count for, the most played first.
That needs `enrich` (see Last.fm below); until it has run, `Hits` has no playlist. Last.fm tells how much a song is played,
not when it came out, so `Hits` is not limited to the last years unless you add `years_back` to its recipe (and the files
carry years). Songs that carry a BPM are also kept to the tempo of the mood (`Active`
is 115 and up, `Focus` is 100 and down); a library with no BPM tags is not held back by it.

The recipes are the engine's, not the plugin's. `smartplay-engine moods` prints them as JSON; put your own, or the same with
changes, in `moods.json` in the engine's data folder. The file replaces the built-in recipes (an empty list turns the moods
off) and is read on each run, so a change applies within a minute, and a recipe that is wrong is reported in the engine's log
and the built-in ones stay in use:

```json
[
  {
    "name": "Tango night",
    "tags": { "tango": 1.0, "milonga": 1.0, "vals": 0.6 },
    "avoid": ["rock"],
    "min": 0.5,
    "years_back": null,
    "bpm": null
  }
]
```

`tags` are the genres, moods and Last.fm tags that make a song fit, each with its weight (a song's fit is the sum of the
weights of the tags it has, at most 1), `avoid` rules a song out, `min` is the fit a song needs, `years_back` keeps the songs
of this many years (this year counts as one), `bpm` is `[low, high]` and `min_popularity` (0 to 1) keeps the songs Last.fm
counts among that share of the most played, and puts the most played first. A tag matches the song's tag when it is the same or
one of its words, ignoring case and accents. The playlist is `smartplay: ` and the name.

What decides the mixes of artists can be tuned too, in `tuning.json` in the same folder (every field is optional, and a value
that is not valid is reported and the defaults stand in): `base_share` (the share of a mix that is of the artists played, 0.4),
`min_fit`, `min_content`, `min_aligned`, `max_artist_share` and `max_mixes`. `eval` takes the same names as options (see
below), so a value can be tried before it is written down.

To see why a song is in a mix, run `eval` (see [deploy/README.md](deploy/README.md)): it prints each mix with the reason
of every song, how well the artists of each mix fit, and how well the mixes would have predicted the last days of plays.
It can also try other thresholds, such as `--base-share 0.5`, without a new build, and it prints the mood playlists too.

### Last.fm (optional)

Two commands of the engine use Last.fm, and nothing else does. Both are run by the operator, with the operator's own API
key, and neither runs on its own:

- `enrich` asks Last.fm for the tags, the close artists and the most played tracks of the artists of a library. It sends
  artist names and your API key, and nothing else, to Last.fm. What it answers widens the mixes for every user, and the
  play counts rank the `Hits` mood. Artists asked about by an earlier version are asked again, only for their most played
  tracks.
- `import-lastfm` brings the history of a user's Last.fm account in as plays. It sends the Last.fm user name and your API
  key, and nothing of the library.

What Last.fm gives is Last.fm's, and its [API terms](https://www.last.fm/api/tos) apply to your key: non-commercial use, and
credit to Last.fm. smartplay stores the tags, close artists and most played tracks for the artists of your own library, a
small part of the 100 MB the terms allow.

`smartplay` is an ordinary playlist, so Navidrome's smart playlists can use it as their source with `inPlaylist`, for
example to keep only unplayed songs, or songs of one genre, in random order:

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

## Privacy

Self-hosted software: the authors run no service, receive no data and include no telemetry. Everything stored stays on
the machine where you run it, in the engine's data folder:

- each granted user's library (their songs as the server lists them) and their decisions about artist names;
- **a history of their plays, kept for two years and then dropped**: for each play, the song, its artists, the time it
  started and, when the source says, the title, album, length, how much was heard, and where the play was reported from
  (the plugin, or an import). This is a history of listening, and it is what the mixes are made from;
- the play counts of the first copy of the library the engine received, and the artists of the albums the plugin last
  listed as recent;
- the tags, close artists and most played tracks Last.fm gave for artist names, if you ran `enrich`. They are kept in one file for every user,
  `enrichment-lastfm.json`, and they are not removed by the commands below: delete the file to remove them.

Removing a server on the engine (`servers remove NAME`) deletes the library copies, decisions, plays, play counts and recent
artists of that server's users, and `purge SERVER USER` does the same for one user. Nothing leaves the machine except what
you send to Last.fm yourself (see above), and the plugin's calls to the engine. You, as the operator, are responsible for
how you use this software, including informing the users of your server.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## Licence

[GPL-3.0-or-later](LICENSE). The plugin links Navidrome's plugin SDK, which is GPL-3.0, so the plugin as built is
distributed under GPL-3.0.
