# Cover Art Archive answers

Recorded answers for the image listing that the Cover Art Archive client reads.
They were fetched on 2026-10-20, between 23:24 and 23:26 UTC, with the app's
User-Agent (`DroppedNeedle/3.0.0 ( contact@droppedneedle.com ; +https://github.com/DroppedNeedle/DroppedNeedle )`)
and `accept: application/json`. The archive needs no key, and none was sent.

Three requests went to coverartarchive.org, one at a time, 63 and 31 seconds
apart. A listing is a redirect chain: the archive answers 307 with an
`archive.org/download/mbid-<release>/index.json` URL, archive.org answers 302
with a `ca.archive.org` datanode, and the datanode serves the JSON. The client
follows these hops without pacing, and the recording followed them too, so four
more requests went to archive.org and its datanode. Every request started at
least six seconds after the one before it. None got a 429 or a 503.

- `release_found.json`: `GET /release/30702389-5c67-4438-9ea0-2351c8de0f1d`,
  no query string. The datanode answered HTTP 200 with the listing. Two images.
- `release_group_found.json`: `GET /release-group/b1392450-e666-3926-a536-22c65f834433`
  (OK Computer), no query string. The datanode answered HTTP 200 with the
  listing. Two images. Its `release` key names
  30702389-5c67-4438-9ea0-2351c8de0f1d, and the document is the same as
  `release_found.json`.
- `release_not_found.html`: `GET /release/fd6ebf0e-85e4-4aa1-995a-07d5ac0c2147`,
  a random version 4 UUID. The archive answered HTTP 404 directly, with no
  redirect. The body is an HTML page, not a listing, so the client reads this
  answer by its status alone.

The bodies are unchanged except re-indented (two spaces). The 404 page is
unchanged. The answers hold no key, token or personal data: the image comments
are empty, and the `edit` fields are numbers only. The second image of
`release_group_found.json` has `front` set to false with the type `Front`, as
the archive answered it.
