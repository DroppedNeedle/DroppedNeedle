"""AudioTagger - the mutagen seam.

Reads tag metadata and cover art across the legacy scan formats:
MP3 (ID3v2), FLAC/OGG (Vorbis comments), and M4A (MP4 atoms). The MusicBrainz
fields use the Picard tag names. All writes go through the staged management
metadata engine and its per-format adapters.

This is the mock seam for the scanner: tests mock ``AudioTagger`` (or
``mutagen.File``), never mutagen's per-format classes directly.
"""

import logging
from pathlib import Path
from typing import Any, BinaryIO

import mutagen
from mutagen.flac import FLAC
from mutagen.mp3 import MP3
from mutagen.mp4 import MP4

from core.exceptions import AudioFormatError
from infrastructure.audio.metadata_engine import (
    AUDIO_EXTENSION_FORMATS,
    AudioMetadataEngine,
    legacy_audio_projection,
)
from models.audio import AudioArtistCredit, AudioInfo, AudioTag
from models.audio_metadata import ReadAudioDocument

logger = logging.getLogger(__name__)

# Picard MusicBrainz tag names per format. Keys are AudioTag attribute names.
# NOTE: the *Recording* MBID (musicbrainz_recording_id) is NOT the "Release Track
# Id" - it is Vorbis MUSICBRAINZ_TRACKID / ID3 UFID:http://musicbrainz.org / MP4
# "MusicBrainz Track Id". (ID3 stores it in a UFID frame, handled separately below,
# so it is intentionally absent from _ID3_MB.)
_MB_UFID_OWNER = "http://musicbrainz.org"
_VORBIS_MB = {
    "musicbrainz_release_group_id": "MUSICBRAINZ_RELEASEGROUPID",
    "musicbrainz_release_id": "MUSICBRAINZ_ALBUMID",
    "musicbrainz_recording_id": "MUSICBRAINZ_TRACKID",
    "musicbrainz_artist_id": "MUSICBRAINZ_ARTISTID",
    "musicbrainz_album_artist_id": "MUSICBRAINZ_ALBUMARTISTID",
    "acoustid_id": "ACOUSTID_ID",
}
_ID3_MB = {
    "musicbrainz_release_group_id": "MusicBrainz Release Group Id",
    "musicbrainz_release_id": "MusicBrainz Album Id",
    "musicbrainz_artist_id": "MusicBrainz Artist Id",
    "musicbrainz_album_artist_id": "MusicBrainz Album Artist Id",
    "acoustid_id": "Acoustid Id",
}
_MP4_PREFIX = "----:com.apple.iTunes:"
_MP4_MB = {
    "musicbrainz_release_group_id": f"{_MP4_PREFIX}MusicBrainz Release Group Id",
    "musicbrainz_release_id": f"{_MP4_PREFIX}MusicBrainz Album Id",
    "musicbrainz_recording_id": f"{_MP4_PREFIX}MusicBrainz Track Id",
    "musicbrainz_artist_id": f"{_MP4_PREFIX}MusicBrainz Artist Id",
    "musicbrainz_album_artist_id": f"{_MP4_PREFIX}MusicBrainz Album Artist Id",
    "acoustid_id": f"{_MP4_PREFIX}Acoustid Id",
}

_SUFFIX_FORMATS = {
    ".flac": "flac",
    ".mp3": "mp3",
    ".ogg": "ogg",
    ".oga": "ogg",
    ".opus": "opus",
    ".m4a": "m4a",
    ".m4b": "m4a",
    ".mp4": "m4a",
}

# Only these containers report a meaningful bit depth. MP4Info.bits_per_sample
# is 16 even for lossy AAC, so bit_depth must be suppressed for lossy formats.
_LOSSLESS_FORMATS = {"flac", "wav"}
_MP4_ALAC_CODECS = {"alac"}


def _bit_depth_for(fmt: str, info: Any) -> int | None:
    """Meaningful bit depth, or None when the container/codec proves nothing.

    F-EDITION-04: an MP4-family file (m4a/mp4/mov) is ALAC - and therefore
    lossless with a real bit depth - only when Mutagen's codec evidence says
    so. AAC's synthetic 16-bit value stays suppressed."""
    if fmt in _LOSSLESS_FORMATS:
        return getattr(info, "bits_per_sample", None) or None
    if fmt in {"m4a", "mp4", "mov"}:
        codec = str(getattr(info, "codec", "") or "").lower()
        if codec in _MP4_ALAC_CODECS:
            return getattr(info, "bits_per_sample", None) or None
    return None


def _first(value: Any) -> str | None:
    """First element of a mutagen value list, coerced to ``str``."""
    if value is None:
        return None
    if isinstance(value, (list, tuple)):
        if not value:
            return None
        value = value[0]
    text = str(value).strip()
    return text or None


def _join_all(value: Any, sep: str = "; ") -> str | None:
    """Every value of a (commonly multi-valued) mutagen field, de-duplicated in
    order and joined with ``sep``.

    Genre lives as a list of Vorbis ``GENRE`` fields, an MP4 ``©gen`` list, or an
    ID3 ``TCON`` frame whose text is itself a list (multi-value frames stringify
    with NUL joins). Reading only the first value (``_first``) silently hides the
    rest from our DB/UI, so genre uses this instead to surface the truth."""
    return sep.join(_all_values(value)) or None


def _all_values(value: Any) -> list[str]:
    """Return only format-native values; punctuation inside one value is opaque."""
    if value is None:
        return []
    raw = getattr(value, "text", value)  # an ID3 frame -> its text list
    items = raw if isinstance(raw, (list, tuple)) else [raw]
    out: list[str] = []
    seen: set[str] = set()
    for item in items:
        text = (
            bytes(item).decode("utf-8", "replace")
            if isinstance(item, (bytes, bytearray))
            else str(item)
        )
        for part in text.split("\x00"):
            part = part.strip()
            if part and part not in seen:
                seen.add(part)
                out.append(part)
    return out


def _artist_credits(
    names: list[str], artist_ids: list[str], sort_name: str | None
) -> list[AudioArtistCredit]:
    return [
        AudioArtistCredit(
            name=name,
            credited_name=name,
            sort_name=sort_name if len(names) == 1 else None,
            musicbrainz_artist_id=artist_ids[position]
            if position < len(artist_ids)
            else None,
        )
        for position, name in enumerate(names)
    ]


def _leading_int(value: Any) -> int | None:
    """Parse the leading integer of a ``"5"`` or ``"5/12"`` style field."""
    text = _first(value)
    if text is None:
        return None
    head = text.split("/", 1)[0].strip()
    try:
        return int(head)
    except ValueError:
        return None


def _year(value: Any) -> int | None:
    text = _first(value)
    if text is None:
        return None
    try:
        return int(text[:4])
    except ValueError:
        return None


def _float_tag(value: Any, *, positive: bool = False) -> float | None:
    text = _first(value)
    if text is None:
        return None
    normalized = text.lower().removesuffix("db").strip()
    try:
        result = float(normalized)
    except ValueError:
        return None
    if positive and result < 0:
        return None
    return result


class AudioTagger:
    """Format-dispatching wrapper over mutagen. No state - safe as a singleton."""

    def __init__(self, metadata_engine: AudioMetadataEngine | None = None) -> None:
        self._metadata_engine = metadata_engine or AudioMetadataEngine()

    @staticmethod
    def _open(path: Path) -> Any:
        """Load via mutagen, normalising both the ``None`` and corrupt-file
        (``MutagenError``) cases into a single ``ValueError``."""
        try:
            audio = mutagen.File(path)
        except mutagen.MutagenError as exc:
            raise ValueError(f"Unsupported or unreadable audio file: {path}") from exc
        if audio is None:
            raise ValueError(f"Unsupported or unreadable audio file: {path}")
        return audio

    def read_tags(self, path: Path) -> tuple[AudioTag, AudioInfo]:
        """Read tags + technical info. Raises ``ValueError`` on unreadable files."""
        if path.suffix.casefold() in AUDIO_EXTENSION_FORMATS:
            try:
                return legacy_audio_projection(self.read_document(path))
            except AudioFormatError as error:
                raise ValueError("Unsupported or unreadable audio file.") from error
        audio = self._open(path)
        fmt = _SUFFIX_FORMATS.get(Path(path).suffix.lower(), "")
        if isinstance(audio, MP4):
            tag = self._read_mp4(audio)
        elif isinstance(audio, MP3):
            tag = self._read_id3(audio)
        else:
            tag = self._read_vorbis(audio)
        info = self._read_info(audio, Path(path), fmt)
        return tag, info

    def read_document(self, path: Path) -> ReadAudioDocument:
        """Read an admitted format without generic Mutagen dispatch."""
        return self._metadata_engine.read(path)

    def read_cover_art(self, path: Path | BinaryIO) -> bytes | None:
        try:
            audio = mutagen.File(path)
        except mutagen.MutagenError:
            return None
        if audio is None:
            return None
        if isinstance(audio, MP4):
            covers = audio.tags.get("covr") if audio.tags else None
            return bytes(covers[0]) if covers else None
        if isinstance(audio, FLAC):
            return bytes(audio.pictures[0].data) if audio.pictures else None
        tags = getattr(audio, "tags", None)
        if tags is not None:
            for frame in tags.getall("APIC") if hasattr(tags, "getall") else []:
                return bytes(frame.data)
        return None

    def _read_id3(self, audio: Any) -> AudioTag:
        tags = audio.tags
        if tags is None:
            return AudioTag(title="", artist="", album="", track_number=0)
        genres = _all_values(tags.get("TCON"))
        artist_ids = _all_values(tags.get("TXXX:MusicBrainz Artist Id"))
        album_artist_ids = _all_values(tags.get("TXXX:MusicBrainz Album Artist Id"))
        artist_names = _all_values(tags.get("TXXX:Artists")) or _all_values(
            tags.get("TPE1")
        )
        album_artist_names = _all_values(tags.get("TPE2"))
        return AudioTag(
            title=_first(tags.get("TIT2")) or "",
            artist=_first(tags.get("TPE1")) or "",
            album=_first(tags.get("TALB")) or "",
            album_artist=_first(tags.get("TPE2")),
            track_number=_leading_int(tags.get("TRCK")) or 0,
            disc_number=_leading_int(tags.get("TPOS")) or 1,
            year=_year(tags.get("TDRC")),
            genre=_join_all(tags.get("TCON")),
            compilation=_first(tags.get("TCMP")) == "1",
            title_sort=_first(tags.get("TSOT")),
            artist_sort=_first(tags.get("TSOP")),
            album_sort=_first(tags.get("TSOA")),
            album_artist_sort=_first(tags.get("TSO2")),
            disc_subtitle=_first(tags.get("TSST")),
            original_release_date=_first(tags.get("TDOR")),
            replaygain_track_gain=_float_tag(tags.get("TXXX:REPLAYGAIN_TRACK_GAIN")),
            replaygain_album_gain=_float_tag(tags.get("TXXX:REPLAYGAIN_ALBUM_GAIN")),
            replaygain_track_peak=_float_tag(
                tags.get("TXXX:REPLAYGAIN_TRACK_PEAK"), positive=True
            ),
            replaygain_album_peak=_float_tag(
                tags.get("TXXX:REPLAYGAIN_ALBUM_PEAK"), positive=True
            ),
            genres=genres,
            artists=_artist_credits(artist_names, artist_ids, _first(tags.get("TSOP"))),
            album_artists=_artist_credits(
                album_artist_names, album_artist_ids, _first(tags.get("TSO2"))
            ),
            musicbrainz_artist_ids=artist_ids,
            musicbrainz_album_artist_ids=album_artist_ids,
            **self._read_id3_mb(tags),
        )

    def _read_id3_mb(self, tags: Any) -> dict[str, str | None]:
        out: dict[str, str | None] = {}
        for field, desc in _ID3_MB.items():
            frame = tags.get(f"TXXX:{desc}")
            out[field] = _first(frame.text) if frame is not None else None
        # The recording MBID is a UFID frame (owner = musicbrainz.org), not a TXXX.
        ufid = tags.get(f"UFID:{_MB_UFID_OWNER}")
        out["musicbrainz_recording_id"] = (
            bytes(ufid.data).decode("utf-8", "ignore").strip() or None
            if ufid is not None
            else None
        )
        return out

    def _read_vorbis(self, audio: Any) -> AudioTag:
        def g(key: str) -> Any:
            return audio.get(key)

        mb = {field: _first(g(key)) for field, key in _VORBIS_MB.items()}
        genres = _all_values(g("GENRE"))
        artist_ids = _all_values(g("MUSICBRAINZ_ARTISTID"))
        album_artist_ids = _all_values(g("MUSICBRAINZ_ALBUMARTISTID"))
        artist_names = _all_values(g("ARTISTS")) or _all_values(g("ARTIST"))
        album_artist_names = _all_values(g("ALBUMARTISTS")) or _all_values(
            g("ALBUMARTIST")
        )
        return AudioTag(
            title=_first(g("TITLE")) or "",
            artist=_first(g("ARTIST")) or "",
            album=_first(g("ALBUM")) or "",
            album_artist=_first(g("ALBUMARTIST")),
            track_number=_leading_int(g("TRACKNUMBER")) or 0,
            disc_number=_leading_int(g("DISCNUMBER")) or 1,
            year=_year(g("DATE")),
            genre=_join_all(g("GENRE")),
            compilation=_first(g("COMPILATION")) == "1",
            title_sort=_first(g("TITLESORT")),
            artist_sort=_first(g("ARTISTSORT")),
            album_sort=_first(g("ALBUMSORT")),
            album_artist_sort=_first(g("ALBUMARTISTSORT")),
            disc_subtitle=_first(g("DISCSUBTITLE")),
            original_release_date=_first(g("ORIGINALDATE")),
            replaygain_track_gain=_float_tag(g("REPLAYGAIN_TRACK_GAIN")),
            replaygain_album_gain=_float_tag(g("REPLAYGAIN_ALBUM_GAIN")),
            replaygain_track_peak=_float_tag(g("REPLAYGAIN_TRACK_PEAK"), positive=True),
            replaygain_album_peak=_float_tag(g("REPLAYGAIN_ALBUM_PEAK"), positive=True),
            genres=genres,
            artists=_artist_credits(artist_names, artist_ids, _first(g("ARTISTSORT"))),
            album_artists=_artist_credits(
                album_artist_names,
                album_artist_ids,
                _first(g("ALBUMARTISTSORT")),
            ),
            musicbrainz_artist_ids=artist_ids,
            musicbrainz_album_artist_ids=album_artist_ids,
            **mb,
        )

    def _read_mp4(self, audio: Any) -> AudioTag:
        tags = audio.tags or {}

        def text(key: str) -> str | None:
            return _first(tags.get(key))

        def pair(key: str) -> int | None:
            value = tags.get(key)
            if value and isinstance(value[0], (list, tuple)) and value[0]:
                return int(value[0][0])
            return None

        def freeform(key: str) -> str | None:
            raw = tags.get(key)
            if not raw:
                return None
            value = raw[0]
            if isinstance(value, bytes):
                return value.decode("utf-8", "replace").strip() or None
            return _first(value)

        mb: dict[str, str | None] = {}
        for field, key in _MP4_MB.items():
            raw = tags.get(key)
            mb[field] = (
                bytes(raw[0]).decode("utf-8", "ignore").strip() or None if raw else None
            )
        genres = _all_values(tags.get("\xa9gen"))
        artist_ids = _all_values(tags.get(_MP4_MB["musicbrainz_artist_id"]))
        album_artist_ids = _all_values(tags.get(_MP4_MB["musicbrainz_album_artist_id"]))
        artist_names = _all_values(tags.get(f"{_MP4_PREFIX}ARTISTS")) or _all_values(
            tags.get("\xa9ART")
        )
        album_artist_names = _all_values(tags.get("aART"))
        cpil = tags.get("cpil")  # mutagen returns a plain bool for this atom
        if isinstance(cpil, list):
            cpil = cpil[0] if cpil else False
        return AudioTag(
            title=text("\xa9nam") or "",
            artist=text("\xa9ART") or "",
            album=text("\xa9alb") or "",
            album_artist=text("aART"),
            track_number=pair("trkn") or 0,
            disc_number=pair("disk") or 1,
            year=_year(text("\xa9day")),
            genre=_join_all(tags.get("\xa9gen")),
            compilation=bool(cpil),
            title_sort=text("sonm"),
            artist_sort=text("soar"),
            album_sort=text("soal"),
            album_artist_sort=text("soaa"),
            disc_subtitle=freeform(f"{_MP4_PREFIX}DISCSUBTITLE"),
            original_release_date=freeform(f"{_MP4_PREFIX}ORIGINALDATE"),
            replaygain_track_gain=_float_tag(
                freeform(f"{_MP4_PREFIX}REPLAYGAIN_TRACK_GAIN")
            ),
            replaygain_album_gain=_float_tag(
                freeform(f"{_MP4_PREFIX}REPLAYGAIN_ALBUM_GAIN")
            ),
            replaygain_track_peak=_float_tag(
                freeform(f"{_MP4_PREFIX}REPLAYGAIN_TRACK_PEAK"), positive=True
            ),
            replaygain_album_peak=_float_tag(
                freeform(f"{_MP4_PREFIX}REPLAYGAIN_ALBUM_PEAK"), positive=True
            ),
            genres=genres,
            artists=_artist_credits(artist_names, artist_ids, text("soar")),
            album_artists=_artist_credits(
                album_artist_names, album_artist_ids, text("soaa")
            ),
            musicbrainz_artist_ids=artist_ids,
            musicbrainz_album_artist_ids=album_artist_ids,
            **mb,
        )

    def _read_info(self, audio: Any, path: Path, fmt: str) -> AudioInfo:
        info = audio.info
        bit_depth = _bit_depth_for(fmt, info)
        return AudioInfo(
            duration_seconds=float(getattr(info, "length", 0.0) or 0.0),
            bitrate=int(getattr(info, "bitrate", 0) or 0) // 1000,
            sample_rate=int(getattr(info, "sample_rate", 0) or 0),
            channels=int(getattr(info, "channels", 0) or 0),
            file_format=fmt or (path.suffix.lower().lstrip(".") or "unknown"),
            file_size_bytes=path.stat().st_size if path.exists() else 0,
            bit_depth=bit_depth,
        )
