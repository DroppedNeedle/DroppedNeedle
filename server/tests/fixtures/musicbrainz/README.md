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
