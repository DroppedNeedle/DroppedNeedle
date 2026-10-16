# AcoustID answers

Recorded answers from `POST https://api.acoustid.org/v2/lookup`, fetched
on 2026-10-15 with the app's User-Agent. Both are unchanged, and neither
holds the API key (it is sent in the request, never echoed).

- `batch_lookup.json`: HTTP 200 to one batch lookup with `format=json`,
  `batch=1`, `meta=recordingids releaseids`, and one print as
  `duration.0` and `fingerprint.0`. The print is the app's own
  Chromaprint of 10cc, "Sand in My Face" (FLAC, 218 seconds), as
  `generate_fingerprint` makes it. One result: AcoustID track
  `a2558b28-8f9d-44ca-b97b-f800ff935a1b`, score 0.8158072, one recording
  (`fe000616-c72c-43d4-8cba-a4083e99636b`) on 43 releases.
- `invalid_key.json`: the body of the HTTP 400 the same request gets with
  a key AcoustID does not know.
