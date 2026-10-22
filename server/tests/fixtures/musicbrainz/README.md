# MusicBrainz release documents

`release_standard.json`, `release_remaster.json` and `release_search.json`
are written by hand and their ids are made up. They are kept because
`server/tests/library/library_matching.rs` needs two editions of one release
group under ids that no answer carries (`c0ffee00-...`), and a search that
finds both. No recording has that case.

`spotify_search_*.json` and `spotify_release_*.json` are written by hand
too, with made-up ids, for the two albums of the Spotify mock's playlist:
a release search and a release lookup with
`inc=recordings+isrcs+release-groups` for each. _Neon Meridian_'s release
carries the ISRC Spotify gives for its track; _Glass Tides_' release
carries another one, so its name matches and nothing proves it. The mock
keeps that case, which no recording has.

`release_relations_classical.json` and `release_relations_pop.json` are
recorded answers, fetched read-only on 2026-10-15 from the BrainzMash
mirror (`https://api.brainzmash.cc/ws/2`, the same ws/2 shapes as
musicbrainz.org) with the app's User-Agent:

- `release/7ce871a0-34b2-40e0-a77e-39b1268f1f61` (Gould and Karajan,
  Beethoven Piano Concerto no. 3 and Sibelius Symphony no. 5)
- `release/863644bd-df97-3259-a17c-e0d51a6ffcc0` (Lady Gaga, _The Remix_,
  US edition)

Both with `inc=recordings+artist-rels+recording-level-rels+work-rels+work-level-rels`,
re-indented and otherwise unchanged. The classical answer was 28 KB in
1.2 s and the pop one 39 KB in 1.6 s. A 30-track release with about 470
credits (The Beatles, 1993 GB) was 315 KB in 2.8 s.

`release_names_*.json` are recorded answers to the artist-names lookup,
fetched the same way on 2026-10-15, re-indented and otherwise unchanged:

- `release/82316878-b6ff-44da-8be3-96c253d36cc7` (Ryuichi Sakamoto +
  Shiro Takatani, _async surround_, credited in Latin script, with no
  aliases for Takatani), `inc=recordings+artist-credits+aliases`: 74 KB
  in 0.6 s
- `release/d4ed5eba-fa9d-4325-8279-093fccdb4a45` (Utada Hikaru,
  _Fantôme_, featured artists, regional aliases), same includes: 48 KB
  in 1.7 s
- `release/7ce871a0-34b2-40e0-a77e-39b1268f1f61` (the classical release
  above) with `inc=recordings+artist-credits+aliases+artist-rels+recording-level-rels+work-rels+work-level-rels`,
  the names and credits in one lookup: 69 KB in 0.5 s

The artists of the release's and each release track's credit carry an
`aliases` list (`[]` for none); recording-level credits and relationship
artists carry none. Aliases have null `primary`, `locale` and `type-id`
when unset. All three are far under the 20 s planning call limit and the
read cap.

# MusicBrainz artist lookups

`artist_merged_*.json` are recorded answers to artist lookups with no
includes, fetched read-only on 2026-10-17 from BrainzMash
(`https://api.brainzmash.cc/ws/2`) with the app's User-Agent, one request
at a time, redirects not followed. The merged id came from Wikidata's
deprecated MusicBrainz artist ids marked as redirects; the mirror's own
answer is what shows the merge.

- `artist/3ec87112-9081-467b-8327-e18286f1b020` (merged away): `301`,
  `Location: https://api.brainzmash.cc/ws/2/artist/ffbcb0c1-37ad-4df8-a78d-e31bd590d7ea?fmt=json`
  (absolute, same origin, with `?fmt=json`). The body is the kept
  artist's document: `artist_merged_redirect.json`.
- `artist/ffbcb0c1-37ad-4df8-a78d-e31bd590d7ea` (Ewald Heidepriem, the
  kept id): `200`, a document whose `id` is the id asked about:
  `artist_merged_canonical.json`.
- `artist/5b0c7e7a-3f0e-4d4a-9a51-2f6c1d8e9b30` (well formed, names
  nothing): `404`, `artist_merged_missing.json`.

Re-indented and otherwise unchanged. The official service
(`https://musicbrainz.org/ws/2`) reset the connection from this host that
day, so only the mirror's shapes are recorded.

# Recorded 2026-10-21 from the BrainzMash mirror

The official service reset the connection on the first request of 2026-10-21,
so these answers come from the BrainzMash mirror
(`https://api.brainzmash.cc/ws/2`), not from musicbrainz.org. Each was read
with a GET that carries the app's User-Agent and `fmt=json`, one request at a
time, four seconds apart. Each body is unchanged except re-indented. None
holds a key, a token or personal data.

- `release_lookup_classical.json`: `release/7ce871a0-34b2-40e0-a77e-39b1268f1f61`
  with `inc=artist-credits+genres+isrcs+labels+recordings+release-groups+url-rels`.
  HTTP 200. The include sets `release-groups`, `url-rels`, `recordings`,
  `labels+recordings+url-rels`, `artist-credits+labels+recordings+release-groups+url-rels`,
  `artist-credits+genres+isrcs+labels+recordings+release-groups` and
  `isrcs+recordings+release-groups` are subsets of it, so this answer covers them.
- `recording_lookup_url_rels.json`: `recording/33ea7e59-3691-4414-92d3-6bce48d5d9cc`
  with `inc=url-rels`. HTTP 200.
- `recording_lookup_plain.json`: the same recording with no include. HTTP 200.
- `release_group_lookup_ok_computer.json`: `release-group/b1392450-e666-3926-a536-22c65f834433`
  with `inc=artist-credits+media+releases+tags+url-rels`. HTTP 200. The include
  sets `artist-credits+media+releases+url-rels` and
  `artist-credits+releases+tags+url-rels` are subsets of it.
- `artist_lookup_radiohead.json`: `artist/a74b1b7f-71a5-4011-9441-d0b5e4122711`
  with `inc=aliases+tags+url-rels`. HTTP 200. The include set `url-rels` is a
  subset of it.

- `url_lookup_discogs_release.json`: `url?resource=https://www.discogs.com/release/249504`
  with `inc=release-group-rels+release-rels`. HTTP 200. The include sets
  `release-rels` and `release-group-rels` are subsets of it.
- `search_artist_radiohead.json`: `artist?query=` with the text search for
  Radiohead and `limit=5`. HTTP 200.
- `search_release_group_ok_computer.json`: `release-group?query=` with the text
  search for OK Computer and `limit=5`. HTTP 200.
- `search_release_ok_computer.json`: `release?query=release:"OK Computer" AND
  artist:"Radiohead"` with `limit=5`. HTTP 200.
- `browse_release_groups_radiohead.json`: `release-group?artist=a74b1b7f-71a5-4011-9441-d0b5e4122711`
  with `inc=artist-credits`, `limit=5` and `offset=0`. HTTP 200.

# Recorded 2026-10-22 from the BrainzMash mirror

Read the same way, one GET with the app's User-Agent and `fmt=json`, body
unchanged except re-indented (12 KB in 0.5 s):

- `search_recording_tuesday_morning.json`: `recording?query=recording:"Tuesday Morning"
  AND artist:"Paul Kossoff"` with `limit=3`. HTTP 200. Each recording lists
  the releases carrying it; a release lists only the medium holding the
  recording, and its own `track-count` is the whole release's.

# Not kept

The same `url` include set for `https://en.wikipedia.org/wiki/OK_Computer`
answered 404 on 2026-10-21. Its body is not kept: the Discogs lookup above is
the recorded `url` answer.
