# Last.fm answers

Recorded answers from `GET https://ws.audioscrobbler.com/2.0/`, fetched on
2026-10-26 with the app's User-Agent, one request every 1.5 seconds. Every
request carried `format=json` and an API key; the key is not in any URL
below or in any answer (Last.fm never echoes it). Each body is unchanged
except re-indented, apart from the biography and wiki text noted below.
None holds personal data.

| File | Query (besides `api_key` and `format=json`) | Status |
| --- | --- | --- |
| `artist_getinfo.json` | `method=artist.getInfo&artist=Radiohead` | 200 |
| `album_getinfo.json` | `method=album.getInfo&artist=Radiohead&album=OK Computer` | 200 |
| `artist_getsimilar.json` | `method=artist.getSimilar&artist=Radiohead&limit=3` | 200 |
| `artist_gettoptracks.json` | `method=artist.getTopTracks&artist=Radiohead&limit=3` | 200 |
| `artist_gettopalbums.json` | `method=artist.getTopAlbums&artist=Radiohead&limit=3` | 200 |
| `artist_gettoptags.json` | `method=artist.getTopTags&artist=Radiohead&autocorrect=0` | 200 |
| `album_gettoptags.json` | `method=album.getTopTags&artist=Radiohead&album=OK Computer&autocorrect=0` | 200 |
| `chart_gettopartists.json` | `method=chart.getTopArtists&limit=3` | 200 |
| `tag_gettopartists.json` | `method=tag.getTopArtists&tag=trip-hop&limit=1` | 200 |
| `artist_not_found.json` | `method=artist.getInfo&artist=Zq7xv Nonexistent Artist 0b9k` | 200, error 6 |
| `user_not_found.json` | `method=user.getTopAlbums&user=dnq7zx0nosuch&period=overall&limit=3` | 404, error 6 |
| `invalid_key.json` | `method=artist.getInfo&artist=Radiohead`, sent with the key `0000` | 403, error 10 |

What the recordings show:

- Counts arrive as text on most answers and as numbers on
  `artist.getTopAlbums` (`playcount`), the top-tags answers (`count`) and
  `album.getInfo`'s tracks (`duration`, `@attr.rank`).
  `tag.getTopArtists` rows carry no `playcount` at all.
- A list of one item came back as a one-item array (`tag.getTopArtists`
  with `limit=1`); the bio's `links.link` came back as a bare object.
- The similar artists inside `artist.getInfo` carry no `mbid` or `match`.
- An unknown entity is error 6 in a 200, an unknown user error 6 in a 404,
  and a refused key error 10 in a 403.

Replaced text: `artist.bio.summary` and `artist.bio.content` in
`artist_getinfo.json`, and `album.wiki.summary` and `album.wiki.content` in
`album_getinfo.json`, are placeholder text of the same form (the closing
"Read more on Last.fm" link and licence line are kept as sent).

Not recorded: the user reads (`user.getTopAlbums`, `user.getTopArtists`,
`user.getWeeklyArtistChart`, `user.getWeeklyAlbumChart`,
`user.getLovedTracks`, `user.getRecentTracks`), as no account was named
for recording; `auth.getToken` and `auth.getSession`, a sign-in flow; and
`track.scrobble` and `track.updateNowPlaying`, which are writes.
