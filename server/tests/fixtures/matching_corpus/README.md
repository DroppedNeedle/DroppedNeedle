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
    and stat revisions are left out.
  - `truth`: the release group and release the album is. `release` is null
    when the files cannot tell its editions apart.
  - `tagged` and `stripped`: the recorded result for the album as tagged,
    and for a copy with every MusicBrainz id taken off its files. Each
    row has a `verdict`, the `release`
    sealed or guessed, and a `known_miss` saying why it falls short of the
    truth.
- No album carries edition tags (media, barcode, catalog number,
  country, disc total), so the edition sort by barcode and format is not
  exercised here.
- `musicbrainz.jsonl`: every MusicBrainz answer recall asks for, one per
  line, keyed by the exact request URL on the BrainzMash mirror, trimmed to
  what matching reads. MusicBrainz data is CC0.
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
