"""Live Music Archive source - the reference audio_source plugin.

Everything here is lawful by construction: the etree collection hosts live
recordings by artists who explicitly permit free trading, served by the
Internet Archive's public APIs (live-verified 2026-07-10):

- search:   GET https://archive.org/advancedsearch.php?q=collection:etree+...&output=json
- files:    GET https://archive.org/metadata/{identifier}
- download: GET https://archive.org/download/{identifier}/{filename}

This file doubles as the worked example for the audio_source capability:
``search`` returns SourceItems, ``fetch`` fills the host-provided directory and
returns the written paths - the host's importer does everything after that.
"""

from pathlib import Path

from infrastructure.plugins.protocols import SourceItem

SEARCH_URL = "https://archive.org/advancedsearch.php"
METADATA_URL = "https://archive.org/metadata/{identifier}"
DOWNLOAD_URL = "https://archive.org/download/{identifier}/{filename}"

_FORMAT_PREFERENCE = {
    "flac": ["flac", "24bit flac"],
    "mp3": ["vbr mp3", "mp3"],
}


class LiveMusicArchiveSource:
    def __init__(self, context):
        self.ctx = context

    async def search(self, query: str, limit: int) -> list[SourceItem]:
        params = {
            "q": f"collection:etree AND mediatype:etree AND ({query})",
            "fl[]": ["identifier", "title", "creator", "date"],
            "rows": str(max(1, min(limit, 50))),
            "output": "json",
        }
        response = await self.ctx.http.get(SEARCH_URL, params=params)
        response.raise_for_status()
        docs = response.json().get("response", {}).get("docs", [])
        items: list[SourceItem] = []
        for doc in docs:
            identifier = doc.get("identifier")
            if not identifier:
                continue
            creator = doc.get("creator")
            if isinstance(creator, list):
                creator = ", ".join(str(c) for c in creator)
            date = str(doc.get("date") or "")[:10]
            items.append(
                SourceItem(
                    id=identifier,
                    title=str(doc.get("title") or identifier),
                    artist=str(creator or ""),
                    detail=date,
                )
            )
        return items

    async def fetch(self, item_id: str, dest_dir: Path) -> list[Path]:
        response = await self.ctx.http.get(METADATA_URL.format(identifier=item_id))
        response.raise_for_status()
        files = response.json().get("files", [])

        preferred = (self.ctx.settings.get("preferred_format") or "flac").strip().lower()
        wanted_formats = _FORMAT_PREFERENCE.get(preferred, _FORMAT_PREFERENCE["flac"])
        chosen = self._pick_format(files, wanted_formats)
        if not chosen:
            self.ctx.logger.warning("no audio files for %s", item_id)
            return []

        written: list[Path] = []
        for entry in chosen:
            name = entry["name"]
            url = DOWNLOAD_URL.format(identifier=item_id, filename=name)
            target = dest_dir / Path(name).name
            async with self.ctx.http.stream("GET", url, follow_redirects=True) as stream:
                stream.raise_for_status()
                with open(target, "wb") as out:
                    async for chunk in stream.aiter_bytes():
                        out.write(chunk)
            written.append(target)
        return written

    @staticmethod
    def _pick_format(files: list[dict], wanted_formats: list[str]) -> list[dict]:
        """All files of the first wanted format that exists, else the first
        format any audio file has (a show is all-or-nothing, like a folder)."""
        by_format: dict[str, list[dict]] = {}
        for entry in files:
            fmt = str(entry.get("format") or "").lower()
            name = entry.get("name")
            if name and fmt in ("flac", "24bit flac", "vbr mp3", "mp3", "ogg vorbis"):
                by_format.setdefault(fmt, []).append(entry)
        for fmt in wanted_formats:
            if fmt in by_format:
                return by_format[fmt]
        for entries in by_format.values():
            return entries
        return []
