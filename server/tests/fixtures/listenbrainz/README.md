# ListenBrainz answers

Recorded answers from `https://api.listenbrainz.org`, fetched on 2026-11-02
with the app's User-Agent. Every request was keyless and read-only; the two
POSTs are lookups that change nothing. Each file holds the body as answered,
unchanged except re-indented, apart from the two popularity lists below. None
holds a key, token or personal data. Names, tags and links are catalogue data.

- `recording_metadata_post.json`: HTTP 200 to `POST /1/metadata/recording/`
  for recording `9e2ad5bc-c6f9-40d2-a36f-3122ee2072a3` (Karma Police) with
  `inc=release`. The same read as a GET answered the same bytes.
- `release_group_metadata.json`: HTTP 200 to `GET /1/metadata/release_group/`
  with `release_group_mbids=b1392450-e666-3926-a536-22c65f834433` (OK Computer)
  and `inc=artist tag release`.
- `release_group_metadata_unknown.json`: HTTP 200 to the same read with
  `release_group_mbids=00000000-0000-4000-8000-000000000000`. The answer is `{}`.
- `popularity_release_group_post.json`: HTTP 200 to
  `POST /1/popularity/release-group` for OK Computer.
- `top_release_groups_radiohead.json`: HTTP 200 to
  `GET /1/popularity/top-release-groups-for-artist/a74b1b7f-71a5-4011-9441-d0b5e4122711`.
  All 347 rows, the bytes as received. The endpoint takes no limit.
- `top_recordings_radiohead.json`: HTTP 200 to
  `GET /1/popularity/top-recordings-for-artist/a74b1b7f-71a5-4011-9441-d0b5e4122711`.
  All 2170 rows, the bytes as received; three have a null `release_mbid` and
  `release_name`. The endpoint takes no limit.
- `lb_radio_artist.json`: HTTP 200 to
  `GET /1/lb-radio/artist/a74b1b7f-71a5-4011-9441-d0b5e4122711` with
  `mode=easy`, `max_similar_artists=3`, `max_recordings_per_artist=5`,
  `pop_begin=0` and `pop_end=100`. The three entries include the seed artist.
- `validate_token_invalid.json`: HTTP 200 to `GET /1/validate-token` with a
  made-up token. A refused token is `valid: false` on a 200, not a 401.
- `listen_count_unknown_user.json`: the body of the HTTP 404 to
  `GET /1/user/droppedneedle-no-such-user-fixture/listen-count`.
- `listens_unknown_user.json`: the body of the HTTP 404 to
  `GET /1/user/droppedneedle-no-such-user-fixture/listens?count=1`.
- `sitewide_artists.json`: HTTP 200 to `GET /1/stats/sitewide/artists` with
  `count=5`. Each row names one `artist_mbid`, not a list.
- `sitewide_release_groups.json`: HTTP 200 to
  `GET /1/stats/sitewide/release-groups` with `count=5`.

Not recorded yet: the 200 answers to `/1/user/{user}/listens` and
`/1/user/{user}/listen-count`, and the answer to a good token at
`/1/validate-token`, and the answers to `/1/stats/user/{user}/artists`,
`/1/stats/user/{user}/recordings`, `/1/stats/user/{user}/release-groups`,
`/1/stats/user/{user}/genre-activity`,
`/1/feedback/user/{user}/get-feedback`, `/1/user/{user}/fresh_releases`,
`/1/user/{user}/similar-users`, `/1/user/{user}/playlists/recommendations` and
`/1/playlist/{id}`. They need an account.
