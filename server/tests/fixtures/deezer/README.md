# Deezer answers

Recorded answers from `https://api.deezer.com`, fetched on 2026-10-20 with the
app's User-Agent (`DroppedNeedle/3.0.0 ( contact@droppedneedle.com ; +https://github.com/DroppedNeedle/DroppedNeedle )`).
Deezer needs no key. The requests were sent at least three seconds apart, slower
than the five-a-second row in the rate table.

- `search_artist.json`: HTTP 200 for `GET /search?q=artist:"Queen"&limit=10`, the
  query the artist top-tracks call sends. Ten track hits.
- `search_track_no_hits.json`: HTTP 200 for
  `GET /search?q=artist:"Queen" track:"Bohemian Rhapsody"&limit=3`, the query the
  track preview call sends. Deezer had no hits: `{"data": [], "total": 0}`. A
  second track query for the same artist (`Radio Ga Ga`) had no hits that day
  too; its answer is not kept.
- `search_album.json`: HTTP 200 for `GET /search/album?q=artist:"Queen" album:"A Night at the Opera"&limit=3`,
  the album search. The first hit is album 1007321681, "A Night At The Opera" by Queen.
- `album_tracks.json`: HTTP 200 for `GET /album/1007321681/tracks?limit=12`, the
  twelve tracks of that album in order.
- `album_not_found.json`: HTTP 200 for `GET /album/99999999999/tracks?limit=4`, an
  album id Deezer does not hold. Deezer answers with an error object inside a
  200, not with a 404.

- `artist.json`: HTTP 200 for `GET /artist/415` (Nirvana), the artist picture
  call. Its `picture_*` URLs are on `cdn-images.dzcdn.net`; the 1000 px one
  answered 200 `image/jpeg` with no redirect. `picture` is
  `api.deezer.com/artist/415/image`, which answers a 302 to the same CDN URL.
- `artist_not_found.json`: HTTP 200 for `GET /artist/99999999999`, an artist id
  Deezer does not hold: the same error object, code 800.

- `track.json`: HTTP 200 for `GET /track/13791930` (Nirvana, "Smells Like Teen
  Spirit"), the song-link lookup, fetched on 2026-11-19. It carries the track's
  `isrc`, `duration` in seconds, its artist and its album.
- `track_not_found.json`: HTTP 200 for `GET /track/99999999999`, fetched on
  2026-11-19: the error object, code 800.
- `album_upc_12.json`: HTTP 200 for `GET /album/upc:634904032463` (In Rainbows, a
  12-digit barcode), fetched on 2026-11-21 during development with a research
  User-Agent, `dn-probe/0.1 ( research probe )`, not the app's. Album id 14880659.
  Tracks trimmed: `tracks.data` is empty and every other field is as recorded. The
  `utm_term` values in the `share` links are REDACTED.
- `album_upc_13.json`: HTTP 200 for `GET /album/upc:5099997855354` (Mellon Collie and
  the Infinite Sadness, Remastered, a 13-digit barcode), fetched on 2026-11-21 with the
  same User-Agent. Album id 6158273. Tracks trimmed as above; the `utm_term` values in
  the `share` links are REDACTED.
- `album_upc_13_leading_zero.json`: HTTP 200 for `GET /album/upc:0634904032463` (the
  GTIN-13 form of In Rainbows with a leading zero), fetched on 2026-11-22 with the same
  User-Agent. The error object, code 800. The 12-digit form of that album answers with
  the album (`album_upc_12.json`).
- `album_upc_14.json`: HTTP 200 for `GET /album/upc:05099997855354` (the 14-digit form
  of Mellon Collie), fetched on 2026-11-21 with the same User-Agent. The error object,
  code 800.
- `album_upc_unknown.json`: HTTP 200 for `GET /album/upc:999999999999`, a barcode
  Deezer does not hold, fetched on 2026-11-21 with the same User-Agent. The error
  object, code 800.

The preview URLs in `search_artist.json` and `album_tracks.json` are signed (an
`hdnea` token) and expire, so the token in each of the 22 URLs is replaced by
`REDACTED`. The client resolves a preview URL just in time (see `preview.rs`).

The tracking value in `artist.json`'s and `track.json`'s `share` links (`utm_term`)
is replaced by `REDACTED` too, and so are `track.json`'s preview token and
`track_token`. The two artist answers were fetched on 2026-10-20 as well.

The bodies above are unchanged except re-indented (two spaces), the trimmed track
lists and those values.
No field was replaced for copyright: the answers hold titles, names, ids, flags
and links, and no lyric or biography text. None of them holds a key, a token or
personal data.
