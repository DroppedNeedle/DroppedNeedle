# Spotify embed pages

Recorded answers from Spotify's web player, fetched on 2026-11-20 with the app's
User-Agent (`DroppedNeedle/3.0.0 ( contact@droppedneedle.com ; +https://github.com/DroppedNeedle/DroppedNeedle )`).
Song links read a track from this page when the server has no Spotify app. It
is the page Spotify's embed player loads, not a documented API, and needs no
token. The two requests were sent seconds apart.

- `embed_track.html`: HTTP 200 `text/html` for `GET https://open.spotify.com/embed/track/5ghIJDpPoe3CfHMGu71E6T`
  (Nirvana, "Smells Like Teen Spirit"). Its `__NEXT_DATA__` script holds the
  track's id, name, artists and duration in milliseconds.
- `embed_track_not_found.html`: HTTP 200 `text/html` for `GET https://open.spotify.com/embed/track/0000000000000000000000`,
  an id Spotify does not hold. Its page data has a `status` of 404 and no
  `state`.

The pages carry an anonymous access token for the embed player and tracing
values. Each is replaced by `REDACTED`: `accessToken` (its expiry set to 0),
`clientId`, `correlationId`, `buildId`, the script `nonce`s, the `sentry-trace`
and `baggage` meta tags, `_sentryTraceData`, `_sentryBaggage`, and the
`sp_cid` tracking value on the artist link. Nothing else is changed. The pages
hold a title, a name, ids and image and preview links, and no lyric or
biography text.
