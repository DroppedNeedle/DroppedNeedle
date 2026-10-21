# Catalog fixtures

The `mb_*.json` files are MusicBrainz answers that the catalog page tests
read through the in-repo mock (`server/testkit/src/musicbrainz.rs`). Their
origin is unknown. They were added with the catalog page tests on 2026-10-06
as recorded payloads, and no request, date, server or version was noted with
them. None is known to be made up, and none is marked as recorded.

- `mb_artist.json`: Radiohead, for `artist/{mbid}` with
  `inc=aliases+tags+url-rels`.
- `mb_artist_release_groups.json`: Radiohead's release groups, for
  `release-group?artist={mbid}` with `inc=artist-credits`.
- `mb_release_cd.json`: the OK Computer CD, for `release/{mbid}` with
  `inc=labels+recordings+url-rels`.
- `mb_release_group.json`: OK Computer, for `release-group/{mbid}` with
  `inc=artist-credits+media+releases+url-rels`.
- `mb_search_artists.json`: an artist search, with no include.
- `mb_search_release_groups.json`: a release-group search, with no include.

None is re-recorded here. The answers recorded on 2026-10-21 come from the
BrainzMash mirror (see `server/tests/fixtures/musicbrainz/README.md`). The
catalog tests read these values, so the files stay as they are.
