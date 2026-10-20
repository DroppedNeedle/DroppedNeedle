# LRCLIB answers

Recorded answers from `GET https://lrclib.net/api/get`, fetched on
2026-10-20 with the app's User-Agent (`DroppedNeedle/3.0.0 ( contact@droppedneedle.com ; +https://github.com/DroppedNeedle/DroppedNeedle )`).
Each request was sent more than a second after the one before it. LRCLIB
needs no key.

- `get_found.json`: HTTP 200 for `track_name=Bohemian Rhapsody`,
  `artist_name=Queen`, `album_name=A Night at the Opera`, `duration=355`.
  The record carries synced and plain lyrics, with id 19080. The lyric text
  in `plainLyrics`, `syncedLyrics` and `lyricsfile` is replaced by two
  placeholder lines (the song's words are copyrighted); every other field
  and the format of each lyric field are as answered.
- `get_not_found.json`: HTTP 404 for `track_name=No Such Track For A Probe`,
  `artist_name=DroppedNeedle Probe`, `album_name=No Such Album`, `duration=1`.
- `get_busy.json`: HTTP 503 with `retry-after: 1`. The first two tries of the
  not-found query got this answer while LRCLIB reported `ServerOverloaded`;
  the third try got the 404 above.

All three bodies are unchanged except re-indented (two spaces) and the
lyric text noted above. None of
them holds a key, a token or personal data.
