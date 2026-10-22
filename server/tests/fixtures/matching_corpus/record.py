"""Record the MusicBrainz answers a corpus run had no answer for.

    python3 -I record.py <test output> musicbrainz.jsonl
    python3 -I record.py --retrim musicbrainz.jsonl

The first form reads the `UNRECORDED <url>` lines the test prints, GETs each
URL from the BrainzMash mirror one at a time (read-only, no redirect
following: the client follows redirects itself, so a redirect is recorded
as it came), trims the answer and adds it to the file, sorted by URL. The
second form trims the answers already in the file again.

Trimming keeps only what recall and matching read: ids, titles, artist
credits (id and name), lengths, positions, media formats and track counts,
dates, country, status, barcode, labels and catalog numbers, and the
release group's types. Search pages keep each hit's id, score, title, release
group and track counts, plus formats and barcode for edition listings.
Recording search pages keep each hit's id and score, and the id, release
group and track count of each release it lists.
"""
import json
import sys
import time
import urllib.error
import urllib.request

MIRROR = "https://api.brainzmash.cc/ws/2/"
USER_AGENT = (
    "DroppedNeedle-test-corpus/1.0 ( contact@droppedneedle.com ; "
    "+https://github.com/DroppedNeedle/DroppedNeedle )"
)
SAVE_EVERY = 25
PACE_SECS = 0.35

DROP = {
    "genres", "tags", "aliases", "relations", "text-representation",
    "cover-art-archive", "release-events", "quality", "isrcs", "asin",
    "rating", "video", "created", "track-offset", "discs", "type",
    "label-code", "country-code", "artist-credit-phrase", "sort-name", "number",
}
GROUP_KEYS = ("id", "primary-type", "secondary-types", "first-release-date")


def trim(value, parent=None):
    if isinstance(value, list):
        return [trim(item, parent) for item in value]
    if not isinstance(value, dict):
        return value
    out = {}
    for key, item in value.items():
        if key in DROP or key.endswith("-id") or key.endswith("-ids"):
            continue
        if key == "artist" and isinstance(item, dict):
            out[key] = {k: item[k] for k in ("id", "name") if k in item}
        elif key == "release-group" and isinstance(item, dict):
            out[key] = {k: item[k] for k in GROUP_KEYS if k in item}
        elif key == "label" and isinstance(item, dict):
            out[key] = {k: item[k] for k in ("id", "name") if k in item}
        elif key in ("disambiguation", "joinphrase") and not item:
            continue
        elif key == "id" and parent == "media":
            continue
        else:
            out[key] = trim(item, key)
    # The track already carries the title, length and credit the
    # recording repeats.
    recording = out.get("recording")
    if isinstance(recording, dict):
        for key in ("title", "length"):
            if out.get(key) is not None and recording.get(key) == out.get(key):
                recording.pop(key, None)
        if out.get("artist-credit"):
            recording.pop("artist-credit", None)
        recording.pop("disambiguation", None)
        recording.pop("first-release-date", None)
    return out


def trim_hit(hit, edition):
    out = {k: hit[k] for k in ("id", "score", "title") if k in hit}
    if "release-group" in hit:
        out["release-group"] = {"id": hit["release-group"]["id"]}
    keys = ("format", "track-count") if edition else ("track-count",)
    out["media"] = [{k: m[k] for k in keys if k in m} for m in hit.get("media", [])]
    if edition and hit.get("barcode"):
        out["barcode"] = hit["barcode"]
    return out


def trim_recording_hit(hit):
    out = {k: hit[k] for k in ("id", "score") if k in hit}
    releases = []
    for release in hit.get("releases", []):
        kept = {k: release[k] for k in ("id", "track-count") if k in release}
        if "release-group" in release:
            kept["release-group"] = {"id": release["release-group"]["id"]}
        releases.append(kept)
    out["releases"] = releases
    return out


def trim_answer(url, body):
    if body is None:
        return None
    if "query=" in url and isinstance(body, dict) and "releases" in body:
        edition = "query=rgid" in url
        page = {k: body[k] for k in ("count", "offset") if k in body}
        page["releases"] = [trim_hit(hit, edition) for hit in body["releases"]]
        return page
    if "query=" in url and isinstance(body, dict) and "recordings" in body:
        page = {k: body[k] for k in ("count", "offset") if k in body}
        page["recordings"] = [trim_recording_hit(hit) for hit in body["recordings"]]
        return page
    return trim(body)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


OPENER = urllib.request.build_opener(NoRedirect)


def fetch(url):
    request = urllib.request.Request(
        url, headers={"User-Agent": USER_AGENT, "Accept": "application/json"}
    )
    for attempt in range(5):
        try:
            with OPENER.open(request, timeout=60) as reply:
                return reply.status, None, json.loads(reply.read())
        except urllib.error.HTTPError as error:
            if error.code in (301, 302, 307, 308):
                return error.code, error.headers.get("Location"), None
            if error.code == 404:
                try:
                    return 404, None, json.loads(error.read())
                except ValueError:
                    return 404, None, {"error": "Not Found"}
            if error.code in (429, 503):
                time.sleep(5 * (attempt + 1))
                continue
            raise
    raise RuntimeError(f"no answer for {url}")


def load(path):
    with open(path, encoding="utf-8") as file:
        return {e["url"]: e for e in map(json.loads, filter(str.strip, file))}


def save(path, entries):
    with open(path, "w", encoding="utf-8") as file:
        for url in sorted(entries):
            line = json.dumps(entries[url], ensure_ascii=False, separators=(",", ":"))
            file.write(line + "\n")


def main():
    if sys.argv[1] == "--retrim":
        entries = load(sys.argv[2])
        for url, entry in entries.items():
            if "body" in entry:
                entry["body"] = trim_answer(url, entry["body"])
        save(sys.argv[2], entries)
        return
    output, store = sys.argv[1], sys.argv[2]
    entries = load(store)
    fetched = 0
    with open(output, encoding="utf-8") as file:
        wanted = [line.split(" ", 1)[1].strip() for line in file if line.startswith("UNRECORDED ")]
    for url in wanted:
        if not url.startswith(MIRROR):
            raise SystemExit(f"refusing a URL outside the mirror: {url}")
        if url in entries:
            continue
        status, location, body = fetch(url)
        entry = {"url": url, "status": status}
        if location:
            entry["location"] = location
        if body is not None:
            entry["body"] = trim_answer(url, body)
        entries[url] = entry
        fetched += 1
        if fetched % SAVE_EVERY == 0:
            save(store, entries)
        time.sleep(PACE_SECS)
    save(store, entries)
    print(f"fetched {fetched}, {len(entries)} answers in all")


if __name__ == "__main__":
    main()
