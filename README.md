# smartplay

[![ci](https://github.com/Ulises961/smartplay/actions/workflows/ci.yml/badge.svg)](https://github.com/Ulises961/smartplay/actions/workflows/ci.yml)

Smart playlists for every user of your [Navidrome](https://www.navidrome.org) server, from what they star and play, made on
your own machine. A Navidrome plugin and a small engine that runs beside it.

- **Mixes that follow what you play**: up to three playlists built on the artists you played last, widened with songs that
  sound close to them. They work for a user who has starred nothing.
- **Moods you can change**: `Focus`, `Classical`, `Active` and `Hits` out of the box, and your own recipes in a file.
- **Every song says why it is there**: `eval` prints the reason of each one. No black box.
- **Private**: no accounts, no telemetry, nothing leaves your machine unless you ask Last.fm yourself.

## What you get

Each user you grant finds these in any Subsonic client, rewritten in place within a minute of what they play:

| Playlist | What is in it |
|---|---|
| `smartplay` | 50 songs they have not starred, from the artists and genres they star most. |
| `Queen, David Bowie +1 - Smartplay` | A mix of the artists they played lately: 40% songs of those artists, 60% songs that fit them (close genres, artists played in the same sitting, artists Last.fm calls close). Up to three. |
| `Focus - Smartplay`<br>`Classical - Smartplay`<br>`Active - Smartplay`<br>`Hits - Smartplay` | One per mood: the songs that fit the recipe and nothing else, the artists they play more first. |

An artist counts once however the library spells the name, no artist plays out a whole playlist, and a mix is left short
rather than filled with weak matches. smartplay writes only the playlists it names and touches no other.

## How it works

```
Navidrome ── plugin (WASM, inside Navidrome) ──HTTP──► engine (a container of your stack)
  stars, plays, library      only the users you granted      keeps its copy of the library and the plays,
                                                             and answers with playlists
```

The plugin is the only part that talks to Navidrome. It sends the engine the library of each granted user (once, and again
only when a scan changes it), tells it about each finished play, and writes what the engine answers. The engine does the work.

## Requirements

- Navidrome **0.64.2** (the version it was tested with) with plugins enabled (`ND_PLUGINS_ENABLED=true`).
- Somewhere to run the engine: normally one more service of your Navidrome compose stack.

## Install

The [latest release](https://github.com/Ulises961/smartplay/releases/latest) has the plugin (`smartplay.ndp`); the engine is the
image `ghcr.io/ulises961/smartplay-engine`. [deploy/README.md](deploy/README.md) has every command; in short:

1. **Add the engine** to your compose file ([`deploy/compose.example.yaml`](deploy/compose.example.yaml)) on the stack's network,
   with the network's range pinned, and start it. It prints a one-time code in its log.
2. **Allow the plugin to reach it.** The plugin may call only the hosts its manifest lists, and the package lists only
   `smartplay-engine`. Navidrome also needs the stack's network range there, so you add it to the package once (commands in
   the deploy guide). It never gets more than that: no `*`, no reach into the rest of your network.
3. **Install it**: copy `smartplay.ndp` into Navidrome's plugins folder, grant the users (by id) and enable the plugin.
4. **Pair it**: in the plugin's settings put the engine address `http://smartplay-engine:7788` and the code from the engine's
   log.
5. **Listen.** Star or play something; the playlists appear within a minute.

## Make it yours

- **Moods and tuning**: change the recipes of the moods, the share of artists played in a mix and the thresholds with two small
  JSON files, and try values with `eval` before you keep them: [docs/moods.md](docs/moods.md).
- **Artist names**: when the library spells one artist several ways, you decide which are one and which are two:
  [docs/engine.md](docs/engine.md#artist-names).
- **Last.fm (optional)**: tags, close artists and play counts make the mixes better and give `Hits` its ranking. You run it
  yourself with your own key: [docs/engine.md](docs/engine.md#lastfm-optional).

## Privacy

Self-hosted software: the authors run no service, receive no data and include no telemetry. Everything stored stays on the
machine where you run it, in the engine's data folder:

- each granted user's library (their songs as the server lists them) and their decisions about artist names;
- **a history of their plays, kept for two years and then dropped**: for each play, the song, its artists, the time it started
  and, when the source says, the title, album, length, how much was heard, and where the play was reported from. This is a
  history of listening, and it is what the mixes are made from;
- the play counts of the first copy of the library the engine received, and the artists of the albums the plugin last listed as
  recent;
- the tags, close artists and most played tracks Last.fm gave for artist names, if you ran `enrich`, in one file for every
  user, `enrichment-lastfm.json`: delete the file to remove them.

`servers remove NAME` deletes the library copies, decisions, plays, play counts and recent artists of that server's users, and
`purge SERVER USER` does the same for one user. Nothing leaves the machine except what you send to Last.fm yourself and the
plugin's calls to the engine. You, as the operator, are responsible for how you use this software, including informing the
users of your server.

## Limits

- Tested with Navidrome 0.64.2 only.
- The moods go by genre, because that is what most libraries have. A genre does not tell a ballad from a banger: a BPM in your
  files does, and the recipes use it when it is there.
- Last.fm says how much a song is played, not when it came out, so `Hits` means "the most played pop of your library".
- A mix is named after its artists, so when the artists you played lately change, a new playlist appears and the old one stays
  until you delete it.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). To build the engine and the plugin from source, see
[docs/engine.md](docs/engine.md) and the deploy guide.

## Licence

[GPL-3.0-or-later](LICENSE). The plugin links Navidrome's plugin SDK, which is GPL-3.0, so the plugin as built is distributed
under GPL-3.0.
