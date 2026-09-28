# Capture 03 — Compat traffic goldens (Subsonic + Jellyfin)

## What it pins

The exact v2 wire behavior external player apps depend on: 48 request/response
pairs (`pairs/NNN_<name>.json`) captured against a temp-DB library holding one
real FLAC (`backend/tests/fixtures/library/flac_full_01.flac`, see `meta.json`
for its sha256), served through **both** compat shims **with**
`GZipMiddleware`, mirroring production.

- **Subsonic (30):** ping (json + xml), getLicense, getOpenSubsonicExtensions,
  getMusicFolders, getArtists, getIndexes, getGenres, getAlbumList2,
  getRandomSongs, search3, getStarred2, getNowPlaying, getScanStatus,
  getPlaylists, getBookmarks, getArtist, getAlbum, getMusicDirectory,
  getCoverArt (placeholder bytes), getSong, stream (full / download /
  `bytes=0-99` 206 / suffix-range 206 / gzip-offered-not-compressed),
  stream-bad-id (binary 404), ping-bad-auth (code 44), star/unstar round-trip.
- **Jellyfin (18):** System/Info/Public, System/Info, Users/Me, Views, Items
  (albums, children), AlbumArtists, Genres, item detail, image-missing 404,
  unknown-item 404, PlaybackInfo GET + POST (Finamp-required fields),
  Audio/universal, Audio/stream (full / `bytes=0-99` 206 / HEAD / no-auth),
  covering `static=true` + embedded `api_key` streaming URLs.
- **Streaming headers pinned:** `Content-Type: audio/flac`, `Accept-Ranges`,
  `Content-Range`, gzip refusal for audio, byte-identity of reassembled ranges
  (audio bodies pinned by sha256 + head/tail hex, not embedded).

`manifest.json` is the **self-golden MUST-MATCH set**: per-pair expected status,
normalized-body sha256, and pinned headers. Normalization
(`<APP_PASSWORD>`, `<SUB_*>` / `<JF_*>` ids, `<SESSION>`) is documented in
`meta.json`. `content-length` is deliberately *not* pinned (gzip-compressed
lengths vary run to run); decoded bodies are pinned by sha instead.

## Method

Live-run, read-only: `/tmp/vcap/cap03_compat.py` rebuilds the v2
`streaming_env` fixture on a temp DB + temp music dir, issues each request,
then **re-issues every MUST-MATCH request and compares status + body + pinned
headers**. Current verdict in `manifest.json → self_verify`: 48 reissued,
0 mismatches. No backend file modified; no prod data touched.

Stubs (same minimalism as v2's own `streaming_env`): cover art → none
(placeholder/404 paths), playlists → empty, now-playing → empty,
random-songs → empty.

## Consumed by (v3)

- **Stage 3 (compat shims):** v3 must replay every MUST-MATCH request and match
  status + body sha + pinned headers — the client-compat acceptance gate.
- **Stage 4 (streaming):** range/gzip/header cases are the byte-level
  streaming contract.
