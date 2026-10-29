# AcoustID answers

Recorded answers from `POST https://api.acoustid.org/v2/lookup`, sent with the
app's User-Agent. `batch_lookup.json` and `invalid_key.json` were fetched on
2026-10-15. The two single lookups were fetched on 2026-10-29, one at a time,
at least a second apart. Neither holds the API key (it is sent in the request,
never echoed), so none is replaced with `REDACTED`. None holds a token or
personal data.

- `batch_lookup.json`: HTTP 200 to one batch lookup with `format=json`,
  `batch=1`, `meta=recordingids releaseids`, and one print as
  `duration.0` and `fingerprint.0`. The print is the app's own
  Chromaprint of 10cc, "Sand in My Face" (FLAC, 218 seconds), as
  `generate_fingerprint` makes it. One result: AcoustID track
  `a2558b28-8f9d-44ca-b97b-f800ff935a1b`, score 0.8158072, one recording
  (`fe000616-c72c-43d4-8cba-a4083e99636b`) on 43 releases.
- `invalid_key.json`: the body of the HTTP 400 the same request gets with
  a key AcoustID does not know.
- `single_lookup.json`: HTTP 200 to the single lookup of the same print:
  `duration=218`, `meta=recordings releasegroups`. The batch fixture keeps
  its answer but not its print, so the print was made again with the app's
  `generate_fingerprint` from a FLAC of the same track. AcoustID returned the
  batch answer's track and score for it (`a2558b28-...`, 0.8158072), so it is
  the same print. The recording's `duration` is `219.0`, a float, as sent.
  Body unchanged except re-indented.
- `single_no_match.json`: HTTP 200 to the same print with `duration=5`, far
  from the track's length. `results` is an empty list. Body unchanged except
  re-indented.

Recording titles, artist names and release-group titles are catalogue
metadata, kept as sent.
