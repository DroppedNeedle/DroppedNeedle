# Deemix answers

Recorded answers from Deemix (deemix-webui 4.8.0, deemix 3.14.1, the
`ghcr.io/bambanah/deemix` image whose digest starts `sha256:aff61206`,
given in full in `acquire/src/deemix/models.rs`) on 2026-11-21. The
server was a throwaway container bound to loopback in single-user mode,
with no Deezer account saved, so nothing could download. Every answer
came on HTTP 200; Deemix sends its JSON as
`application/json; charset=utf-8` (Express's `res.send` of an object). No
answer holds a sign-in or any personal data. `testkit/src/deemix.rs` serves
these files as they are.

The tracks' `preview` links were cut from the tracklists and searches:
they carry signed tokens and nothing reads them. Nothing else was
changed.

- `connect_signed_out.json`: `GET /api/connect` on a fresh session with no
  login saved: `autologin: true`, `currentUser: {}`, `singleUser.arl`
  null and the whole `settingsData`. An empty queue leaves `queue` out.
- `search_album.json`: `GET /api/search` with `type=album` and the term
  `Radiohead In Rainbows`, asking for three hits.
- `search_album_multidisc.json`: `GET /api/search` with `type=album` and the
  term `Mellon Collie and the Infinite Sadness`, asking for five hits.
- `tracklist_album.json`: `GET /api/getTracklist?type=album&id=14880659`
  (In Rainbows, one disc).
- `tracklist_album_multidisc.json`: `GET
  /api/getTracklist?type=album&id=6158273` (Mellon Collie, two discs: 30
  rows for 28 tracks, with a `disc_separator` row before each disc).
- `tracklist_unknown.json`: `GET /api/getTracklist?type=album&id=` with an
  id Deezer does not know, copied from the probe's notes (its answer was
  not saved on its own).
- `addToQueue_not_logged_in.json`: `POST /api/addToQueue` with
  `{"url":"https://www.deezer.com/album/14880659","bitrate":9}` and no
  Deezer sign-in.
- `getQueue_empty.json`: `GET /api/getQueue` with nothing queued.

## Signed in, 2026-11-22

Recorded from the same image, bound to loopback in single-user mode and
signed in with a free Deezer account, which downloads MP3 128 only. Real
downloads were made and deleted with the container. Before recording
ended, the answers were scrubbed: the ARL is `deadbeef` 24 times (192
characters, the real length), every `license_token` is `x` of its real
length, the user id is 1000001 and the loved-tracks id 1000002, the name
"Test User", the country `XX` and the picture empty. The `cover` and `albumURLs` image
links were cut from the add and queue answers. Nothing else was changed.

- `connect_signed_in.json`: `GET /api/connect` on a session that signed
  in: `autologin: false`, `currentUser` with `can_stream_hq` and
  `can_stream_lossless` both false, no `singleUser`, no `queue`.
- `connect_saved_login.json`: `GET /api/connect` on a fresh session once a
  sign-in was saved: `autologin: true`, `currentUser: {}`, and the saved
  ARL offered as `singleUser.arl`.
- `connect_with_queue.json`: `GET /api/connect` signed in while the queue
  holds items: the queue rides along as `queue`.
- `loginArl_success.json`: `POST /api/loginArl` with the offered ARL on a
  signed-out session: `status: 1`. The answer echoes the ARL and the
  account.
- `loginArl_already_logged.json`: the same again on the signed-in session:
  `status: 2`.
- `addToQueue_cant_stream.json`: `POST /api/addToQueue` with
  `{"url":"https://www.deezer.com/album/14880659","bitrate":9}` signed in:
  `CantStream`, as the account has no FLAC.
- `addToQueue_track.json`: track 138546807 at bitrate 1: `obj[0]` with
  uuid `track_138546807_1`.
- `addToQueue_album.json`: album 14880659 at bitrate 1: uuid
  `album_14880659_1`.
- `addToQueue_nothing_added.json`: album 6158273 at bitrate 1, answered
  `result: true` with an empty `obj` and nothing queued: Deemix failed a
  DNS lookup of deezer.com while building the item.
- `addToQueue_album_multidisc.json`: the same add repeated, which queued
  it: uuid `album_6158273_1`.
- `removeFromQueue.json`: `POST /api/removeFromQueue?uuid=` for a finished
  album. An absent uuid and the item being downloaded answer the same.
- `getQueue_track_1_downloading.json`, `getQueue_track_2_downloading.json`,
  `getQueue_track_3_completed.json`: the track downloading (in `queue` as
  `downloading`, its progress in `current`), then completed: one file at
  `/downloads//Radiohead - Nude.mp3`, loose in the download folder, with
  `extrasPath` `/downloads/`.
- `getQueue_two_albums_1.json` to `_7.json`: album 14880659 current with
  album 6158273 waiting (`inQueue`: no counters, files or errors), then
  6158273 current, then both completed. A finished album lists its files
  in the order they finished, `filename` below `extrasPath`; the two-disc
  album's are in `CD1/` and `CD2/`.
- `getQueue_readd_overwrite_n.json`: album 14880659 added again with
  "Overwrite files" set to No: completed with every file listed.
- `getQueue_after_restart.json`: after a restart, the finished items kept.
- `getQueue_readd_overwrite_y.json`: album 14880659 added again with
  "Overwrite files" set to Yes: completed, every file rewritten.
- `getQueue_before_kill.json`: album 6158273 current with 14880659
  waiting; `queueOrder` holds only the waiting one.
- `getQueue_after_kill.json`, `getQueue_after_kill_30s.json`: after the
  container was killed and started: both albums `inQueue`, only 14880659
  in `queueOrder`, nothing current, nothing started.
- `getQueue_after_kill_login_1.json`, `getQueue_after_kill_login_2.json`:
  after a sign-in 14880659 runs and completes; 6158273 stays `inQueue`.
- `getQueue_stranded_readd_done.json`: 6158273 removed and added again:
  completed.
- `getQueue_after_cancel.json`: just after removing the item being
  downloaded: it has left `queue`, but `current` still names it.

The mock makes these, which a free account could not record: a failed
sign-in (`status: 0`; on a real Deemix it erases the saved sign-in),
items ending `withErrors` or `failed` and their `errors`, and FLAC files.

Not stored as files:

- A path Deemix has no route for answers HTTP 200 with its web UI's HTML.
- The first answer to a new session sets a `connect.sid` cookie.
