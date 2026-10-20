# iTunes answers

Recorded answers from `https://itunes.apple.com/search`, fetched on 2026-10-20
with the app's User-Agent (`DroppedNeedle/3.0.0 ( contact@droppedneedle.com ; +https://github.com/DroppedNeedle/DroppedNeedle )`).
The Search API needs no key. The four requests were sent at least nine seconds
apart, slower than the 0.3-a-second row in the rate table.

The client sends three searches, and each has a fixture:

- `search_album.json`: HTTP 200 for `GET /search?term=Nirvana+Nevermind&entity=album&country=US&limit=10`,
  the album search. Ten hits, all `wrapperType` `collection`. The album Nevermind
  by Nirvana is not among them: the hits include tribute and same-name albums by
  other artists.
- `search_track.json`: HTTP 200 for `GET /search?term=Nirvana+Smells+Like+Teen+Spirit&entity=song&limit=5`,
  the track preview search. Five song hits.
- `search_album_tracks.json`: HTTP 200 for `GET /search?term=Nirvana+Nevermind&entity=song&limit=25`,
  the album track list. 25 song hits.
- `search_no_results.json`: HTTP 200 for `GET /search?term=zqxjvwkp+nonexistent+release+qqz&entity=album&country=US&limit=10`,
  a search with no hits: `{"resultCount": 0, "results": []}`.

Each answer is sent as `content-type: text/javascript; charset=utf-8`, but the
body is plain JSON.

The bodies are unchanged except re-indented (two spaces). No field was replaced
for copyright: the answers hold names, ids, store and preview links, prices and
dates, and no lyric, biography or review text. The preview URLs carry no query
string, and the store links carry only `uo=4` and, on track rows, `i=<track id>`.
No signed token appears, so nothing is replaced with `REDACTED`. None of the
bodies holds a key, a token or personal data.

No error answer is kept. The client reads an iTunes failure by its status only,
so the status cases in `providers_catalog.rs` keep their own bodies.
