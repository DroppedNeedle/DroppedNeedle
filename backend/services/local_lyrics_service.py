"""Lyrics for native-library tracks: sidecar .lrc/.txt files and embedded tags.

Extraction priority:
1. Sidecar next to the audio file (same basename): ``.lrc`` always wins when
   present; a ``.txt`` sidecar only counts when it actually contains LRC
   timestamps (plain-text .txt files are commonly unrelated notes).
2. Embedded tags via mutagen: ID3 USLT (and SYLT converted to synced lines),
   FLAC/Vorbis ``LYRICS``/``UNSYNCEDLYRICS``, MP4 ``\xa9lyr``.

Any lyric text that itself contains LRC ``[mm:ss.xx]`` timestamps is parsed
into synced lines; otherwise the text is returned as plain split-lines.

Parsed results are cached in-process keyed by (path, audio mtime, sidecar
mtimes) so seek-driven refetches from the player don't re-read tags from disk.

When local extraction finds nothing AND the admin has enabled the optional
LRCLIB fetch (Settings -> Library -> Lyrics, off by default), the service asks
LRCLIB once and writes a ``.lrc`` sidecar next to the audio file so the result
is permanent and the normal sidecar path serves it from then on. If the sidecar
can't be written (read-only mount) the fetched lyrics are served from an
in-memory cache instead. Misses are negative-cached per file for ~24h so
repeated player requests don't re-hit LRCLIB.
"""

import asyncio
import logging
import re
import time
from collections import OrderedDict
from pathlib import Path
from typing import TYPE_CHECKING

import mutagen
from mutagen.id3 import ID3
from mutagen.mp4 import MP4Tags

from api.v1.schemas.library import LibraryLyricLine, LibraryLyricsResponse
from services.local_files_service import LocalFilesService

if TYPE_CHECKING:
    from repositories.lrclib_repository import LrcLibRepository
    from repositories.protocols import LibraryRepositoryProtocol
    from services.preferences_service import PreferencesService

logger = logging.getLogger(__name__)

# [mm:ss], [mm:ss.xx], [mm:ss.xxx], [mm:ss:xx] - minutes may exceed two digits
_TS_RE = re.compile(r"\[(\d{1,4}):(\d{1,2})(?:[.:](\d{1,3}))?\]")
# metadata/ID tag lines like [ar:...], [ti:...], [offset:...], [#comment]
_META_RE = re.compile(r"^\[[a-zA-Z#][^\]]*\]\s*$")
_OFFSET_RE = re.compile(r"\[offset:\s*([+-]?\d+)\s*\]", re.IGNORECASE)

_SIDECAR_SUFFIXES = (".lrc", ".txt")


def _timestamp_seconds(match: re.Match) -> float:
    minutes, seconds, frac = match.group(1), match.group(2), match.group(3)
    value = int(minutes) * 60 + int(seconds)
    if frac:
        value += int(frac) / (10 ** len(frac))
    return float(value)


def _split_leading_timestamps(line: str) -> tuple[list[float], str]:
    """Consume the run of leading ``[mm:ss.xx]`` stamps; return (stamps, rest)."""
    stamps: list[float] = []
    pos = 0
    while True:
        match = _TS_RE.match(line, pos)
        if not match:
            break
        stamps.append(_timestamp_seconds(match))
        pos = match.end()
    return stamps, line[pos:].strip()


def parse_lrc(text: str) -> tuple[bool, list[tuple[str, float | None]]]:
    """Parse LRC-ish text into ``(is_synced, [(line_text, start_seconds)])``.

    Multiple timestamps on one line each yield an entry. Metadata tags
    ([ar:], [ti:], [offset:], ...) are stripped; a ``[offset:ms]`` tag shifts
    all timestamps (positive offset = lyrics display earlier, LRC convention).
    If no timestamps exist at all, the text is returned as plain lines with
    ``None`` starts.
    """
    offset_seconds = 0.0
    offset_match = _OFFSET_RE.search(text)
    if offset_match:
        try:
            offset_seconds = int(offset_match.group(1)) / 1000.0
        except ValueError:
            offset_seconds = 0.0

    synced: list[tuple[str, float]] = []
    plain: list[str] = []
    for raw in text.splitlines():
        line = raw.strip()
        if not line:
            plain.append("")
            continue
        stamps, remainder = _split_leading_timestamps(line)
        if stamps:
            if not remainder:
                continue  # bare timestamp marker (e.g. instrumental gap)
            for stamp in stamps:
                synced.append((remainder, max(0.0, stamp - offset_seconds)))
        elif not _META_RE.match(line):
            plain.append(line)

    if synced:
        synced.sort(key=lambda item: item[1])
        return True, list(synced)

    # trim leading/trailing blank lines kept for verse separation
    while plain and not plain[0]:
        plain.pop(0)
    while plain and not plain[-1]:
        plain.pop()
    return False, [(line, None) for line in plain]


def _build_response(text: str) -> LibraryLyricsResponse | None:
    is_synced, parsed = parse_lrc(text)
    if not parsed:
        return None
    lines = [
        LibraryLyricLine(text=line_text, start_seconds=start)
        for line_text, start in parsed
    ]
    return LibraryLyricsResponse(
        text="\n".join(line.text for line in lines),
        is_synced=is_synced,
        lines=lines,
    )


def _synced_response(pairs: list[tuple[str, float]]) -> LibraryLyricsResponse:
    ordered = sorted(pairs, key=lambda item: item[1])
    lines = [
        LibraryLyricLine(text=line_text, start_seconds=start)
        for line_text, start in ordered
    ]
    return LibraryLyricsResponse(
        text="\n".join(line.text for line in lines),
        is_synced=True,
        lines=lines,
    )


def _read_sidecar(audio_path: Path) -> str | None:
    """Text of a same-basename sidecar: .lrc always, .txt only with LRC content."""
    for suffix in _SIDECAR_SUFFIXES:
        candidate = audio_path.with_suffix(suffix)
        try:
            if not candidate.is_file():
                continue
            content = candidate.read_text(encoding="utf-8-sig", errors="replace")
        except OSError:
            continue
        if not content.strip():
            continue
        if suffix == ".txt" and not _TS_RE.search(content):
            continue  # plain .txt sidecars are not necessarily lyrics
        return content
    return None


def _first_text(values) -> str | None:
    if values is None:
        return None
    if not isinstance(values, (list, tuple)):
        values = [values]
    for value in values:
        text = str(value)
        if text.strip():
            return text
    return None


def _extract_embedded(audio_path: Path) -> LibraryLyricsResponse | None:
    audio = mutagen.File(audio_path)
    if audio is None or not audio.tags:
        return None
    tags = audio.tags

    if isinstance(tags, ID3):
        uslt_text: str | None = None
        for frame in tags.getall("USLT"):
            uslt_text = _first_text(frame.text)
            if uslt_text:
                break
        uslt_response = _build_response(uslt_text) if uslt_text else None
        if uslt_response is not None and uslt_response.is_synced:
            return uslt_response
        # SYLT format 2 = absolute milliseconds; preferred over plain USLT text
        for frame in tags.getall("SYLT"):
            if getattr(frame, "format", None) != 2 or not frame.text:
                continue
            pairs = [
                (line_text.strip(), ms / 1000.0)
                for line_text, ms in frame.text
                if line_text and line_text.strip()
            ]
            if pairs:
                return _synced_response(pairs)
        return uslt_response

    if isinstance(tags, MP4Tags):
        text = _first_text(tags.get("\xa9lyr"))
        return _build_response(text) if text else None

    # Vorbis comments (FLAC/Ogg/Opus); lookups are case-insensitive but be safe
    for key in ("LYRICS", "UNSYNCEDLYRICS", "lyrics", "unsyncedlyrics"):
        try:
            text = _first_text(tags.get(key))
        except Exception:  # noqa: BLE001 - non-dict tag containers
            text = None
        if text:
            return _build_response(text)
    return None


class LocalLyricsService:
    """Sidecar + embedded lyrics for local library files, with an LRU cache.

    The optional collaborators (library repo, preferences, LRCLIB client) power
    the admin-gated online fetch; without them the service is local-only.
    """

    _CACHE_MAX = 128
    _NEGATIVE_TTL_SECONDS = 24 * 60 * 60  # don't re-ask LRCLIB about a miss for ~24h
    _NEGATIVE_MAX = 4096

    def __init__(
        self,
        local_files_service: LocalFilesService,
        library_repo: "LibraryRepositoryProtocol | None" = None,
        preferences_service: "PreferencesService | None" = None,
        lrclib_repository: "LrcLibRepository | None" = None,
    ):
        self._local_files = local_files_service
        self._library_repo = library_repo
        self._preferences = preferences_service
        self._lrclib = lrclib_repository
        self._cache: OrderedDict[tuple, LibraryLyricsResponse | None] = OrderedDict()
        # fetched-but-unwritable lyrics (read-only mounts), keyed by path
        self._fetched: OrderedDict[str, LibraryLyricsResponse] = OrderedDict()
        # LRCLIB misses: path -> retry-allowed-after monotonic-ish epoch seconds
        self._negative: dict[str, float] = {}

    async def get_lyrics(self, file_id: str) -> LibraryLyricsResponse | None:
        """Lyrics for a library file id, or None when nothing is found.

        Raises ResourceNotFoundError for unknown ids / missing files and
        PermissionError for paths outside the configured library roots
        (same contract as the local stream endpoint).
        """
        audio_path = await self._local_files.resolve_validated_path(file_id)
        local = await asyncio.to_thread(self._get_lyrics_sync, audio_path)
        if local is not None:
            return local
        if not self._online_fetch_enabled():
            return None  # setting off = byte-identical local-only behaviour
        return await self._get_online_lyrics(file_id, audio_path)

    def _online_fetch_enabled(self) -> bool:
        if self._lrclib is None or self._preferences is None:
            return False
        try:
            return bool(self._preferences.get_library_settings().lyrics_fetch_enabled)
        except Exception:  # noqa: BLE001 - unreadable settings must not break lyrics
            return False

    async def _get_online_lyrics(
        self, file_id: str, audio_path: Path
    ) -> LibraryLyricsResponse | None:
        key = str(audio_path)
        fetched = self._fetched.get(key)
        if fetched is not None:
            self._fetched.move_to_end(key)
            return fetched
        now = time.time()
        retry_after = self._negative.get(key)
        if retry_after is not None and now < retry_after:
            return None
        self._negative.pop(key, None)

        row = None
        if self._library_repo is not None:
            try:
                row = await self._library_repo.get_file_row_by_id(file_id)
            except Exception as exc:  # noqa: BLE001 - metadata lookup is best-effort
                logger.debug("Lyrics metadata lookup failed for %s: %s", file_id, exc)
        if not row:
            self._remember_miss(key, now)
            return None
        return await self._fetch_and_store(
            audio_path,
            artist_name=row.get("artist_name") or row.get("album_artist_name"),
            track_name=row.get("track_title"),
            album_name=row.get("album_title"),
            duration_seconds=row.get("duration_seconds"),
        )

    async def _fetch_and_store(
        self,
        audio_path: Path,
        *,
        artist_name: str | None,
        track_name: str | None,
        album_name: str | None = None,
        duration_seconds: float | None = None,
    ) -> LibraryLyricsResponse | None:
        """Ask LRCLIB once and persist a hit as a ``.lrc`` sidecar.

        Shared by the player's on-demand fetch and the post-import hook. A miss
        is negative-cached; an unwritable sidecar (read-only mount) falls back
        to the in-memory fetched cache."""
        key = str(audio_path)
        now = time.time()
        result = await self._lrclib.fetch_lyrics(
            artist_name=artist_name,
            track_name=track_name,
            album_name=album_name,
            duration_seconds=duration_seconds,
        )
        response = _build_response(result[0]) if result else None
        if response is None:
            self._remember_miss(key, now)
            return None

        # audio_path came from resolve_validated_path (or is a freshly imported
        # library file), so the same-directory sidecar is inside the library
        # roots by construction.
        sidecar = audio_path.with_suffix(".lrc")
        try:
            await asyncio.to_thread(sidecar.write_text, result[0], encoding="utf-8")
        except OSError as exc:
            logger.warning(
                "Could not write lyrics sidecar %s (%s); serving fetched lyrics "
                "from memory",
                sidecar,
                exc,
            )
            self._fetched[key] = response
            if len(self._fetched) > self._CACHE_MAX:
                self._fetched.popitem(last=False)
        return response

    async def fetch_lyrics_for_new_import(
        self,
        audio_path: Path,
        *,
        artist_name: str | None,
        track_title: str | None,
        album_title: str | None = None,
        duration_seconds: float | None = None,
    ) -> None:
        """Post-import hook: fetch + write a ``.lrc`` sidecar for a track that
        just landed in the library, when the admin has enabled the LRCLIB fetch
        and the file has no lyrics yet (sidecar or embedded).

        Best-effort by contract: every failure is swallowed and logged - a
        lyrics problem must never fail or slow an import. Setting off = no-op."""
        try:
            if not self._online_fetch_enabled():
                return
            audio_path = Path(audio_path)
            existing = await asyncio.to_thread(self._get_lyrics_sync, audio_path)
            if existing is not None:
                return  # sidecar/embedded lyrics already present
            key = str(audio_path)
            if key in self._fetched:
                return
            retry_after = self._negative.get(key)
            if retry_after is not None and time.time() < retry_after:
                return
            self._negative.pop(key, None)
            await self._fetch_and_store(
                audio_path,
                artist_name=artist_name,
                track_name=track_title,
                album_name=album_title,
                duration_seconds=duration_seconds,
            )
        except Exception as exc:  # noqa: BLE001 - lyrics must never break an import
            logger.warning(
                "Post-import lyrics fetch failed for %s: %s", audio_path, exc
            )

    def _remember_miss(self, key: str, now: float) -> None:
        if len(self._negative) >= self._NEGATIVE_MAX:
            self._negative = {
                k: expiry for k, expiry in self._negative.items() if expiry > now
            }
        self._negative[key] = now + self._NEGATIVE_TTL_SECONDS

    def _get_lyrics_sync(self, audio_path: Path) -> LibraryLyricsResponse | None:
        key = self._cache_key(audio_path)
        if key in self._cache:
            self._cache.move_to_end(key)
            return self._cache[key]

        result = self._extract(audio_path)
        self._cache[key] = result
        if len(self._cache) > self._CACHE_MAX:
            self._cache.popitem(last=False)
        return result

    @staticmethod
    def _cache_key(audio_path: Path) -> tuple:
        parts: list = [str(audio_path)]
        for candidate in (audio_path, *(
            audio_path.with_suffix(suffix) for suffix in _SIDECAR_SUFFIXES
        )):
            try:
                parts.append(candidate.stat().st_mtime_ns)
            except OSError:
                parts.append(None)
        return tuple(parts)

    @staticmethod
    def _extract(audio_path: Path) -> LibraryLyricsResponse | None:
        sidecar_text = _read_sidecar(audio_path)
        if sidecar_text:
            response = _build_response(sidecar_text)
            if response is not None:
                return response
        try:
            return _extract_embedded(audio_path)
        except Exception as exc:  # noqa: BLE001 - unreadable/corrupt tags => no lyrics
            logger.debug("Embedded lyrics read failed for %s: %s", audio_path, exc)
            return None
