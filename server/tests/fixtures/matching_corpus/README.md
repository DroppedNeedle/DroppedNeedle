# Matching corpus

Real albums from a test library, replayed through recall, scoring and the
decision by `tests/library/library_matching_corpus.rs`. The test runs
offline and prints one row per album and copy, then a summary per copy
(an example, from when the corpus was recorded):

```
tagged: exact 38, group only 7, review 1, unmatched 6, wrong 0
```

The facts were read from a test library's catalog, and the answers
fetched read-only on 2026-10-22 from the BrainzMash mirror
(`https://api.brainzmash.cc/ws/2`, the same ws/2 shapes as
musicbrainz.org).

## Files

- `albums.json`: one entry per album.
  - `facts`: the album as identification reads it from the catalog
    (`LocalAlbumFacts`). Album and track ids are placeholders; file paths
    and stat revisions are left out. The three albums whose row an upgrade
    left on "Unknown Artist" (`crimson-king`, `close-to-the-edge`,
    `back-street-crawler-cd1`) show the row after the catalog cleanup gave
    it its tracks' artist.
  - `truth`: the release group and release the album is. `release` is null
    when the files cannot tell its editions apart.
  - `tagged` and `stripped`: the recorded result for the album as tagged,
    and for a copy with every MusicBrainz id taken off its files. Each
    row has a `verdict`, the `release`
    sealed or guessed, and a `known_miss` saying why it falls short of the
    truth.
- Four albums carry the edition tags their files hold (media, barcode,
  catalog number, country, disc total): `fallen-20th`, `the-wall`,
  `from-me-to-u` and `violence-against-nature`. Stripped copies keep
  them, so the edition sort by barcode and format is exercised there.
  The other albums carry none.
- `musicbrainz.jsonl`: every MusicBrainz answer recall asks for, one per
  line, keyed by the exact request URL on the BrainzMash mirror, trimmed to
  what matching reads. MusicBrainz data is CC0. Release search pages
  recorded before 2026-10-22 carry no hit titles; only the loose word
  search reads them, and its answers were recorded after that.
- `record.py`: records missing answers.

## Verdicts

Best first: `exact` (confirmed on the true release), `group` (confirmed in
the true release group, on another edition or where the edition is not
known), `review` (a best guess sealed unconfirmed), `unmatched` (nothing
sealed), `wrong` (confirmed on another release group).

The test fails when a row comes out worse, and also when it comes out
better, so a change that improves matching records the rows it flips in
the same commit. A row that reaches the truth must drop its `known_miss`.
It also fails on a request with no recorded answer.

## Fingerprints

The corpus holds no AcoustID answers. `fingerprints_confirm_and_never_veto`
replays it with AcoustID made to hear, in every file, a recording no
candidate carries (the one in `fixtures/acoustid/batch_lookup.json`),
and in each stripped file the recording it pairs with on its release, or
the next file's. It prints a summary per scenario.

## Recording answers for new requests

A change that makes recall ask something new (another query, another
lookup) leaves those requests unrecorded. To record them, from `server/`:

```
cargo test --test library library_matching_corpus -- --nocapture > /tmp/corpus.log
python3 -I tests/fixtures/matching_corpus/record.py /tmp/corpus.log tests/fixtures/matching_corpus/musicbrainz.jsonl
```

Repeat until the run lists no `UNRECORDED` line: each answer can lead to
further requests. The script only sends read-only GETs to the mirror, one
at a time.

## Adding an album

Copy the album's facts from the catalog the way `album_facts` reads them,
replace ids and paths, say why it is here, set its truth, record its
answers, then record its rows from the test's output.
