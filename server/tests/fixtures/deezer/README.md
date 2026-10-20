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

The preview URLs in `search_artist.json` and `album_tracks.json` are signed (an
`hdnea` token) and expire, so the token in each of the 22 URLs is replaced by
`REDACTED`. The client resolves a preview URL just in time (see `preview.rs`).

All five bodies are unchanged except re-indented (two spaces) and those tokens.
No field was replaced for copyright: the answers hold titles, names, ids, flags
and links, and no lyric or biography text. None of them holds a key, a token or
personal data.
