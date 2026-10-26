# Skiddle answers

Recorded from the Skiddle API v1 (`https://www.skiddle.com/api/v1`) on
2026-10-26, with the app's User-Agent. The `api_key` query value is removed
from every URL below.

- `artists_search.json`: `GET /artists/?name=Orbital&api_key=REDACTED`.
  HTTP 200. One exact match (the act with id `123487626`) among three.
- `artists_no_match.json`: `GET /artists/?name=zzqxkvwnomatch&api_key=REDACTED`.
  HTTP 200, with `results` as an empty list.
- `artists_bad_key.json`: `GET /artists/?name=Orbital&api_key=REDACTED`, sent
  with a made-up key rather than an account's key. HTTP 400, with Skiddle's
  error envelope (`error` 1, `errorcode` 997).
- `events_for_artist.json`: `GET /events/search/?a=123487626&description=1&api_key=REDACTED`.
  HTTP 200. Two gigs with lineups.
- `events_key_check.json`: `GET /events/search/?limit=1&api_key=REDACTED`, the
  call that checks a saved key. HTTP 200. One event.

Each body is unchanged except that it is re-indented. In
`events_for_artist.json` and `events_key_check.json` each event's
`description` is replaced with `Placeholder event description.`, because that
text is promoter copy.

None of the bodies holds a key, token or personal data. Venue addresses and
phone numbers are business listings and are kept as sent.
