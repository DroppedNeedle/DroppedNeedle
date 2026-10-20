# Open-Meteo geocoding answers

Recorded answers from `GET https://geocoding-api.open-meteo.com/v1/search`,
fetched on 2026-10-20 with the app's User-Agent (`DroppedNeedle/3.0.0 ( contact@droppedneedle.com ; +https://github.com/DroppedNeedle/DroppedNeedle )`).
Each request was sent more than a second after the one before it. Open-Meteo
needs no key.

The first two requests carry `count=8&language=en&format=json`, the query the
client sends for a city search.

- `search_liverpool.json`: HTTP 200 for `name=Liverpool`. The first match is
  Liverpool, GB.
- `search_no_results.json`: HTTP 200 for `name=No Such Place For A Probe`. The
  answer has no `results` key: the geocoder knows no such place.
- `search_bad_count.json`: HTTP 400 for `name=Liverpool&count=0&language=en&format=json`,
  a refused parameter. The answer carries `error` and `reason` and no `results`
  key.

All three bodies are unchanged except re-indented (two spaces). No field is
replaced: the answers hold place names, coordinates, codes and postcodes, no
lyrics, biographies or reviews. None of them holds a key, a token or personal
data.
