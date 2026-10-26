# Ticketmaster answers

Recorded answers from the Discovery API at `https://app.ticketmaster.com/discovery/v2/`,
fetched on 2026-10-26 with the app's User-Agent. Every request was a read-only
GET, one at a time, at least three seconds apart. Six requests in all: five
with the real key and one with a made-up key, for the error answer. No answer
echoed the key, so nothing was replaced with `REDACTED`. None holds a token or
personal data.

- `attractions_search_radiohead.json`: HTTP 200 to
  `attractions.json?keyword=Radiohead&classificationName=Music&size=50`, the
  client's own search. Twelve acts. The one named "Radiohead" carries a
  `musicbrainz` link. Body unchanged except re-indented.
- `attractions_no_results.json`: HTTP 200 to
  `attractions.json?keyword=zzqxvnosuchact&classificationName=Music&size=50`.
  Zero results: no `_embedded`. Body unchanged except re-indented.
- `attractions_invalid_key.json`: HTTP 401 to `attractions.json?keyword=test&size=1`
  with a made-up key that Ticketmaster does not know. Body unchanged except
  re-indented.
- `events_radiohead_none.json`: HTTP 200 to
  `events.json?attractionId=K8vZ91713wV&sort=date,asc&size=200&page=0`. Radiohead
  has no upcoming events: `page.totalPages` is 0 and there is no `_embedded`.
  Body unchanged except re-indented.
- `events_fontaines_dc.json`: HTTP 200 to
  `events.json?attractionId=K8vZ9179LP7&sort=date,asc&size=200&page=0`, the
  client's own events call. Fifty-six events on one page (`totalPages` 1). The
  id came from a search for "Fontaines D.C." that is not kept. The search was
  made to find an act with upcoming events. Body unchanged except re-indented,
  except that the text in these fields, which is promoter and venue copy, is
  replaced with `Placeholder text.`: `info`, `pleaseNote`, `description`,
  `adaCustomCopy`, `openHoursDetail`, `willCallDetail`, `parkingDetail`,
  `accessibleSeatingDetail`, `generalRule`, `childRule` and `phoneNumberDetail`.
  The two e-mail addresses in `ada.adaPhones` are replaced with
  `name@example.com`. The act and venue handles and channel links that remain
  are public business accounts.

Not recorded: the 429 answer for a spent daily allowance (`QuotaViolation`).
Reaching it needs the day's allowance spent, which a recording should not do.
