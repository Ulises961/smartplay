# Moods, tuning and `eval`

A **mood** is a recipe, and its playlist is `NAME - Smartplay`. The recipes are the engine's, so they live in the engine's
data folder, not in the plugin.

## The built-in moods

The recipes go by genre, because that is what most libraries have. The engine also uses what Last.fm says of an artist when
you ran `enrich` (see [engine.md](engine.md#lastfm-optional)).

| Mood | What is in it |
|---|---|
| `Classical` | Classical music (`Clásica` too), baroque, orchestral and the like. |
| `Focus` | Ambient, classical, instrumental, piano, lo-fi and the like; leaves out loud genres. |
| `Active` | Pop, dance, funk and the like; leaves out slow genres. It asks for more than a bare `pop` tag, so a pop artist's ballads stay out. |
| `Hits` | Pop that Last.fm says is played a lot: the most played fifth of the songs of the library it has a count for, the most played first. Needs `enrich`; until it has run, `Hits` has no playlist. |

A playlist holds the songs that fit the recipe and nothing else, the artists the user plays more first, and no artist takes
more than a fifth of it. A mood with fewer than 10 songs that fit gets no playlist. Songs that carry a BPM are also kept to the
tempo of the mood (`Active` is 115 and up, `Focus` is 100 and down); a library with no BPM tags is not held back by it.

What the moods cannot tell: Last.fm says how much a song is played, not when it came out, so `Hits` is not limited to recent
years unless you add `years_back` to its recipe (and your files carry years). Genres do not tell a ballad from a banger within
one genre: a BPM in the files does.

## Your own moods

`smartplay-engine moods` prints the recipes in use as JSON. Put your own, or the same with changes, in `moods.json` in the
engine's data folder. The file replaces the built-in recipes (an empty list turns the moods off) and is read on each run, so a
change applies within a minute. A recipe that is wrong is reported in the engine's log and the built-in ones stay in use.

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

- `tags` are the genres, moods and Last.fm tags that make a song fit, each with its weight. A song's fit is the sum of the
  weights of the tags it has, at most 1. A tag matches the song's tag when it is the same or one of its words, ignoring case
  and accents.
- `avoid` rules a song out; `min` is the fit a song needs (0.5 by default).
- `years_back` keeps the songs of this many years (this year counts as one); a song with no year is left out.
- `bpm` is `[low, high]`; a song with no BPM is not left out.
- `min_popularity` (0 to 1) keeps the songs Last.fm counts among that share of the most played (`0.8` is the most played
  fifth), and puts the most played first.

An artist called like a mood (a band called `Focus`) gets `Focus (artists) - Smartplay`, so that it does not share the mood's
name.

## The mixes of artists

Each user gets up to three mixes of the artists they played lately, named after them (`Queen, David Bowie +1 - Smartplay`).
The recent artists are grouped by how well they fit each other, so artists that sound unlike do not share a mix. About 40% of
a mix are songs of those artists and 60% are songs of others: the ones whose genres and moods are close, the ones that Last.fm
calls close, and the ones the user tends to play in the same sitting (plays less than an hour apart) as the artists of the
mix. A song only gets in if it fits well enough, so a mix may be shorter than 50 songs rather than filled with weak matches.

A mix is named after its artists, so when the artists played lately change, a new playlist appears and the old one stays as it
was: delete the ones you do not want.

## Tuning

What decides the mixes of artists can be tuned in `tuning.json` in the engine's data folder. Every field is optional, it is
read on each run, and a value that is not valid is reported and the defaults stand in:

| Field | Default | Meaning |
|---|---|---|
| `base_share` | 0.4 | The share of a mix that is of the artists played; the rest are similar songs. |
| `min_fit` | 0.5 | A similar song must score at least this to get in. |
| `min_content` | 0.45 | The part of the score that comes from the song's own sound (tags, close artists), without the sittings. |
| `min_aligned` | 0.4 | How well two groups of recent artists must fit to share a mix. |
| `max_artist_share` | 0.2 | The share of a mix one artist may take while others can fill it. |
| `max_mixes` | 3 | The most mixes of artists. |

## `eval`

`eval` prints what each mix and mood would be, with the reason of every song, how well the artists of each mix fit, and how
well the mixes would have predicted the last days of plays. Nothing is changed. It takes the tuning names as options
(`--base-share 0.5`, `--min-fit`, `--min-content`, `--min-aligned`, `--max-share`, `--max-mixes`, and `--size`,
`--holdout-days`) to try a value before you write it in `tuning.json`:

```bash
docker exec smartplay-engine smartplay-engine eval default USER_NAME --data /state
```

`default` is the server's name in the engine (`servers list`) and `USER_NAME` the user's name in Navidrome.
