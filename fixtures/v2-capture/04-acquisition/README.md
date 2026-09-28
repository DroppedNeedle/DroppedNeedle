# Capture 04 — Acquisition mock transcripts

## What it pins

The v2 executable record of third-party download/indexer wire shapes, recorded
as data so v3 can rebuild its acquisition clients without the v2 code:

- **slskd (9 transcripts):** `application` (0.25.1 version/server/shares),
  search start → state → responses (3 canned peers: complete FLAC album with
  absent-`bitRate` lossless quirk + empty `extension`, partial MP3 folder, junk
  folder), enqueue `[{filename,size}]` → 201 `{Enqueued, Failed}`, per-user
  transfers (directories → files), delete transfer, search-404, and the
  no-`X-API-Key` 401 shape.
- **SABnzbd (11 transcripts):** version, get_cats, get_config, queue (stringly
  `mb`/`mbleft`/`percentage`), history (numeric `bytes`), `nzo_ids`-filtered
  history, addfile/addurl `{status, nzo_ids}`, queue/history deletes incl. the
  5.0.4 quirk that completed-job storage is **retained** on
  `del_files=1` (only failed-job storage is deleted), plus the API-key-incorrect
  shape.
- **Newznab (8 transcripts + 7 raw XML files + 1 plain-text body):**
  DrunkenSlug caps (`audio-search available="no"`) + `t=search` feed (clean
  FLAC cat-3040, promo cat-3999, obfuscated-title release) + `t=music`
  error-202; Audionix caps (`audio-search available="yes"`) + `t=music` feed
  (dedup-shared FLAC identity + unique MP3); auth-error, rate-limit, and
  caps-dead shapes. Full bodies are on disk (`newznab_*.xml`, plus
  `newznab_caps_dead.txt` - a 500 `text/plain` dead-server shape, kept as
  `.txt` so XML consumers never trip on it); the transcript JSON pins status,
  content-type, item titles, attr maps, and enclosure lengths.

## Method

Live-run, read-only: `/tmp/vcap/cap04_acq.py` drives
`backend/tests/mocks/slskd_mock.py` via `httpx.ASGITransport` and the SAB /
Newznab `MockTransport` handlers directly. No backend file modified.

## Consumed by (v3)

- **Stage 3 (acquisition clients):** transcript JSON (+ raw Newznab XML) is the
  fixture set for v3's slskd/SAB/Newznab client tests — same requests must
  parse the same responses, quirks included.
