# Wikidata, Wikipedia and Commons answers

Recorded on 2026-10-20 with the app's User-Agent
(`DroppedNeedle/3.0.0 ( contact@droppedneedle.com ; +https://github.com/DroppedNeedle/DroppedNeedle )`).
None of the three APIs needs a key. The requests were sent one at a time, each
after the previous answer, well under the `wikimedia-api` row in the rate table
(4 a second, one at a time). The subject is the band Radiohead, Wikidata item
Q44190, and a merged item, Q13165295, that redirects to Q1. The artist picture
lookup was recorded against the band Nirvana, item Q11649.

Every endpoint the client calls has a fixture:

- `entity_found.json`: `GET https://www.wikidata.org/wiki/Special:EntityData/Q44190.json`,
  HTTP 200, `application/json; charset=UTF-8`. Trimmed, see below.
- `entity_redirect.json`: `GET https://www.wikidata.org/wiki/Special:EntityData/Q13165295.json`,
  HTTP 200, no redirect status. Q13165295 is a redirect to Q1. The answer's one
  entity is that target, under its own id, `Q1`. Trimmed, see below. Q13165295
  was found with `action=query&generator=allredirects&garnamespace=0&garlimit=1&garfrom=Q&prop=info`.
- `claims_p18_found.json`: `GET https://www.wikidata.org/w/api.php?action=wbgetclaims&entity=Q44190&property=P18&format=json`,
  HTTP 200. The image claim, a `commonsMedia` file name.
- `claims_no_statement.json`: the same request for `property=P2048` (height),
  which the band has no statement for. HTTP 200, `{"claims":{}}`.
- `claims_no_such_entity.json`: the same request for `entity=Q999999999`, an id
  of the right form that names no entity. HTTP 200 with an `error` object,
  code `no-such-entity`.
- `claims_invalid_id.json`: the same request for `entity=Q9999999999999`, which
  the id builder refuses. HTTP 200 with an `error` object, code `param-invalid`.
- `claims_unresolved_redirect.json`: the same request for `entity=Q13165295`, the
  redirect above. HTTP 200 with an `error` object, code `unresolved-redirect`.
  This endpoint does not resolve a merged item to its target.
- `claims_all.json`: `GET https://www.wikidata.org/w/api.php?action=wbgetclaims&entity=Q11649&format=json`,
  HTTP 200, every claim of the item (114 properties). P18 is
  `Nirvana around 1992.jpg` and P2722, the Deezer artist id, is `415`.
  `wbgetclaims` takes one property or none: asked for `property=P18|P2722` it
  answered a 200 with an `error` object (`param-invalid`), so the picture lookup
  reads every claim in one call.
- `search_p434.json`: `GET https://www.wikidata.org/w/api.php?action=query&list=search&srsearch=haswbstatement:P434=5b11f4ce-a62d-471e-81fc-a69a8278c7da&srlimit=2&srprop=&format=json`,
  HTTP 200, the search for the item carrying Nirvana's MusicBrainz artist id.
  One hit, `Q11649`.
- `search_p434_no_hits.json`: the same search for the id
  `00000000-0000-4000-8000-000000000000`, which no item carries. HTTP 200, no
  hits.
- `extract_found.json`: `GET https://en.wikipedia.org/w/api.php?action=query&titles=Radiohead&prop=extracts&exintro=1&explaintext=1&format=json`,
  HTTP 200. Page 38252.
- `extract_not_found.json`: the same request for the title `Zzqxjvwkp nonexistent article qqz`.
  HTTP 200, page `-1` with `missing`.
- `extract_invalid.json`: the same request for the title `Radiohead[1]`, which
  MediaWiki refuses because `[` is not allowed in a title. HTTP 200, page `-1`
  with `invalid` and `invalidreason`.
- `commons_imageinfo_found.json`: `GET https://commons.wikimedia.org/w/api.php?action=query&titles=File:RadioheadO2211125 composite.jpg&prop=imageinfo&iiprop=url&iiurlwidth=1280&format=json`,
  HTTP 200. The 1280 px thumbnail is in `thumburl`. The answer carries a
  `continue` block, which the client does not follow.
- `commons_imageinfo_not_found.json`: the same request for a file name that
  does not exist. HTTP 200, page `-1` with `missing` and no image info.
- `commons_imageinfo_credit.json`, recorded on 2026-10-21: the request the
  client now sends for the same file, `GET https://commons.wikimedia.org/w/api.php?action=query&titles=File:RadioheadO2211125 composite.jpg&prop=imageinfo&iiprop=url|extmetadata&iiurlwidth=1280&iiextmetadatafilter=Artist|LicenseShortName|LicenseUrl&format=json`,
  HTTP 200, `application/json; charset=utf-8`. Beside the URLs it carries
  `extmetadata` with the three fields asked for, each an object with a string
  `value` and a `source`: `Artist` is HTML (a link to the photographer's
  Flickr page), `LicenseShortName` is `CC BY 4.0` and `LicenseUrl` the
  licence's address.

Not recorded: the Special:EntityData answer for an unknown id. The client
reads its 404 as absence without decoding the body.

The two entity answers are trimmed to the fields read: each entity's `id`, and
each sitelink's `title`. The values kept are unchanged, and every sitelink key
is kept. As sent, the Q44190 answer was 134,661 bytes and the Q13165295 answer
215,162 bytes.

`claims_all.json` is not trimmed: it is byte for byte as answered (about 110 KB
on one line), so that every kind of statement an item carries is shown to
decode. Its only free text is a short image caption.

The other bodies are unchanged except re-indented (two spaces), with one
exception. In `extract_found.json` the English Wikipedia intro in
`query.pages.38252.extract` is CC BY-SA article text, so it is replaced by a
placeholder sentence. Nothing else in that file was changed. The two older
Commons answers carry no author or licence metadata: they were recorded before
the client asked for `extmetadata`. The newer one holds a photographer's
public name and a licence, and no description text.

None of the bodies holds a key, a token or personal data. The error answers
name the Wikimedia server that served them (`servedby`), which is not personal data.
