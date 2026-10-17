# MusicBrainz release documents

`release_standard.json`, `release_remaster.json` and `release_search.json`
are written by hand and their ids are made up.

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
