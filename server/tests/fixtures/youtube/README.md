# YouTube answers

Recorded answers from the YouTube Data API at `https://www.googleapis.com/youtube/v3/`,
fetched on 2026-10-29 with the app's User-Agent. Every request was a read-only
GET, one at a time, each sent after the previous answer came back. Five requests
in all: four with the real key and one with a made-up key, for the error answer.
One of the four was a search, which costs 100 units of the project's daily quota.
No answer echoed the key, so nothing was replaced with `REDACTED`. None holds a
credential or personal data.

The search asks for `part=id` only, so the answer carries ids and no title,
description or thumbnail. The `etag` fields are cache tags and `nextPageToken`
is a paging cursor, neither a credential, so they are kept as answered.

- `search_found.json`: HTTP 200 to
  `search?part=id&type=video&maxResults=1&q=Portishead Dummy full album`, the
  client's own preview search for an album. One result, video `hd9OYCqBK88`.
  Body unchanged.
- `search_invalid_key.json`: HTTP 400 to the same search with a made-up key.
  `error.errors[].reason` is `badRequest` and `details[].reason` is
  `API_KEY_INVALID`. Body unchanged.
- `videos_found.json`: HTTP 200 to `videos?part=id&id=dQw4w9WgXcQ`, the client's
  key check. One item, whose `id` is the video it asked for. Body unchanged.
- `videos_unknown_id.json`: HTTP 200 to `videos?part=id&id=zzzzzzzzzzz`, an id
  that names no video. `items` is empty and `pageInfo` counts zero. Body
  unchanged.
- `videos_forbidden_part.json`: HTTP 403 to `videos?part=fileDetails&id=dQw4w9WgXcQ`.
  The file-details part is for a video's owner only. The error's reason is
  `forbidden` in domain `youtube.video`. Body unchanged.

Not recorded:

- The 403 a spent daily quota gets, with reason `quotaExceeded`. Reaching it
  needs the project's quota spent, which a recording should not do. The reason
  is the one Google's error reference names for a spent quota
  (https://developers.google.com/youtube/v3/docs/errors, read 2026-10-29).
- A search with no result. A second search costs 100 units.
- The 429 answer. It needs the per-second limit hit.
