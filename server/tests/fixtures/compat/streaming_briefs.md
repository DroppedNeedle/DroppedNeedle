# Streaming-semantics briefs (stage 9 journeys slice, shared with stage 6)

Both compat protocols serve bytes through the same engine seam:
`droppedneedle::stream::routes` (`StreamEngine::open`, `parse_range`,
`content_type_for_extension`, `OpenMedia` / `StreamFault`). The journeys
slice proves the wire contract from the client's point of view; stage 6
owns the engine itself. If either side changes these rules, both suites
should fail loudly.

## Brief 1: identity (whole-object) play

- `GET <stream-url>` with no `Range` answers `200` with the full object,
  `Content-Length` set, `Accept-Ranges: bytes`, and the engine-resolved
  `Content-Type` (upstream wins; local files via the extension table).
- Subsonic: `GET /subsonic/rest/stream?id=so-1`. Jellyfin:
  `GET /jellyfin/Audio/so-1/stream` and `/universal`.

## Brief 2: seeking (single range)

- `GET` with `Range: bytes=a-b` answers `206` with exactly bytes `a..=b`,
  `Content-Range: bytes a-b/N`, and `Content-Length: b-a+1`.
- Suffix ranges (`bytes=-k`) serve the last `k` bytes; open ranges
  (`bytes=a-`) run to the end. Each layer owns its own parser
  implementing the v2 `stream_track` rules byte-for-byte (the engine's
  `parse_range`, the Subsonic slice's, the Jellyfin seam's — all three
  trim surrounding whitespace); the `compat_adapters` parity briefs pin
  both production adapters to identical 200/206/416/HEAD behavior, and
  the journey streaming matrices pin the seam fakes the same way, so
  seeking can never silently drift apart.

## Brief 3: unsatisfiable ranges

- A range starting at or past the end answers `416` with
  `Content-Range: bytes */N` and an empty body. No partial bytes, no
  fallback to `200`. Both protocols map this identically.

## Brief 4: HEAD (probe before play)

- `HEAD` on any stream URL answers the same status and headers a `GET`
  would (including `206` + `Content-Range` when `Range` is present)
  with an empty body. Finamp probes before buffering; Symfonium's
  offline cache validates length this way.

## Brief 5: headerless media, authenticated metadata

- Stream URLs need no credentials: with no token the principal falls
  back to the client address (v2 `_media_principal`, `ip:` principal).
  Jellify plays `Latest` items straight from the advertised URL.
- Everything else (browse, favorites, playlists, scrobbles) requires
  the fixture token, standing in for the stage-3 app-password contracts.

## Brief 6: what compat never touches

- Journeys may mutate favorites, playlists, queues, bookmarks, and
  presence (now-playing / scrobble projections) inside the fixture
  scope only. A dedicated scope test snapshots the library catalog
  reads before and after every allowed mutation and requires them
  byte-identical (modulo favorite overlays). Transcode landings refuse
  ranges; the stub serves identity only and the transcode slice owns
  that refusal.
