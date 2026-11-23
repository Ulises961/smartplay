# Putting smartplay on a Navidrome stack

Two parts: the **engine** (a container of the stack) and the **plugin** (a file Navidrome loads). The engine keeps each granted
user's library copy, history of plays and what Last.fm said of the artists, in one folder, and nothing leaves the stack unless
you run `enrich` or `import-lastfm`. The commands below run on the machine that hosts Navidrome, in the folder of its compose
file, as the user that owns that folder. The engine is built on the development machine:

```bash
cargo build --release -p smartplay-engine
cargo build --release -p smartplay-plugin --target wasm32-wasip1
# the plugin is a zip of its manifest and the program
cp target/wasm32-wasip1/release/smartplay_plugin.wasm plugin.wasm
zip -j smartplay.ndp crates/plugin/manifest.json plugin.wasm   # -j: the two files at the top of the zip, not in folders
```

The engine runs in `debian:bookworm-slim`, whose glibc is 2.36: check that the program asks for no newer one
(`objdump -T target/release/smartplay-engine | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -1`).

## 1. Look first

```bash
docker exec navidrome /app/navidrome --version     # plugins need a Navidrome that has them (0.64.2 is the one tried)
docker inspect navidrome --format '{{range $k,$v := .NetworkSettings.Networks}}network={{$k}} ip={{$v.IPAddress}}{{end}}'
```

The engine joins the network Navidrome is on (`navidrome_default` for a stack in a folder called `navidrome`). The plugin may
only call the hosts its manifest lists, so before building the plugin put that network's range in
`crates/plugin/manifest.json`, under `permissions.http.requiredHosts` (for a network of `172.22.0.0/16`:
`"requiredHosts": ["172.22.0.0/16"]`). Read the range with `docker network inspect NETWORK --format '{{range .IPAM.Config}}{{.Subnet}}{{end}}'`.

The range is what authorizes the engine: Navidrome never lets a host name authorize a private address on its own, but it
checks the address the name resolves to when the connection opens. So the plugin can call the engine by its name (step 4),
and a recreated container keeps working as long as it stays in that range.

## 2. The engine

```bash
mkdir -p smartplay/state
# copy smartplay-engine to smartplay/, and add the service of compose.example.yaml to the compose file
chmod 755 smartplay/smartplay-engine
docker compose up -d smartplay-engine
docker logs smartplay-engine          # a box with a one-time code (15 minutes); it is repeated on each start until enrolled
```

The engine's address on the stack is its name, `smartplay-engine` (the `container_name` of the compose file): Docker resolves
it to the engine's current IP on the network, so it does not change when the container is recreated.

## 3. The plugin

Add `ND_PLUGINS_ENABLED: "true"` to the `navidrome` service if it is not there, and `docker compose up -d navidrome`
(Navidrome is off for the seconds this takes). Then:

```bash
cp smartplay.ndp data/plugins/               # `data` is the folder mounted as /data
docker exec navidrome /app/navidrome plugin list
docker exec navidrome /app/navidrome plugin edit smartplay --users USER_ID     # the user's id, not their name
docker exec navidrome /app/navidrome plugin enable smartplay
docker compose restart navidrome             # the running server learns of changes made by the command only on a restart
docker exec navidrome /app/navidrome plugin info smartplay                     # the user must be listed
```

The id of a user is in the address of their page in Navidrome's admin pages. A user's name granted by mistake is accepted
and grants nothing.

## 4. Enroll

In Navidrome, the plugin's settings: engine address `http://smartplay-engine:7788` and the code from the engine's log. Use the name, not an IP: an IP set here
goes stale when the engine container is recreated, and the plugin then logs `connection refused` on every run. Within a
minute the plugin sends the library, and `docker logs smartplay-engine` and Navidrome's log say so. The playlists `smartplay`
and `smartplay: mix 1`/`2` appear for the granted user once there is something to put in them.

## 5. Look at what it does

```bash
docker exec smartplay-engine /opt/smartplay-engine eval default USER_NAME --data /state
```

`default` is the server's name in the engine and `USER_NAME` the user's name in Navidrome. It prints the seeds, the mixes and
how well they would have predicted the last two weeks of plays. Nothing is changed.

## 6. History from outside, and what Last.fm knows

These are opt-in and need a Last.fm API key. Pass it for the one command, so that it is not kept on the machine:

```bash
docker exec -e SMARTPLAY_LASTFM_KEY=KEY smartplay-engine /opt/smartplay-engine import-lastfm default USER_NAME \
  --lastfm-user LASTFM_NAME --data /state          # what was heard anywhere; run it again for what is newer
docker exec -e SMARTPLAY_LASTFM_KEY=KEY smartplay-engine /opt/smartplay-engine enrich default USER_NAME \
  --data /state --limit 300                        # tags and close artists of the library's artists, a few hundred a run
```

`import-lastfm` sends the Last.fm name and nothing of the library; `enrich` sends artist names and nothing else. `import FILE`
adds plays from a file of JSON lines (`{"at": seconds, "artist": ..., "title": ...}`) with no network at all.

## Undo

```bash
docker compose rm -sf smartplay-engine
docker exec navidrome /app/navidrome plugin disable smartplay
rm -r smartplay data/plugins/smartplay.ndp
```

`purge default USER_NAME` deletes everything the engine holds about one user. The playlists the plugin wrote are the user's:
Navidrome removes them like any other.
