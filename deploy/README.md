# Putting smartplay on a Navidrome stack

Two parts: the **engine** (a container of the stack) and the **plugin** (a file Navidrome loads). The engine keeps each granted
user's library copy, history of plays and what Last.fm said of the artists, in one folder, and nothing leaves the stack unless
you run `enrich` or `import-lastfm`. The commands below run on the machine that hosts Navidrome, in the folder of its compose
file.

A release of the repository has the two parts: the engine as an image (`ghcr.io/ulises961/smartplay-engine:VERSION`) and the
plugin as `smartplay.ndp`. To build them yourself, from a clone (see `CONTRIBUTING.md`):

```bash
docker build -t smartplay-engine .             # the image; use it as `image:` in the compose file
cargo build --release -p smartplay-plugin --target wasm32-wasip1
cp target/wasm32-wasip1/release/smartplay_plugin.wasm plugin.wasm
zip -j smartplay.ndp crates/plugin/manifest.json plugin.wasm   # -j: the two files at the top of the zip, not in folders
```

## 1. Look first

```bash
docker exec navidrome /app/navidrome --version     # plugins need a Navidrome that has them (0.64.2 is the one tried)
docker network inspect navidrome_default --format '{{range .IPAM.Config}}{{.Subnet}}{{end}}'
```

The engine joins the network Navidrome is on (`navidrome_default` for a stack in a folder called `navidrome`; `docker inspect
navidrome` names it). The second command prints that network's range, for example `172.28.0.0/16`. Keep it: it goes into the
compose file (step 2) and into the plugin (step 3).

## 2. The engine

Add the service, the volume and the `networks` part of `compose.example.yaml` to the compose file, with the range from step 1
as the `subnet`. Pin **the range the network has now**: Compose recreates the network if the range differs, which restarts
every service on it, Navidrome included; with the same range nothing is touched (both checked with Docker 29.9).

```bash
docker compose up -d smartplay-engine
docker logs smartplay-engine          # a box with a one-time code (15 minutes); it is repeated on each start until enrolled
```

The engine's address on the stack is its service name, `smartplay-engine`: Docker's DNS gives it the engine's current IP, so
the address does not change when the container is recreated, nor when the stack goes down and up (the range is fixed).

## 3. The plugin

The plugin may only call the hosts in `requiredHosts` of its manifest, and nothing lets the administrator change that outside
the package. `smartplay.ndp` ships with only the engine's name, `smartplay-engine`, so out of the box it cannot reach any
address of your network. Navidrome checks two things on each call, and so the manifest needs two entries
([plugin docs](https://navidrome.org/docs/usage/features/plugins/), "Network Access to Local Services"): the host written in
the address, and, for a private address, an IP or a range that covers the address the host resolves to. In the docs' words, "a
host name in the manifest […] never allows a connection to a private, loopback or link-local address, even if that name
resolves to one."

Add the range from step 1 to the package and check it (`smartplay.ndp` is in the current folder; `-n` hides Navidrome's banner):

```bash
unzip -p smartplay.ndp manifest.json > manifest.json
# edit manifest.json: "requiredHosts": ["smartplay-engine", "172.28.0.0/16"]    (your range)
zip smartplay.ndp manifest.json               # replaces the manifest inside the package
docker run --rm -v "$PWD":/pkg:ro deluan/navidrome:0.64.2 plugin validate /pkg/smartplay.ndp -n
```

Which entries a setup needs:

| Where the engine is | Address in the plugin's settings | `requiredHosts` |
|---|---|---|
| The compose stack (this guide) | `http://smartplay-engine:7788` | `smartplay-engine` and the range of the stack |
| The host of Docker, port published | `http://172.17.0.1:7788` (the host's address on the bridge) | that IP, or a range that covers it |
| Another machine of your network | `https://192.168.1.50` | its IP, or a range that covers it |
| Another machine of your network, by name | `https://engine.lan` | the name, and a range that covers the IP it resolves to |
| A public host | `https://engine.example.com` | the name |

The address is the plugin's setting; the permission is the manifest's. An address the manifest does not list is refused
(`host "…" is not allowed`). The plugin runs inside Navidrome, whose client checks the server's certificate: an engine outside
the stack needs HTTPS with a certificate Navidrome trusts, for example behind a reverse proxy. The engine itself speaks plain
HTTP. `"*"` allows every host, private ones included: use it only if you accept that the plugin may reach your whole network.

Then install it:

```bash
cp smartplay.ndp data/plugins/               # `data` is the folder mounted as /data
```

Add `ND_PLUGINS_ENABLED: "true"` to the `navidrome` service if it is not there, and `docker compose up -d navidrome`
(Navidrome is off for the seconds this takes). Then:

```bash
docker exec navidrome /app/navidrome plugin list
docker exec navidrome /app/navidrome plugin edit smartplay --users USER_ID     # the user's id, not their name
docker exec navidrome /app/navidrome plugin enable smartplay
docker compose restart navidrome             # the running server learns of changes made by the command only on a restart
docker exec navidrome /app/navidrome plugin info smartplay                     # the user must be listed
```

The id of a user is in the address of their page in Navidrome's admin pages. `plugin edit --help` says "usernames", but on
0.64.2 only ids work: a user's name is accepted and grants nothing (tried, see `FINDINGS.md`). Replacing the `.ndp` later
leaves the plugin disabled: enable it again and restart Navidrome.

## 4. Enroll

In Navidrome, the plugin's settings: engine address `http://smartplay-engine:7788` and the code from the engine's log. Use
the name, not an IP: an IP set here goes stale when the engine container is recreated, and the plugin then logs `connection
refused` on every run. Within a minute the plugin sends the library, and `docker logs smartplay-engine` and Navidrome's log say
so. The playlists `smartplay`, the mixes of artists played lately and the moods (`smartplay: Focus`, …) appear for the granted user once there is something to put in them.

## 5. Look at what it does

```bash
docker exec smartplay-engine smartplay-engine eval default USER_NAME --data /state
```

`default` is the server's name in the engine and `USER_NAME` the user's name in Navidrome. It prints the seeds, the mixes, the
mood playlists and how well the mixes would have predicted the last two weeks of plays. Nothing is changed. Add
`--base-share 0.5` (or `--min-fit`, `--min-content`, `--min-aligned`, `--max-share`, `--max-mixes`) to try another value; once
you like it, write it in `tuning.json` in `/state` (see the README). `smartplay-engine moods --data /state` prints the moods
in use, to start a `moods.json` from.

## 6. History from outside, and what Last.fm knows

These are opt-in and need a Last.fm API key. Pass it for the one command, so that it is not kept on the machine:

```bash
docker exec -e SMARTPLAY_LASTFM_KEY=KEY smartplay-engine smartplay-engine import-lastfm default USER_NAME \
  --lastfm-user LASTFM_NAME --data /state          # what was heard anywhere; run it again for what is newer
docker exec -e SMARTPLAY_LASTFM_KEY=KEY smartplay-engine smartplay-engine enrich default USER_NAME \
  --data /state --limit 300                        # tags, close artists and most played tracks of the library's artists, a few hundred a run (three requests each)
```

`import-lastfm` sends the Last.fm name and nothing of the library; `enrich` sends artist names and nothing else. `import FILE`
adds plays from a file of JSON lines (`{"at": seconds, "artist": ..., "title": ...}`) with no network at all.

## Update

A new engine is a new `image:` tag and `docker compose up -d smartplay-engine`: the state is in the `smartplay-state` volume
and the plugin's address does not change. A new plugin is a new `smartplay.ndp`, patched as in step 3 and copied over the old
one; then enable it again and restart Navidrome.

## Undo

```bash
docker compose rm -sf smartplay-engine
docker exec navidrome /app/navidrome plugin disable smartplay
rm data/plugins/smartplay.ndp
docker volume rm NAME_OF_THE_STATE_VOLUME      # `docker volume ls` shows it; this deletes the engine's data
```

`purge default USER_NAME` deletes everything the engine holds about one user. The playlists the plugin wrote are the user's:
Navidrome removes them like any other.
