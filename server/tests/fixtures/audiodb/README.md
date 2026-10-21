# TheAudioDB answers

Recorded answers from `https://www.theaudiodb.com/api/v1/json/123/`, fetched on
2026-10-21 with the app's User-Agent
(`DroppedNeedle/3.0.0 ( contact@droppedneedle.com ; +https://github.com/DroppedNeedle/DroppedNeedle )`).

The key in the path is `123`, TheAudioDB's public free key. It is the key the
app uses when none is saved. No other key was used.

Five GETs were sent, one at a time, each at least three seconds after the one
before. That is slower than the free row's 0.48 a second in
`server/providers/src/rates.rs`.

- `artist_mb_found.json`: HTTP 200 for `GET /123/artist-mb.php?i=a74b1b7f-71a5-4011-9441-d0b5e4122711`,
  the artist lookup by MusicBrainz id (Radiohead).
- `album_mb_found.json`: HTTP 200 for `GET /123/album-mb.php?i=b1392450-e666-3926-a536-22c65f834433`,
  the album lookup by MusicBrainz release group id (OK Computer).
- `search_artist_found.json`: HTTP 200 for `GET /123/search.php?s=Radiohead`,
  the artist search by name. On that day it held the same body as the lookup.
- `search_album_found.json`: HTTP 200 for `GET /123/searchalbum.php?s=Radiohead&a=OK+Computer`,
  the album search by artist and title. On that day it held the same body as the
  album lookup.
- `artist_mb_not_found.json`: HTTP 200 for `GET /123/artist-mb.php?i=00000000-0000-0000-0000-000000000000`,
  a lookup with no match: `{"artists": null}`.

Every answer is `application/json; charset=utf-8`.

The bodies are unchanged except re-indented (two spaces) and the copied text
fields below. Each of those held text, and now holds a short placeholder
sentence in its place:

- Artist answers: `strBiography`, and the biographies in other languages
  (`strBiographyDE`, `strBiographyES`, `strBiographyFR`, `strBiographyPT`,
  `strBiographySE`).
- Album answers: `strDescription`, its other-language copies
  (`strDescriptionES`, `strDescriptionFR`, `strDescriptionPT`,
  `strDescriptionSE`), and `strReview`.

Fields that were empty or null are left as answered. Artwork URLs, ids, genres
and every other field are as answered.

None of the answers holds a key, a token or personal data. They do not echo the
key, so nothing was replaced with `REDACTED`.

Not recorded: a bad-key answer, because it would need a second key and only the
free key is used. Also an album lookup with no match. The album list reads as
no match when it is null or empty.
