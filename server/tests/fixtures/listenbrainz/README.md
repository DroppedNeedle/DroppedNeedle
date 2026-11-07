# ListenBrainz answers

Recorded answers from `https://api.listenbrainz.org`, with the app's
User-Agent. The public answers were fetched on 2026-11-02; the account answers
on 2026-11-03, for the throwaway account `DroppedNeedle`. Every request was
read-only; the two POSTs are lookups that change nothing. The recording log
names no token for the account reads. Each file holds the body as answered,
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

Account answers, 2026-11-03, for `DroppedNeedle` (listen count 28, three loved
recordings, no releases, similar users or recommended playlists yet):

- `listen_count_account.json`: HTTP 200 to `GET /1/user/DroppedNeedle/listen-count`.
- `listens_account.json`: HTTP 200 to `GET /1/user/DroppedNeedle/listens?count=3`.
  Three listens, newest first.
- `feedback_account.json`: HTTP 200 to
  `GET /1/feedback/user/DroppedNeedle/get-feedback?score=1&count=5`. Three rows,
  each with `track_metadata` null: this request asks for no metadata.
- `fresh_releases_account.json`: HTTP 200 to
  `GET /1/user/DroppedNeedle/fresh_releases?past=true&future=false`. An empty list.
- `similar_users_account.json`: HTTP 200 to `GET /1/user/DroppedNeedle/similar-users`.
  An empty list.
- `recommendation_playlists_account.json`: HTTP 200 to
  `GET /1/user/DroppedNeedle/playlists/recommendations`. An empty list.

Another user's answer, 2026-11-07, because the throwaway account's genre stats
are still not computed:

- `genre_activity_user.json`: HTTP 200 to `GET /1/stats/user/rob/genre-activity`,
  a public account's stats. Re-indented, and `user_id` replaced with
  `listenbrainz-user`; nothing else changed. One row per genre and hour.

No file: HTTP 204 with an empty body while a new account's stats are not
computed yet. Recorded for `GET /1/stats/user/DroppedNeedle/artists?count=5`,
`.../recordings?count=5`, `.../release-groups?count=5` and
`GET /1/stats/user/DroppedNeedle/genre-activity`.

Not recorded yet:

- `get-feedback` with `metadata=true`, which the loved-recordings and
  loved-artist reads send. The `track_metadata` shape is unverified.
- The 200 answers of the user `artists`, `release-groups` and `recordings`
  stats. On 2026-11-07 a public account's answers to these, with `range` and
  `offset`, were checked against the models and decoded, but not kept.
- A row of `fresh_releases`, `similar-users` or `playlists/recommendations`:
  the account's lists are empty.
- `/1/playlist/{id}`: the account has no recommended playlist to take an id from.
- The answer to a good token at `/1/validate-token`: only a made-up token is
  recorded.
