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
./target/release/smartplay-engine servers remove NAME       # revokes its token at once; deletes its library copies and decisions
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

Navidrome may only reach a private engine address that the plugin lists. Put the engine's IP address (or CIDR) in
`requiredHosts` in `crates/plugin/manifest.json` (it ships with `172.17.0.1`, Docker's default host address), then:

```bash
cargo build -p smartplay-plugin --release --target wasm32-wasip1
cp target/wasm32-wasip1/release/smartplay_plugin.wasm plugin.wasm
zip -j smartplay.ndp crates/plugin/manifest.json plugin.wasm
```

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
the machine where you run it. You, as the operator, are responsible for how you use this software, including informing
the users of your server.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## Licence

[GPL-3.0-or-later](LICENSE). The plugin links Navidrome's plugin SDK, which is GPL-3.0, so the plugin as built is
distributed under GPL-3.0.
