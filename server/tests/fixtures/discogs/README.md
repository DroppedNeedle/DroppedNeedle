# Discogs answers

Recorded answers from `https://api.discogs.com`, fetched on 2026-10-21 with the
app's default User-Agent (`DroppedNeedle/3.0.0 ( contact@droppedneedle.com ; +https://github.com/DroppedNeedle/DroppedNeedle )`)
and `accept: application/json`. No key or token was sent. The release lookup
and the search answer without one.

Five GETs went to the API, one at a time, each at least three seconds after the
one before it. The Discogs row in `server/providers/src/rates.rs` allows one
request every 2.4 seconds. Each answer carried `x-discogs-ratelimit: 25`.

Releases, `/releases/{id}`:

- `release_249504.json`: HTTP 200 for `GET /releases/249504`. A UK vinyl 7 inch
  single from 1987.
- `release_3562468.json`: HTTP 200 for `GET /releases/3562468`. A CD with index
  and heading rows in its tracklist. Its `year` is 0, as answered.
- `release_not_found.json`: HTTP 404 for `GET /releases/999999999`. The body
  says the release does not exist or was deleted.

Search, `/database/search`:

- `search_found.json`: HTTP 200 for
  `GET /database/search?type=release&q=Nirvana+Nevermind&per_page=5`, the query
  the client sends for a release search. Five hits. A hit's `year` is text
  (`"2015"`), and the fifth hit has no `year` key.
- `search_no_results.json`: HTTP 200 for
  `GET /database/search?type=release&q=zxqvjkwpq&per_page=5`. `results` is an
  empty list and `pagination.items` is 0.

The bodies are unchanged except re-indented (two spaces) and these fields:

- `notes` (release notes, copied text): replaced by `Placeholder release notes.`
- `videos[].description` in `release_249504.json` (copied from the video sites):
  each replaced by `Placeholder video description.`
- `community.contributors[]` and `community.submitter`: the `username` and
  `resource_url` of each (Discogs user names, which are personal data) replaced
  by `REDACTED`.

The search answers hold no notes, video descriptions or user names, so nothing
in them was replaced. The `descriptions` of their `formats` are format labels
such as `Album`, kept as answered.

The answers hold no key or token. Everything else is as answered.

Not recorded: masters and artists. The client does not call them.
