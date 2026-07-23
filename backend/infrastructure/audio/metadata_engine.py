"""Explicit read adapters for every audio extension admitted by DroppedNeedle."""

import base64
import binascii
from collections.abc import Callable, Mapping, Sequence
from io import BytesIO
import hashlib
import math
from pathlib import Path
import re
import stat
import struct
from typing import Literal
import uuid
import warnings

from PIL import Image, UnidentifiedImageError
from mutagen import MutagenError
from mutagen.aac import AAC
from mutagen.apev2 import APEBinaryValue, APEv2, error as APEError
from mutagen.asf import ASF
from mutagen.flac import FLAC, Picture
from mutagen.id3 import ID3
from mutagen.mp3 import MP3
from mutagen.mp4 import MP4
from mutagen.oggopus import OggOpus
from mutagen.oggvorbis import OggVorbis
from mutagen.wave import WAVE

from api.v1.schemas.library_management import (
    AUDIO_MANAGED_FIELD_NAMES,
    ENRICHMENT_MANAGED_FIELD_NAMES,
    LYRICS_MANAGED_FIELD_NAMES,
    MANAGED_FIELD_NAMES,
    REPLAYGAIN_MANAGED_FIELD_NAMES,
)
from core.exceptions import AudioFormatMismatchError, UnsupportedAudioFormatError
from infrastructure.audio.riff_info import read_riff_info
from infrastructure.audio.lyrics import parse_lrc, render_lrc
from models.audio import AudioArtistCredit, AudioInfo, AudioTag
from models.audio_metadata import (
    AudioContainer,
    AudioCleanupAction,
    AudioCustomTagMutation,
    AudioFieldMutation,
    AudioMetadataDocument,
    AudioSemanticField,
    AudioTechnicalInfo,
    AudioWriteCompatibility,
    AudioWritePlan,
    AudioWritePolicy,
    AudioWriteResult,
    CapabilityReport,
    DesiredAudioDocument,
    EmbeddedArtworkDescriptor,
    FileAttributeSnapshot,
    FormatCapabilities,
    FormatProbe,
    NativeMetadataSnapshot,
    NativeTagEntry,
    NativeTagValue,
    RawTagDescriptor,
    ReadAudioDocument,
    SemanticTagSnapshot,
)
from models.library_management_artwork import ArtworkImageType
from models.library_management_scripts import CustomTagValue

_MB_UFID_OWNER = "http://musicbrainz.org"
_MP4_PREFIX = "----:com.apple.iTunes:"
_MAX_EMBEDDED_ART_BYTES = 50 * 1024 * 1024
_MAX_EMBEDDED_ART_PIXELS = 100_000_000
_SNAPSHOT_VERSION = 1
_ADAPTER_VERSION = "1"

AUDIO_EXTENSION_FORMATS: dict[str, AudioContainer] = {
    ".flac": "flac",
    ".mp3": "mp3",
    ".ogg": "ogg",
    ".opus": "opus",
    ".m4a": "m4a",
    ".aac": "aac",
    ".wav": "wav",
    ".wma": "wma",
}

_ARTWORK_MIMES = ("image/jpeg", "image/png", "image/webp", "image/gif")
LYRICS_FIELDS = LYRICS_MANAGED_FIELD_NAMES
REPLAYGAIN_FIELDS = REPLAYGAIN_MANAGED_FIELD_NAMES
ENRICHMENT_AUDIO_FIELDS = ENRICHMENT_MANAGED_FIELD_NAMES
_ALL_FIELDS = AUDIO_MANAGED_FIELD_NAMES
_READ_FIELD_NAMES = AUDIO_MANAGED_FIELD_NAMES
_SYNCED_LYRICS_UNSUPPORTED = ("lyrics_synced",)
_MP4_UNSUPPORTED = ("performer", "arranger", *_SYNCED_LYRICS_UNSUPPORTED)
_APE_UNSUPPORTED = _SYNCED_LYRICS_UNSUPPORTED
_ASF_UNSUPPORTED = (
    "total_tracks",
    "movement",
    "movement_number",
    "movement_count",
    "performer",
    "arranger",
)
_ORDERED_FIELDS = {
    "artist",
    "album_artist",
    "artist_sort",
    "album_artist_sort",
    "release_type",
    "label",
    "catalog_number",
    "isrc",
    "musicbrainz_artist_id",
    "musicbrainz_album_artist_id",
    "musicbrainz_work_id",
    "composer",
    "lyricist",
    "conductor",
    "performer",
    "arranger",
    "remixer",
    "producer",
    "genre",
}
_ID3V23_UNSUPPORTED = {
    "disc_subtitle",
    "movement",
    "movement_number",
    "movement_count",
}
_RIFF_INFO_FIELDS = {
    "album",
    "title",
    "artist",
    "track_number",
    "date",
    "genre",
    "media",
    "composer",
    "producer",
}

FORMAT_CAPABILITIES: dict[AudioContainer, FormatCapabilities] = {
    "flac": FormatCapabilities(
        audio_format="flac",
        extensions=(".flac",),
        adapter_name="flac-vorbis-comments",
        tag_format="Vorbis comments",
        readable=True,
        writable=True,
        native_multivalue=True,
        artwork_readable=True,
        artwork_writable=True,
        artwork_types_preserved=True,
        artwork_mime_types=_ARTWORK_MIMES,
        supported_fields=tuple(
            field for field in _ALL_FIELDS if field not in _SYNCED_LYRICS_UNSUPPORTED
        ),
        unsupported_fields=_SYNCED_LYRICS_UNSUPPORTED,
    ),
    "mp3": FormatCapabilities(
        audio_format="mp3",
        extensions=(".mp3",),
        adapter_name="mp3-id3v2",
        tag_format="ID3v2",
        readable=True,
        writable=True,
        native_multivalue=True,
        artwork_readable=True,
        artwork_writable=True,
        artwork_types_preserved=True,
        artwork_mime_types=_ARTWORK_MIMES,
        supported_fields=_ALL_FIELDS,
        unsupported_fields=(),
        representation_warnings=(
            "ID3v2.3 flattens configured multi-value fields when explicitly selected.",
        ),
    ),
    "ogg": FormatCapabilities(
        audio_format="ogg",
        extensions=(".ogg",),
        adapter_name="ogg-vorbis-comments",
        tag_format="Vorbis comments",
        readable=True,
        writable=True,
        native_multivalue=True,
        artwork_readable=True,
        artwork_writable=True,
        artwork_types_preserved=True,
        artwork_mime_types=_ARTWORK_MIMES,
        supported_fields=tuple(
            field for field in _ALL_FIELDS if field not in _SYNCED_LYRICS_UNSUPPORTED
        ),
        unsupported_fields=_SYNCED_LYRICS_UNSUPPORTED,
    ),
    "opus": FormatCapabilities(
        audio_format="opus",
        extensions=(".opus",),
        adapter_name="ogg-opus-comments",
        tag_format="Vorbis comments",
        readable=True,
        writable=True,
        native_multivalue=True,
        artwork_readable=True,
        artwork_writable=True,
        artwork_types_preserved=True,
        artwork_mime_types=_ARTWORK_MIMES,
        supported_fields=tuple(
            field for field in _ALL_FIELDS if field not in _SYNCED_LYRICS_UNSUPPORTED
        ),
        unsupported_fields=_SYNCED_LYRICS_UNSUPPORTED,
    ),
    "m4a": FormatCapabilities(
        audio_format="m4a",
        extensions=(".m4a",),
        adapter_name="mp4-atoms",
        tag_format="MP4 atoms",
        readable=True,
        writable=True,
        native_multivalue=True,
        artwork_readable=True,
        artwork_writable=True,
        artwork_types_preserved=False,
        artwork_mime_types=("image/jpeg", "image/png"),
        supported_fields=tuple(
            field for field in _ALL_FIELDS if field not in _MP4_UNSUPPORTED
        ),
        unsupported_fields=_MP4_UNSUPPORTED,
        representation_warnings=(
            "MP4 covr atoms preserve image bytes and MIME but not Picard image types.",
        ),
    ),
    "aac": FormatCapabilities(
        audio_format="aac",
        extensions=(".aac",),
        adapter_name="raw-aac-apev2",
        tag_format="APEv2",
        readable=True,
        writable=True,
        native_multivalue=True,
        artwork_readable=True,
        artwork_writable=True,
        artwork_types_preserved=True,
        artwork_mime_types=_ARTWORK_MIMES,
        supported_fields=tuple(
            field for field in _ALL_FIELDS if field not in _APE_UNSUPPORTED
        ),
        unsupported_fields=_APE_UNSUPPORTED,
        representation_warnings=(
            "Raw AAC technical duration may include appended APEv2 bytes in Mutagen 1.48.0.",
        ),
    ),
    "wav": FormatCapabilities(
        audio_format="wav",
        extensions=(".wav",),
        adapter_name="wave-id3",
        tag_format="ID3v2 in RIFF",
        readable=True,
        writable=True,
        native_multivalue=True,
        artwork_readable=True,
        artwork_writable=True,
        artwork_types_preserved=True,
        artwork_mime_types=_ARTWORK_MIMES,
        supported_fields=_ALL_FIELDS,
        unsupported_fields=(),
        representation_warnings=(
            "RIFF INFO mode cannot represent the full managed field set; strict profiles use ID3.",
        ),
    ),
    "wma": FormatCapabilities(
        audio_format="wma",
        extensions=(".wma",),
        adapter_name="asf-attributes",
        tag_format="ASF attributes",
        readable=True,
        writable=True,
        native_multivalue=True,
        artwork_readable=True,
        artwork_writable=True,
        artwork_types_preserved=True,
        artwork_mime_types=_ARTWORK_MIMES,
        supported_fields=tuple(
            field for field in _ALL_FIELDS if field not in _ASF_UNSUPPORTED
        ),
        unsupported_fields=_ASF_UNSUPPORTED,
    ),
}


def _values(value: object) -> tuple[str, ...]:
    if value is None:
        return ()
    raw = getattr(value, "text", value)
    items: Sequence[object]
    if isinstance(raw, (list, tuple)):
        items = raw
    else:
        items = (raw,)
    result: list[str] = []
    for item in items:
        item = getattr(item, "value", item)
        if isinstance(item, (bytes, bytearray, memoryview)):
            text = bytes(item).decode("utf-8", "replace")
        elif isinstance(item, tuple):
            text = "/".join(str(part) for part in item)
        else:
            text = str(item)
        for part in text.split("\x00"):
            normalized = part.strip()
            if normalized and normalized not in result:
                result.append(normalized)
    return tuple(result)


def _first(value: object) -> str | None:
    values = _values(value)
    return values[0] if values else None


def _integer(value: object) -> int | None:
    text = _first(value)
    if text is None:
        return None
    try:
        return int(text.split("/", 1)[0].strip())
    except ValueError:
        return None


def _pair(value: object) -> tuple[int | None, int | None]:
    text = _first(value)
    if text is None:
        return None, None
    parts = text.split("/", 1)
    try:
        number = int(parts[0])
    except ValueError:
        number = None
    try:
        total = int(parts[1]) if len(parts) == 2 and parts[1] else None
    except ValueError:
        total = None
    return number, total


def _bool(value: object) -> bool | None:
    text = _first(value)
    if text is None:
        return None
    return text.casefold() in {"1", "true", "yes"}


def _float(value: object, *, positive: bool = False) -> float | None:
    text = _first(value)
    if text is None:
        return None
    normalized = text.casefold().replace("db", "").strip()
    try:
        result = float(normalized)
    except ValueError:
        return None
    if not math.isfinite(result):
        return None
    return result if not positive or result >= 0 else None


def _field_document(
    values: Mapping[str, object],
    *,
    artist_display: str | None,
    album_artist_display: str | None,
) -> AudioMetadataDocument:
    fields: list[AudioSemanticField] = []
    for name in _READ_FIELD_NAMES:
        value = values.get(name)
        if value is None or value == () or value == "":
            continue
        fields.append(AudioSemanticField(name=name, value=value))
    return AudioMetadataDocument(
        fields=tuple(fields),
        artist_display=artist_display,
        album_artist_display=album_artist_display,
    )


def _picture_type(value: int) -> ArtworkImageType:
    return {
        3: "front",
        4: "back",
        5: "booklet",
        6: "medium",
    }.get(value, "other")


def _sniff_mime(data: bytes) -> str | None:
    if data.startswith(b"\xff\xd8\xff"):
        return "image/jpeg"
    if data.startswith(b"\x89PNG\r\n\x1a\n"):
        return "image/png"
    if data.startswith((b"GIF87a", b"GIF89a")):
        return "image/gif"
    if len(data) >= 12 and data[:4] == b"RIFF" and data[8:12] == b"WEBP":
        return "image/webp"
    return None


def _artwork_descriptor(
    data: bytes,
    *,
    image_type: ArtworkImageType,
    declared_mime: str | None,
    description: str,
) -> EmbeddedArtworkDescriptor:
    actual_mime = _sniff_mime(data)
    width: int | None = None
    height: int | None = None
    supported = bool(data) and len(data) <= _MAX_EMBEDDED_ART_BYTES
    if supported:
        try:
            with warnings.catch_warnings():
                warnings.simplefilter("error", Image.DecompressionBombWarning)
                with Image.open(BytesIO(data)) as image:
                    width, height = image.size
                    supported = (
                        width > 0
                        and height > 0
                        and width * height <= _MAX_EMBEDDED_ART_PIXELS
                        and image.format in {"JPEG", "PNG", "WEBP", "GIF"}
                    )
                    if supported:
                        image.verify()
        except (
            Image.DecompressionBombError,
            Image.DecompressionBombWarning,
            UnidentifiedImageError,
            OSError,
            SyntaxError,
            ValueError,
        ):
            supported = False
            width = None
            height = None
    if actual_mime is None:
        supported = False
    return EmbeddedArtworkDescriptor(
        image_type=image_type,
        mime_type=actual_mime or declared_mime,
        description=description,
        width=width,
        height=height,
        byte_size=len(data),
        sha256=hashlib.sha256(data).hexdigest(),
        content=data,
        format_supported=supported,
    )


def _technical(path: Path, audio: object) -> AudioTechnicalInfo:
    info = getattr(audio, "info")
    bit_depth = getattr(info, "bits_per_sample", None)
    codec = getattr(info, "codec", None)
    sample_rate = getattr(info, "sample_rate", None)
    if sample_rate is None and isinstance(audio, OggOpus):
        # Opus decodes at 48 kHz; Mutagen's OggOpusInfo does not expose the
        # otherwise universal output rate as a sample_rate attribute.
        sample_rate = 48_000
    return AudioTechnicalInfo(
        duration_seconds=float(getattr(info, "length", 0.0) or 0.0),
        bitrate_bps=int(getattr(info, "bitrate", 0) or 0),
        sample_rate_hz=int(sample_rate or 0),
        channels=int(getattr(info, "channels", 0) or 0),
        bit_depth=int(bit_depth) if bit_depth else None,
        codec=str(codec) if codec else None,
        file_size_bytes=path.stat().st_size,
    )


def _raw_tags(tags: object, binary_keys: set[str]) -> tuple[RawTagDescriptor, ...]:
    if tags is None or not hasattr(tags, "keys"):
        return ()
    descriptors: list[RawTagDescriptor] = []
    for raw_key in tags.keys():
        key = str(raw_key)
        value = tags[raw_key]
        folded = key.casefold()
        is_binary = (
            folded in binary_keys
            or folded.startswith(("apic:", "ufid:"))
            or isinstance(value, APEBinaryValue)
        )
        if is_binary:
            try:
                payload = bytes(value)
            except (TypeError, ValueError):
                payload = repr(value).encode()
            descriptors.append(
                RawTagDescriptor(
                    key=key,
                    values=(),
                    value_kind="binary",
                    binary_sha256=hashlib.sha256(payload).hexdigest(),
                )
            )
            continue
        descriptors.append(
            RawTagDescriptor(key=key, values=_values(value), value_kind="text")
        )
    return tuple(sorted(descriptors, key=lambda value: value.key.casefold()))


def _native_type_code(value: object) -> int | None:
    for attribute in ("imageformat", "dataformat", "TYPE", "kind"):
        raw = getattr(value, attribute, None)
        if raw is not None:
            try:
                return int(raw)
            except (TypeError, ValueError):
                return None
    return None


def _native_value(value: object) -> NativeTagValue:
    type_code = _native_type_code(value)
    language = getattr(value, "language", None)
    stream = getattr(value, "stream", None)
    if isinstance(value, bool):
        return NativeTagValue(kind="boolean", boolean=value, type_code=type_code)
    if isinstance(value, int):
        return NativeTagValue(kind="integer", integer=value, type_code=type_code)
    if isinstance(value, float):
        return NativeTagValue(kind="float", float_value=value, type_code=type_code)
    if isinstance(value, str):
        return NativeTagValue(kind="text", text=value, type_code=type_code)
    if isinstance(value, uuid.UUID):
        return NativeTagValue(kind="text", text=str(value), type_code=type_code)
    if isinstance(value, (bytes, bytearray, memoryview)):
        return NativeTagValue(kind="binary", binary=bytes(value), type_code=type_code)
    if isinstance(value, tuple) and all(isinstance(item, int) for item in value):
        return NativeTagValue(
            kind="integer_pair", integer_pair=tuple(value), type_code=type_code
        )

    raw = getattr(value, "value", value)
    if raw is not value:
        native = _native_value(raw)
        return NativeTagValue(
            kind=native.kind,
            text=native.text,
            integer=native.integer,
            float_value=native.float_value,
            boolean=native.boolean,
            binary=native.binary,
            integer_pair=native.integer_pair,
            type_code=type_code,
            language=int(language) if language is not None else None,
            stream=int(stream) if stream is not None else None,
        )
    return NativeTagValue(
        kind="opaque",
        text=repr(value),
        type_code=type_code,
        language=int(language) if language is not None else None,
        stream=int(stream) if stream is not None else None,
    )


def _native_entries(tags: object) -> tuple[NativeTagEntry, ...]:
    if tags is None or not hasattr(tags, "keys"):
        return ()
    entries: list[NativeTagEntry] = []
    for raw_key in tags.keys():
        key = str(raw_key)
        raw_value = tags[raw_key]
        items = (
            raw_value
            if isinstance(raw_value, (list, tuple))
            and not (
                isinstance(raw_value, tuple)
                and all(isinstance(item, int) for item in raw_value)
            )
            else [raw_value]
        )
        entries.append(
            NativeTagEntry(
                key=key,
                values=tuple(_native_value(item) for item in items),
                container=(
                    "list" if isinstance(raw_value, (list, tuple)) else "scalar"
                ),
            )
        )
    return tuple(sorted(entries, key=lambda value: value.key.casefold()))


def _encoded_id3(tags: object) -> bytes | None:
    if tags is None or not hasattr(tags, "values"):
        return None
    semantic_tags = ID3()
    for frame in tags.values():
        semantic_tags.add(frame)
    output = BytesIO()
    semantic_tags.save(output, v1=0)
    return output.getvalue()


def _native_metadata_snapshot(
    path: Path, audio_format: AudioContainer, tags: object
) -> NativeMetadataSnapshot:
    if audio_format in {"mp3", "wav"}:
        auxiliary_entries: tuple[NativeTagEntry, ...] = ()
        if audio_format == "mp3":
            try:
                auxiliary_entries = _native_entries(APEv2(path))
            except (APEError, MutagenError):
                pass
        riff_entries = (
            _native_entries(read_riff_info(path)) if audio_format == "wav" else ()
        )
        return NativeMetadataSnapshot(
            storage_kind="id3",
            encoded_id3=_encoded_id3(tags),
            auxiliary_storage_kind=(
                "apev2" if auxiliary_entries else "riff_info" if riff_entries else None
            ),
            auxiliary_entries=auxiliary_entries or riff_entries,
        )
    if audio_format in {"flac", "ogg", "opus"}:
        auxiliary_id3: bytes | None = None
        if audio_format == "flac":
            try:
                auxiliary_id3 = _encoded_id3(ID3(path))
            except MutagenError:
                pass
        return NativeMetadataSnapshot(
            storage_kind="vorbis_comments",
            entries=_native_entries(tags),
            auxiliary_storage_kind="id3" if auxiliary_id3 else None,
            auxiliary_encoded_id3=auxiliary_id3,
        )
    if audio_format == "m4a":
        return NativeMetadataSnapshot(
            storage_kind="mp4_atoms", entries=_native_entries(tags)
        )
    if audio_format == "aac":
        return NativeMetadataSnapshot(
            storage_kind="apev2", entries=_native_entries(tags)
        )
    return NativeMetadataSnapshot(
        storage_kind="asf_attributes", entries=_native_entries(tags)
    )


def _file_attributes(path: Path) -> FileAttributeSnapshot:
    file_stat = path.stat()
    return FileAttributeSnapshot(
        atime_ns=file_stat.st_atime_ns,
        mtime_ns=file_stat.st_mtime_ns,
        permission_bits=stat.S_IMODE(file_stat.st_mode),
    )


_VORBIS_KEYS: dict[str, tuple[str, ...]] = {
    "album": ("ALBUM",),
    "title": ("TITLE",),
    "album_sort": ("ALBUMSORT",),
    "title_sort": ("TITLESORT",),
    "artist": ("ARTISTS", "ARTIST"),
    "album_artist": ("ALBUMARTISTS", "ALBUMARTIST"),
    "artist_sort": ("ARTISTSORT",),
    "album_artist_sort": ("ALBUMARTISTSORT",),
    "disc_subtitle": ("DISCSUBTITLE",),
    "date": ("DATE",),
    "original_date": ("ORIGINALDATE",),
    "release_status": ("RELEASESTATUS", "MUSICBRAINZ_ALBUMSTATUS"),
    "release_country": ("RELEASECOUNTRY",),
    "release_type": ("RELEASETYPE", "MUSICBRAINZ_ALBUMTYPE"),
    "media": ("MEDIA",),
    "label": ("LABEL",),
    "catalog_number": ("CATALOGNUMBER",),
    "barcode": ("BARCODE",),
    "asin": ("ASIN",),
    "isrc": ("ISRC",),
    "musicbrainz_recording_id": ("MUSICBRAINZ_TRACKID",),
    "musicbrainz_release_track_id": ("MUSICBRAINZ_RELEASETRACKID",),
    "musicbrainz_release_id": ("MUSICBRAINZ_ALBUMID",),
    "musicbrainz_release_group_id": ("MUSICBRAINZ_RELEASEGROUPID",),
    "musicbrainz_artist_id": ("MUSICBRAINZ_ARTISTID",),
    "musicbrainz_album_artist_id": ("MUSICBRAINZ_ALBUMARTISTID",),
    "work": ("WORK",),
    "musicbrainz_work_id": ("MUSICBRAINZ_WORKID",),
    "movement": ("MOVEMENTNAME",),
    "composer": ("COMPOSER",),
    "lyricist": ("LYRICIST",),
    "conductor": ("CONDUCTOR",),
    "performer": ("PERFORMER",),
    "arranger": ("ARRANGER",),
    "remixer": ("REMIXER",),
    "producer": ("PRODUCER",),
    "acoustid_id": ("ACOUSTID_ID",),
    "acoustid_fingerprint": ("ACOUSTID_FINGERPRINT",),
    "lyrics_plain": ("LYRICS",),
    "replaygain_track_gain": ("REPLAYGAIN_TRACK_GAIN",),
    "replaygain_album_gain": ("REPLAYGAIN_ALBUM_GAIN",),
    "replaygain_track_peak": ("REPLAYGAIN_TRACK_PEAK",),
    "replaygain_album_peak": ("REPLAYGAIN_ALBUM_PEAK",),
}


def _mapped_values(
    getter: Callable[[str], object], mapping: Mapping[str, tuple[str, ...]]
) -> dict[str, object]:
    result: dict[str, object] = {}
    for field, keys in mapping.items():
        values: tuple[str, ...] = ()
        for key in keys:
            values = _values(getter(key))
            if values:
                break
        if not values:
            continue
        result[field] = (
            values
            if field
            in {
                "artist",
                "album_artist",
                "artist_sort",
                "album_artist_sort",
                "release_type",
                "label",
                "catalog_number",
                "isrc",
                "musicbrainz_artist_id",
                "musicbrainz_album_artist_id",
                "musicbrainz_work_id",
                "composer",
                "lyricist",
                "conductor",
                "performer",
                "arranger",
                "remixer",
                "producer",
            }
            else values[0]
        )
    return result


def _vorbis_document(tags: object) -> AudioMetadataDocument:
    getter = lambda key: tags.get(key) if tags is not None else None
    values = _mapped_values(getter, _VORBIS_KEYS)
    values["track_number"] = _integer(getter("TRACKNUMBER"))
    values["total_tracks"] = _integer(getter("TOTALTRACKS") or getter("TRACKTOTAL"))
    values["disc_number"] = _integer(getter("DISCNUMBER"))
    values["total_discs"] = _integer(getter("TOTALDISCS") or getter("DISCTOTAL"))
    values["movement_number"] = _integer(getter("MOVEMENT"))
    values["movement_count"] = _integer(getter("MOVEMENTTOTAL"))
    values["compilation"] = _bool(getter("COMPILATION"))
    values["genre"] = _values(getter("GENRE"))
    values["replaygain_track_gain"] = _float(getter("REPLAYGAIN_TRACK_GAIN"))
    values["replaygain_album_gain"] = _float(getter("REPLAYGAIN_ALBUM_GAIN"))
    values["replaygain_track_peak"] = _float(
        getter("REPLAYGAIN_TRACK_PEAK"), positive=True
    )
    values["replaygain_album_peak"] = _float(
        getter("REPLAYGAIN_ALBUM_PEAK"), positive=True
    )
    return _field_document(
        values,
        artist_display=_first(getter("ARTIST")),
        album_artist_display=_first(getter("ALBUMARTIST")),
    )


def _flac_artwork(audio: FLAC) -> tuple[EmbeddedArtworkDescriptor, ...]:
    return tuple(
        _artwork_descriptor(
            bytes(picture.data),
            image_type=_picture_type(picture.type),
            declared_mime=picture.mime or None,
            description=picture.desc or "",
        )
        for picture in audio.pictures
    )


def _ogg_artwork(tags: object) -> tuple[EmbeddedArtworkDescriptor, ...]:
    result: list[EmbeddedArtworkDescriptor] = []
    if tags is None:
        return ()
    for encoded in _values(tags.get("METADATA_BLOCK_PICTURE")):
        try:
            picture = Picture(base64.b64decode(encoded, validate=True))
        except (ValueError, TypeError, binascii.Error, MutagenError):
            continue
        result.append(
            _artwork_descriptor(
                bytes(picture.data),
                image_type=_picture_type(picture.type),
                declared_mime=picture.mime or None,
                description=picture.desc or "",
            )
        )
    return tuple(result)


_ID3_TXXX: dict[str, str] = {
    "release_status": "MusicBrainz Album Status",
    "release_country": "MusicBrainz Album Release Country",
    "release_type": "MusicBrainz Album Type",
    "label": "LABEL",
    "catalog_number": "CATALOGNUMBER",
    "barcode": "BARCODE",
    "asin": "ASIN",
    "musicbrainz_release_track_id": "MusicBrainz Release Track Id",
    "musicbrainz_release_id": "MusicBrainz Album Id",
    "musicbrainz_release_group_id": "MusicBrainz Release Group Id",
    "musicbrainz_artist_id": "MusicBrainz Artist Id",
    "musicbrainz_album_artist_id": "MusicBrainz Album Artist Id",
    "work": "WORK",
    "musicbrainz_work_id": "MusicBrainz Work Id",
    "arranger": "ARRANGER",
    "producer": "PRODUCER",
    "acoustid_id": "Acoustid Id",
    "acoustid_fingerprint": "Acoustid Fingerprint",
}


def _id3_document(tags: object) -> AudioMetadataDocument:
    get = lambda key: tags.get(key) if tags is not None else None
    values: dict[str, object] = {
        "album": _first(get("TALB")),
        "title": _first(get("TIT2")),
        "album_sort": _first(get("TSOA")),
        "title_sort": _first(get("TSOT")),
        "artist": _values(get("TXXX:Artists")) or _values(get("TPE1")),
        "album_artist": _values(get("TPE2")),
        "artist_sort": _values(get("TSOP")),
        "album_artist_sort": _values(get("TSO2")),
        "disc_subtitle": _first(get("TSST")),
        "date": _first(get("TDRC")),
        "original_date": _first(get("TDOR")),
        "media": _first(get("TMED")),
        "label": _values(get("TPUB")),
        "compilation": _bool(get("TCMP")),
        "genre": _values(get("TCON")),
        "isrc": _values(get("TSRC")),
        "movement": _first(get("MVNM")),
        "composer": _values(get("TCOM")),
        "lyricist": _values(get("TEXT")),
        "conductor": _values(get("TPE3")),
        "remixer": _values(get("TPE4")),
        "replaygain_track_gain": _float(get("TXXX:REPLAYGAIN_TRACK_GAIN")),
        "replaygain_album_gain": _float(get("TXXX:REPLAYGAIN_ALBUM_GAIN")),
        "replaygain_track_peak": _float(
            get("TXXX:REPLAYGAIN_TRACK_PEAK"), positive=True
        ),
        "replaygain_album_peak": _float(
            get("TXXX:REPLAYGAIN_ALBUM_PEAK"), positive=True
        ),
    }
    if tags is not None:
        unsynced = tags.getall("USLT")
        if unsynced:
            values["lyrics_plain"] = unsynced[0].text
        synced = next(
            (
                frame
                for frame in tags.getall("SYLT")
                if frame.type == 1 and frame.format == 2 and frame.text
            ),
            None,
        )
        if synced is not None:
            values["lyrics_synced"] = render_lrc(tuple(synced.text))
    track, total_tracks = _pair(get("TRCK"))
    disc, total_discs = _pair(get("TPOS"))
    movement, movement_count = _pair(get("MVIN"))
    values.update(
        track_number=track,
        total_tracks=total_tracks,
        disc_number=disc,
        total_discs=total_discs,
        movement_number=movement,
        movement_count=movement_count,
    )
    for field, description in _ID3_TXXX.items():
        raw = _values(get(f"TXXX:{description}"))
        if raw:
            values[field] = (
                raw
                if field
                in {
                    "release_type",
                    "label",
                    "catalog_number",
                    "musicbrainz_artist_id",
                    "musicbrainz_album_artist_id",
                    "musicbrainz_work_id",
                    "arranger",
                    "producer",
                }
                else raw[0]
            )
    ufid = get(f"UFID:{_MB_UFID_OWNER}")
    if ufid is not None:
        values["musicbrainz_recording_id"] = bytes(ufid.data).decode("utf-8", "replace")
    people_by_field: dict[str, list[str]] = {
        "performer": [],
        "arranger": [],
        "producer": [],
    }
    for key in ("TMCL", "TIPL", "IPLS"):
        for frame in tags.getall(key) if tags is not None else ():
            for role, person in getattr(frame, "people", ()):
                normalized_role = role.casefold().strip()
                if normalized_role == "arranger":
                    field = "arranger"
                    formatted = person
                elif normalized_role == "producer":
                    field = "producer"
                    formatted = person
                else:
                    field = "performer"
                    formatted = f"{person} ({role})" if role else person
                if formatted not in people_by_field[field]:
                    people_by_field[field].append(formatted)
    for field, people in people_by_field.items():
        if people:
            values[field] = tuple(people)
    return _field_document(
        values,
        artist_display=_first(get("TPE1")),
        album_artist_display=_first(get("TPE2")),
    )


def _id3_artwork(tags: object) -> tuple[EmbeddedArtworkDescriptor, ...]:
    if tags is None:
        return ()
    return tuple(
        _artwork_descriptor(
            bytes(frame.data),
            image_type=_picture_type(frame.type),
            declared_mime=frame.mime or None,
            description=frame.desc or "",
        )
        for frame in tags.getall("APIC")
    )


_MP4_KEYS: dict[str, tuple[str, ...]] = {
    "album": ("©alb",),
    "title": ("©nam",),
    "album_sort": ("soal",),
    "title_sort": ("sonm",),
    "artist": (f"{_MP4_PREFIX}ARTISTS", "©ART"),
    "album_artist": ("aART",),
    "artist_sort": ("soar",),
    "album_artist_sort": ("soaa",),
    "disc_subtitle": (f"{_MP4_PREFIX}DISCSUBTITLE",),
    "date": ("©day",),
    "original_date": (f"{_MP4_PREFIX}ORIGINALDATE",),
    "release_status": (f"{_MP4_PREFIX}MusicBrainz Album Status",),
    "release_country": (f"{_MP4_PREFIX}MusicBrainz Album Release Country",),
    "release_type": (f"{_MP4_PREFIX}MusicBrainz Album Type",),
    "media": (f"{_MP4_PREFIX}MEDIA",),
    "label": (f"{_MP4_PREFIX}LABEL",),
    "catalog_number": (f"{_MP4_PREFIX}CATALOGNUMBER",),
    "barcode": (f"{_MP4_PREFIX}BARCODE",),
    "asin": (f"{_MP4_PREFIX}ASIN",),
    "isrc": (f"{_MP4_PREFIX}ISRC",),
    "musicbrainz_recording_id": (f"{_MP4_PREFIX}MusicBrainz Track Id",),
    "musicbrainz_release_track_id": (f"{_MP4_PREFIX}MusicBrainz Release Track Id",),
    "musicbrainz_release_id": (f"{_MP4_PREFIX}MusicBrainz Album Id",),
    "musicbrainz_release_group_id": (f"{_MP4_PREFIX}MusicBrainz Release Group Id",),
    "musicbrainz_artist_id": (f"{_MP4_PREFIX}MusicBrainz Artist Id",),
    "musicbrainz_album_artist_id": (f"{_MP4_PREFIX}MusicBrainz Album Artist Id",),
    "work": ("©wrk",),
    "musicbrainz_work_id": (f"{_MP4_PREFIX}MusicBrainz Work Id",),
    "movement": ("©mvn",),
    "composer": ("©wrt",),
    "lyricist": (f"{_MP4_PREFIX}LYRICIST",),
    "conductor": (f"{_MP4_PREFIX}CONDUCTOR",),
    "arranger": (f"{_MP4_PREFIX}ARRANGER",),
    "remixer": (f"{_MP4_PREFIX}REMIXER",),
    "producer": (f"{_MP4_PREFIX}PRODUCER",),
    "acoustid_id": (f"{_MP4_PREFIX}Acoustid Id",),
    "acoustid_fingerprint": (f"{_MP4_PREFIX}Acoustid Fingerprint",),
    "lyrics_plain": ("©lyr",),
    "replaygain_track_gain": (f"{_MP4_PREFIX}REPLAYGAIN_TRACK_GAIN",),
    "replaygain_album_gain": (f"{_MP4_PREFIX}REPLAYGAIN_ALBUM_GAIN",),
    "replaygain_track_peak": (f"{_MP4_PREFIX}REPLAYGAIN_TRACK_PEAK",),
    "replaygain_album_peak": (f"{_MP4_PREFIX}REPLAYGAIN_ALBUM_PEAK",),
}


def _mp4_document(tags: object) -> AudioMetadataDocument:
    getter = lambda key: tags.get(key) if tags is not None else None
    values = _mapped_values(getter, _MP4_KEYS)
    track = getter("trkn")
    disc = getter("disk")
    if track:
        values["track_number"], values["total_tracks"] = track[0]
    if disc:
        values["disc_number"], values["total_discs"] = disc[0]
    values["movement_number"] = _integer(getter("©mvi"))
    values["movement_count"] = _integer(getter("©mvc"))
    compilation = getter("cpil")
    values["compilation"] = (
        bool(
            compilation[0]
            if isinstance(compilation, list) and compilation
            else compilation
        )
        if compilation is not None
        else None
    )
    values["genre"] = _values(getter("©gen"))
    values["replaygain_track_gain"] = _float(
        getter(f"{_MP4_PREFIX}REPLAYGAIN_TRACK_GAIN")
    )
    values["replaygain_album_gain"] = _float(
        getter(f"{_MP4_PREFIX}REPLAYGAIN_ALBUM_GAIN")
    )
    values["replaygain_track_peak"] = _float(
        getter(f"{_MP4_PREFIX}REPLAYGAIN_TRACK_PEAK"), positive=True
    )
    values["replaygain_album_peak"] = _float(
        getter(f"{_MP4_PREFIX}REPLAYGAIN_ALBUM_PEAK"), positive=True
    )
    return _field_document(
        values,
        artist_display=_first(getter("©ART")),
        album_artist_display=_first(getter("aART")),
    )


def _mp4_artwork(tags: object) -> tuple[EmbeddedArtworkDescriptor, ...]:
    covers = tags.get("covr", ()) if tags is not None else ()
    return tuple(
        _artwork_descriptor(
            bytes(cover),
            image_type="front",
            declared_mime=None,
            description="",
        )
        for cover in covers
    )


_APE_KEYS: dict[str, tuple[str, ...]] = {
    **_VORBIS_KEYS,
    "album": ("Album", "ALBUM"),
    "title": ("Title", "TITLE"),
    "album_sort": ("ALBUMSORT",),
    "title_sort": ("TITLESORT",),
    "artist": ("Artists", "ARTISTS", "Artist", "ARTIST"),
    "album_artist": ("Album Artist", "ALBUMARTIST"),
    "release_status": ("MUSICBRAINZ_ALBUMSTATUS", "RELEASESTATUS"),
    "release_type": ("MUSICBRAINZ_ALBUMTYPE", "RELEASETYPE"),
    "media": ("Media", "MEDIA"),
    "catalog_number": ("CatalogNumber", "CATALOGNUMBER"),
    "remixer": ("MixArtist", "REMIXER"),
    "lyrics_plain": ("Lyrics", "LYRICS"),
}


def _ape_document(tags: APEv2) -> AudioMetadataDocument:
    getter = lambda key: tags.get(key)
    values = _mapped_values(getter, _APE_KEYS)
    track, total_tracks = _pair(getter("Track") or getter("TRACKNUMBER"))
    disc, total_discs = _pair(getter("Disc") or getter("DISCNUMBER"))
    values.update(
        track_number=track,
        total_tracks=total_tracks or _integer(getter("TOTALTRACKS")),
        disc_number=disc,
        total_discs=total_discs or _integer(getter("TOTALDISCS")),
        movement_number=_integer(getter("MOVEMENT")),
        movement_count=_integer(getter("MOVEMENTTOTAL")),
        compilation=_bool(getter("Compilation") or getter("COMPILATION")),
        genre=_values(getter("Genre") or getter("GENRE")),
        replaygain_track_gain=_float(getter("REPLAYGAIN_TRACK_GAIN")),
        replaygain_album_gain=_float(getter("REPLAYGAIN_ALBUM_GAIN")),
        replaygain_track_peak=_float(getter("REPLAYGAIN_TRACK_PEAK"), positive=True),
        replaygain_album_peak=_float(getter("REPLAYGAIN_ALBUM_PEAK"), positive=True),
    )
    return _field_document(
        values,
        artist_display=_first(getter("Artist") or getter("ARTIST")),
        album_artist_display=_first(getter("Album Artist") or getter("ALBUMARTIST")),
    )


def _ape_artwork(tags: APEv2) -> tuple[EmbeddedArtworkDescriptor, ...]:
    results: list[EmbeddedArtworkDescriptor] = []
    type_names: dict[str, ArtworkImageType] = {
        "cover art (front)": "front",
        "cover art (back)": "back",
        "cover art (media)": "medium",
    }
    for key in tags.keys():
        folded = str(key).casefold()
        if not folded.startswith("cover art ("):
            continue
        value = tags[key]
        if not isinstance(value, APEBinaryValue):
            continue
        payload = bytes(value)
        separator = payload.find(b"\x00")
        if separator < 0:
            continue
        description = payload[:separator].decode("utf-8", "replace")
        results.append(
            _artwork_descriptor(
                payload[separator + 1 :],
                image_type=type_names.get(folded, "other"),
                declared_mime=None,
                description=description,
            )
        )
    return tuple(results)


_ASF_KEYS: dict[str, tuple[str, ...]] = {
    "album": ("WM/AlbumTitle",),
    "title": ("Title",),
    "album_sort": ("WM/AlbumSortOrder",),
    "title_sort": ("WM/TitleSortOrder",),
    "artist": ("WM/ARTISTS", "Author"),
    "album_artist": ("WM/AlbumArtist",),
    "artist_sort": ("WM/ArtistSortOrder",),
    "album_artist_sort": ("WM/AlbumArtistSortOrder",),
    "disc_subtitle": ("WM/SetSubTitle",),
    "date": ("WM/Year",),
    "original_date": ("WM/OriginalReleaseYear",),
    "release_status": ("MusicBrainz/Album Status",),
    "release_country": ("MusicBrainz/Album Release Country",),
    "release_type": ("MusicBrainz/Album Type",),
    "media": ("WM/Media",),
    "label": ("WM/Publisher",),
    "catalog_number": ("WM/CatalogNo",),
    "barcode": ("WM/Barcode",),
    "asin": ("ASIN", "MusicBrainz/ASIN"),
    "isrc": ("WM/ISRC",),
    "musicbrainz_recording_id": ("MusicBrainz/Track Id",),
    "musicbrainz_release_track_id": ("MusicBrainz/Release Track Id",),
    "musicbrainz_release_id": ("MusicBrainz/Album Id",),
    "musicbrainz_release_group_id": ("MusicBrainz/Release Group Id",),
    "musicbrainz_artist_id": ("MusicBrainz/Artist Id",),
    "musicbrainz_album_artist_id": ("MusicBrainz/Album Artist Id",),
    "work": ("WM/Work",),
    "musicbrainz_work_id": ("MusicBrainz/Work Id",),
    "composer": ("WM/Composer",),
    "lyricist": ("WM/Writer",),
    "conductor": ("WM/Conductor",),
    "arranger": ("WM/Arranger",),
    "remixer": ("WM/ModifiedBy",),
    "producer": ("WM/Producer",),
    "acoustid_id": ("Acoustid/Id",),
    "acoustid_fingerprint": ("Acoustid/Fingerprint",),
    "lyrics_plain": ("WM/Lyrics",),
    "lyrics_synced": ("WM/Lyrics_Synchronised",),
    "replaygain_track_gain": ("REPLAYGAIN_TRACK_GAIN",),
    "replaygain_album_gain": ("REPLAYGAIN_ALBUM_GAIN",),
    "replaygain_track_peak": ("REPLAYGAIN_TRACK_PEAK",),
    "replaygain_album_peak": ("REPLAYGAIN_ALBUM_PEAK",),
}


def _asf_document(tags: object) -> AudioMetadataDocument:
    getter = lambda key: tags.get(key) if tags is not None else None
    values = _mapped_values(getter, _ASF_KEYS)
    track, track_total = _pair(getter("WM/TrackNumber"))
    disc, disc_total = _pair(getter("WM/PartOfSet"))
    values.update(
        track_number=track,
        total_tracks=track_total or _integer(getter("WM/TrackTotal")),
        disc_number=disc,
        total_discs=disc_total,
        compilation=_bool(getter("WM/IsCompilation")),
        genre=_values(getter("WM/Genre")),
        replaygain_track_gain=_float(getter("REPLAYGAIN_TRACK_GAIN")),
        replaygain_album_gain=_float(getter("REPLAYGAIN_ALBUM_GAIN")),
        replaygain_track_peak=_float(getter("REPLAYGAIN_TRACK_PEAK"), positive=True),
        replaygain_album_peak=_float(getter("REPLAYGAIN_ALBUM_PEAK"), positive=True),
    )
    return _field_document(
        values,
        artist_display=_first(getter("Author")),
        album_artist_display=_first(getter("WM/AlbumArtist")),
    )


def _riff_document(values: Mapping[str, object]) -> AudioMetadataDocument:
    mapped: dict[str, object] = {
        "title": _first(values.get("INAM")),
        "album": _first(values.get("IPRD")),
        "artist": _values(values.get("IART")),
        "track_number": _integer(values.get("ITRK")),
        "date": _first(values.get("ICRD")),
        "genre": _values(values.get("IGNR")),
        "media": _first(values.get("IMED")),
        "composer": _values(values.get("IMUS")),
        "producer": _values(values.get("IPRO")),
    }
    return _field_document(
        mapped,
        artist_display=_first(values.get("IART")),
        album_artist_display=None,
    )


def _overlay_metadata(
    base: AudioMetadataDocument, overlay: AudioMetadataDocument
) -> AudioMetadataDocument:
    values = {field.name: field.value for field in base.fields}
    values.update({field.name: field.value for field in overlay.fields})
    return AudioMetadataDocument(
        fields=tuple(
            AudioSemanticField(name=name, value=value) for name, value in values.items()
        ),
        artist_display=overlay.artist_display or base.artist_display,
        album_artist_display=(
            overlay.album_artist_display or base.album_artist_display
        ),
    )


def _read_utf16z(data: bytes, offset: int) -> tuple[str, int]:
    end = offset
    while end + 1 < len(data) and data[end : end + 2] != b"\x00\x00":
        end += 2
    if end + 1 >= len(data):
        raise ValueError("unterminated UTF-16 value")
    return data[offset:end].decode("utf-16-le", "replace"), end + 2


def _asf_artwork(tags: object) -> tuple[EmbeddedArtworkDescriptor, ...]:
    results: list[EmbeddedArtworkDescriptor] = []
    if tags is None:
        return ()
    for value in tags.get("WM/Picture", ()):
        payload = bytes(getattr(value, "value", value))
        try:
            if len(payload) < 5:
                raise ValueError("short ASF picture")
            image_type = payload[0]
            size = struct.unpack_from("<I", payload, 1)[0]
            mime, offset = _read_utf16z(payload, 5)
            description, offset = _read_utf16z(payload, offset)
            data = payload[offset : offset + size]
            if len(data) != size:
                raise ValueError("truncated ASF picture")
        except (ValueError, struct.error):
            continue
        results.append(
            _artwork_descriptor(
                data,
                image_type=_picture_type(image_type),
                declared_mime=mime or None,
                description=description,
            )
        )
    return tuple(results)


class _MappedAdapter:
    def __init__(
        self,
        capabilities: FormatCapabilities,
        document_reader: Callable[[object], AudioMetadataDocument],
        artwork_reader: Callable[[object], tuple[EmbeddedArtworkDescriptor, ...]],
        tags_reader: Callable[[Path, object], object],
    ) -> None:
        self.capabilities = capabilities
        self._document_reader = document_reader
        self._artwork_reader = artwork_reader
        self._tags_reader = tags_reader

    def read(
        self,
        path: Path,
        audio: object,
        probe: FormatProbe,
        file_attributes: FileAttributeSnapshot,
    ) -> ReadAudioDocument:
        tags = self._tags_reader(path, audio)
        artwork_source = audio if probe.detected_format == "flac" else tags
        artwork = self._artwork_reader(artwork_source)
        binary_keys = (
            {
                "covr",
                "wm/picture",
                "metadata_block_picture",
                *(
                    str(key).casefold()
                    for key in tags.keys()
                    if str(key).casefold().startswith("cover art (")
                ),
            }
            if tags is not None and hasattr(tags, "keys")
            else set()
        )
        warnings_out = tuple(
            "embedded artwork could not be validated"
            for image in artwork
            if not image.format_supported
        )
        metadata = self._document_reader(tags)
        raw_tags = _raw_tags(tags, binary_keys)
        if probe.detected_format == "wav":
            riff_values = read_riff_info(path)
            if riff_values:
                metadata = _overlay_metadata(metadata, _riff_document(riff_values))
                raw_tags = (
                    *raw_tags,
                    *(
                        RawTagDescriptor(
                            key=f"RIFF_INFO:{key}",
                            values=tuple(values),
                            value_kind="text",
                        )
                        for key, values in sorted(riff_values.items())
                    ),
                )
        return ReadAudioDocument(
            probe=probe,
            metadata=metadata,
            artwork=artwork,
            technical=_technical(path, audio),
            raw_tags=raw_tags,
            native_tags=_native_metadata_snapshot(path, probe.detected_format, tags),
            file_attributes=file_attributes,
            warnings=warnings_out,
        )


def _native_tags(_path: Path, audio: object) -> object:
    return getattr(audio, "tags", None)


def _aac_tags(path: Path, _audio: object) -> object:
    try:
        return APEv2(path)
    except APEError:
        return APEv2()


_ADAPTERS: dict[AudioContainer, _MappedAdapter] = {
    "flac": _MappedAdapter(
        FORMAT_CAPABILITIES["flac"], _vorbis_document, _flac_artwork, _native_tags
    ),
    "mp3": _MappedAdapter(
        FORMAT_CAPABILITIES["mp3"], _id3_document, _id3_artwork, _native_tags
    ),
    "ogg": _MappedAdapter(
        FORMAT_CAPABILITIES["ogg"], _vorbis_document, _ogg_artwork, _native_tags
    ),
    "opus": _MappedAdapter(
        FORMAT_CAPABILITIES["opus"], _vorbis_document, _ogg_artwork, _native_tags
    ),
    "m4a": _MappedAdapter(
        FORMAT_CAPABILITIES["m4a"], _mp4_document, _mp4_artwork, _native_tags
    ),
    "aac": _MappedAdapter(
        FORMAT_CAPABILITIES["aac"], _ape_document, _ape_artwork, _aac_tags
    ),
    "wav": _MappedAdapter(
        FORMAT_CAPABILITIES["wav"], _id3_document, _id3_artwork, _native_tags
    ),
    "wma": _MappedAdapter(
        FORMAT_CAPABILITIES["wma"], _asf_document, _asf_artwork, _native_tags
    ),
}

_OPENERS: dict[AudioContainer, Callable[[Path], object]] = {
    "flac": FLAC,
    "mp3": MP3,
    "ogg": OggVorbis,
    "opus": OggOpus,
    "m4a": MP4,
    "aac": AAC,
    "wav": WAVE,
    "wma": ASF,
}


def _synchsafe_size(header: bytes) -> int | None:
    if len(header) < 10 or not header.startswith(b"ID3"):
        return None
    values = header[6:10]
    if any(value & 0x80 for value in values):
        return None
    return 10 + sum(value << shift for value, shift in zip(values, (21, 14, 7, 0)))


def _detect_format(path: Path) -> AudioContainer | None:
    with path.open("rb") as handle:
        header = handle.read(256)
        id3_size = _synchsafe_size(header)
        after_id3 = b""
        if id3_size is not None:
            handle.seek(id3_size)
            after_id3 = handle.read(16)
    if header.startswith(b"fLaC") or after_id3.startswith(b"fLaC"):
        return "flac"
    if header.startswith(b"RIFF") and header[8:12] == b"WAVE":
        return "wav"
    if header.startswith(bytes.fromhex("3026b2758e66cf11a6d900aa0062ce6c")):
        return "wma"
    if header.startswith(b"OggS"):
        return "opus" if b"OpusHead" in header else "ogg"
    if len(header) >= 12 and header[4:8] == b"ftyp":
        return "m4a"
    if header.startswith(b"ADIF") or (
        len(header) >= 2 and header[0] == 0xFF and header[1] & 0xF6 == 0xF0
    ):
        return "aac"
    if id3_size is not None or (
        len(header) >= 2 and header[0] == 0xFF and header[1] & 0xE0 == 0xE0
    ):
        return "mp3"
    return None


def _tag_details(audio_format: AudioContainer, path: Path) -> tuple[str, str | None]:
    capabilities = FORMAT_CAPABILITIES[audio_format]
    if audio_format in {"mp3", "wav"}:
        try:
            tags = ID3(path) if audio_format == "mp3" else WAVE(path).tags
            version = getattr(tags, "version", None)
            version_text = (
                ".".join(str(value) for value in version) if version else None
            )
            if audio_format == "wav" and read_riff_info(path):
                return (
                    "RIFF INFO + ID3v2" if version_text else "RIFF INFO",
                    version_text,
                )
            return capabilities.tag_format, version_text
        except MutagenError:
            return capabilities.tag_format, None
    if audio_format == "aac":
        try:
            tags = APEv2(path)
            version = getattr(tags, "version", None)
            return capabilities.tag_format, str(version) if version else None
        except APEError:
            return capabilities.tag_format, None
    return capabilities.tag_format, None


def _snapshot_from_document(document: ReadAudioDocument) -> SemanticTagSnapshot:
    return SemanticTagSnapshot(
        snapshot_version=_SNAPSHOT_VERSION,
        adapter_version=_ADAPTER_VERSION,
        probe=document.probe,
        metadata=document.metadata,
        artwork=document.artwork,
        technical=document.technical,
        raw_tags=document.raw_tags,
        native_tags=document.native_tags,
        file_attributes=document.file_attributes,
    )


def _merge_field_values(before: object, desired: object) -> tuple[str, ...]:
    result: list[str] = []
    seen: set[str] = set()
    for values in (desired, before):
        if not isinstance(values, tuple):
            continue
        for value in values:
            folded = value.casefold()
            if folded not in seen:
                seen.add(folded)
                result.append(value)
    return tuple(result)


def _clear_field_value(name: str) -> object:
    return () if name in _ORDERED_FIELDS else None


def _validate_desired_value(name: str, value: object) -> None:
    if name in _ORDERED_FIELDS:
        if not isinstance(value, tuple) or any(
            not isinstance(item, str) for item in value
        ):
            raise ValueError(f"{name} requires an ordered tuple of text values")
        return
    if name in {
        "track_number",
        "total_tracks",
        "disc_number",
        "total_discs",
        "movement_number",
        "movement_count",
    }:
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            raise ValueError(f"{name} requires a non-negative integer")
        return
    if name == "compilation":
        if not isinstance(value, bool):
            raise ValueError("compilation requires a boolean")
        return
    if name in REPLAYGAIN_FIELDS:
        if (
            isinstance(value, bool)
            or not isinstance(value, (float, int))
            or not math.isfinite(float(value))
            or (name.endswith("_peak") and float(value) < 0)
        ):
            raise ValueError(f"{name} requires a finite numeric value")
        return
    if not isinstance(value, str):
        raise ValueError(f"{name} requires text")
    if name in LYRICS_FIELDS and ("\x00" in value or len(value) > 1_000_000):
        raise ValueError(f"{name} requires bounded text")
    if name == "lyrics_synced" and not parse_lrc(value):
        raise ValueError("lyrics_synced requires timestamped LRC text")
    if name in {"date", "original_date"} and not re.fullmatch(
        r"\d{4}(?:-\d{2}(?:-\d{2})?)?", value
    ):
        raise ValueError(f"{name} requires a year, month, or day precision date")


def _is_artwork_raw_key(key: str) -> bool:
    folded = key.casefold()
    return (
        folded == "covr"
        or folded == "wm/picture"
        or folded == "metadata_block_picture"
        or folded.startswith("apic:")
        or folded.startswith("cover art (")
    )


def _has_opaque_native_value(snapshot: NativeMetadataSnapshot) -> bool:
    return any(
        value.kind == "opaque"
        for entry in (*snapshot.entries, *snapshot.auxiliary_entries)
        for value in entry.values
    )


def _preserved_native_keys(
    audio_format: AudioContainer,
    preserved_names: set[str],
) -> set[str]:
    mappings: list[Mapping[str, tuple[str, ...]]] = []
    extras: dict[str, tuple[str, ...]] = {}
    if audio_format in {"flac", "ogg", "opus"}:
        mappings.append(_VORBIS_KEYS)
        extras = {
            "track_number": ("TRACKNUMBER",),
            "total_tracks": ("TOTALTRACKS", "TRACKTOTAL"),
            "disc_number": ("DISCNUMBER",),
            "total_discs": ("TOTALDISCS", "DISCTOTAL"),
            "movement_number": ("MOVEMENT",),
            "movement_count": ("MOVEMENTTOTAL",),
            "compilation": ("COMPILATION",),
            "genre": ("GENRE",),
        }
    elif audio_format in {"mp3", "wav"}:
        extras = {
            "album": ("TALB",),
            "title": ("TIT2",),
            "album_sort": ("TSOA",),
            "title_sort": ("TSOT",),
            "artist": ("TPE1", "TXXX:Artists"),
            "album_artist": ("TPE2",),
            "artist_sort": ("TSOP",),
            "album_artist_sort": ("TSO2",),
            "track_number": ("TRCK",),
            "total_tracks": ("TRCK",),
            "disc_number": ("TPOS",),
            "total_discs": ("TPOS",),
            "disc_subtitle": ("TSST",),
            "date": ("TDRC",),
            "original_date": ("TDOR",),
            "media": ("TMED",),
            "compilation": ("TCMP",),
            "isrc": ("TSRC",),
            "musicbrainz_recording_id": (f"UFID:{_MB_UFID_OWNER}",),
            "movement": ("MVNM",),
            "movement_number": ("MVIN",),
            "movement_count": ("MVIN",),
            "composer": ("TCOM",),
            "lyricist": ("TEXT",),
            "conductor": ("TPE3",),
            "performer": ("TMCL",),
            "arranger": ("TIPL", "IPLS"),
            "remixer": ("TPE4",),
            "producer": ("TIPL", "IPLS"),
            "genre": ("TCON",),
            "lyrics_plain": ("USLT",),
            "lyrics_synced": ("SYLT",),
            "replaygain_track_gain": ("TXXX:REPLAYGAIN_TRACK_GAIN",),
            "replaygain_album_gain": ("TXXX:REPLAYGAIN_ALBUM_GAIN",),
            "replaygain_track_peak": ("TXXX:REPLAYGAIN_TRACK_PEAK",),
            "replaygain_album_peak": ("TXXX:REPLAYGAIN_ALBUM_PEAK",),
            **{
                field: (f"TXXX:{description}",)
                for field, description in _ID3_TXXX.items()
            },
        }
        if audio_format == "wav":
            for field, key in {
                "album": "IPRD",
                "title": "INAM",
                "artist": "IART",
                "track_number": "ITRK",
                "date": "ICRD",
                "genre": "IGNR",
                "media": "IMED",
                "composer": "IMUS",
                "producer": "IPRO",
            }.items():
                extras[field] = (*extras.get(field, ()), f"RIFF_INFO:{key}")
    elif audio_format == "m4a":
        mappings.append(_MP4_KEYS)
        extras = {
            "track_number": ("trkn",),
            "total_tracks": ("trkn",),
            "disc_number": ("disk",),
            "total_discs": ("disk",),
            "movement_number": ("©mvi",),
            "movement_count": ("©mvc",),
            "compilation": ("cpil",),
            "genre": ("©gen",),
        }
    elif audio_format == "aac":
        mappings.append(_APE_KEYS)
        extras = {
            "track_number": ("Track", "TRACKNUMBER"),
            "total_tracks": ("Track", "TOTALTRACKS"),
            "disc_number": ("Disc", "DISCNUMBER"),
            "total_discs": ("Disc", "TOTALDISCS"),
            "movement_number": ("MOVEMENT",),
            "movement_count": ("MOVEMENTTOTAL",),
            "compilation": ("Compilation", "COMPILATION"),
            "genre": ("Genre", "GENRE"),
        }
    else:
        mappings.append(_ASF_KEYS)
        extras = {
            "track_number": ("WM/TrackNumber",),
            "total_tracks": ("WM/TrackNumber", "WM/TrackTotal"),
            "disc_number": ("WM/PartOfSet",),
            "total_discs": ("WM/PartOfSet",),
            "compilation": ("WM/IsCompilation",),
            "genre": ("WM/Genre",),
        }

    native_keys: set[str] = set()
    for mapping in mappings:
        for field, keys in mapping.items():
            if field.casefold() in preserved_names:
                native_keys.update(key.casefold() for key in keys)
    for field, keys in extras.items():
        if field.casefold() in preserved_names:
            native_keys.update(key.casefold() for key in keys)
    return native_keys


def _raw_key_is_preserved(
    raw_key: str,
    preserved_names: set[str],
    preserved_native_keys: set[str],
) -> bool:
    folded = raw_key.casefold()
    aliases = {folded}
    for prefix in ("txxx:", _MP4_PREFIX.casefold(), "riff_info:"):
        if folded.startswith(prefix):
            aliases.add(folded.removeprefix(prefix))
    id3_qualified_base = folded.split(":", 1)[0]
    return (
        bool(aliases & preserved_names)
        or folded in preserved_native_keys
        or (
            id3_qualified_base in {"uslt", "sylt"}
            and id3_qualified_base in preserved_native_keys
        )
    )


def _custom_tag_native_key(
    audio_format: AudioContainer,
    name: str,
    wav_tag_policy: Literal["id3", "riff_info", "preserve_existing"],
) -> str:
    if audio_format in {"flac", "ogg", "opus", "aac", "wma"}:
        return name
    if audio_format in {"mp3", "wav"} and not (
        audio_format == "wav" and wav_tag_policy == "riff_info"
    ):
        return f"TXXX:{name}"
    if audio_format == "m4a":
        return f"{_MP4_PREFIX}{name}"
    return f"RIFF_INFO:{name}"


def _custom_tag_before(
    current: ReadAudioDocument,
    native_key: str,
) -> tuple[tuple[str, ...], bool]:
    values: list[str] = []
    binary = False
    for raw in current.raw_tags:
        if raw.key.casefold() != native_key.casefold():
            continue
        binary = binary or raw.value_kind != "text"
        for value in raw.values:
            if value not in values:
                values.append(value)
    return tuple(values), binary


def _custom_tag_reserved_keys(
    audio_format: AudioContainer,
    wav_tag_policy: Literal["id3", "riff_info", "preserve_existing"],
) -> set[str]:
    fields = set(AUDIO_MANAGED_FIELD_NAMES)
    if audio_format == "wav" and wav_tag_policy == "riff_info":
        keys = {
            key
            for key in _preserved_native_keys("wav", fields)
            if key.startswith("riff_info:")
        }
    else:
        keys = _preserved_native_keys(
            "mp3" if audio_format == "wav" else audio_format,
            fields,
        )
    artwork_keys = {
        "metadata_block_picture",
        "covr",
        "wm/picture",
        "apic",
    }
    return {*keys, *artwork_keys}


def _resolved_wav_policy(
    current: ReadAudioDocument,
    policy: AudioWritePolicy,
) -> Literal["id3", "riff_info", "preserve_existing"]:
    wav_tag_policy = policy.wav_tag_policy
    if current.probe.detected_format == "wav" and wav_tag_policy == "preserve_existing":
        if current.probe.tag_version is not None:
            return "id3"
        if current.probe.tag_format == "RIFF INFO":
            return "riff_info"
    return wav_tag_policy


def _write_compatibility(
    audio_format: AudioContainer,
    policy: AudioWritePolicy,
    *,
    wav_tag_policy: Literal["id3", "riff_info", "preserve_existing"],
) -> AudioWriteCompatibility:
    uses_id3 = audio_format == "mp3" or (
        audio_format == "wav" and wav_tag_policy != "riff_info"
    )
    return AudioWriteCompatibility(
        id3_version=policy.id3_version if uses_id3 else None,
        id3_text_encoding=policy.id3_text_encoding if uses_id3 else None,
        id3v23_join_delimiter=(
            policy.id3v23_join_delimiter
            if uses_id3 and policy.id3_version == "2.3"
            else None
        ),
        wav_tag_policy=wav_tag_policy if audio_format == "wav" else None,
        raw_aac_tag_policy=(
            policy.raw_aac_tag_policy if audio_format == "aac" else None
        ),
    )


class AudioMetadataEngine:
    def probe(self, path: Path) -> FormatProbe:
        extension = path.suffix.casefold()
        expected = AUDIO_EXTENSION_FORMATS.get(extension)
        detected = _detect_format(path)
        tag_format: str | None = None
        tag_version: str | None = None
        if detected is not None:
            tag_format, tag_version = _tag_details(detected, path)
        return FormatProbe(
            extension=extension,
            admitted=expected is not None,
            detected_format=detected,
            detected_class=(
                _OPENERS[detected].__name__ if detected is not None else None
            ),
            extension_matches=expected is not None and expected == detected,
            tag_format=tag_format,
            tag_version=tag_version,
        )

    def capabilities(self, path: Path) -> CapabilityReport:
        probe = self.probe(path)
        blockers: list[str] = []
        if not probe.admitted:
            blockers.append("file extension is not admitted")
        if probe.detected_format is None:
            blockers.append("audio container could not be detected")
        elif not probe.extension_matches:
            blockers.append("file extension does not match the detected container")
        capabilities = (
            FORMAT_CAPABILITIES.get(probe.detected_format)
            if probe.detected_format is not None
            else None
        )
        return CapabilityReport(
            probe=probe,
            capabilities=capabilities,
            blockers=tuple(blockers),
            warnings=capabilities.representation_warnings if capabilities else (),
        )

    def read(self, path: Path) -> ReadAudioDocument:
        try:
            file_attributes = _file_attributes(path)
            probe = self.probe(path)
        except OSError as error:
            raise UnsupportedAudioFormatError(
                "The audio file could not be read."
            ) from error
        if not probe.admitted or probe.detected_format is None:
            raise UnsupportedAudioFormatError(
                "The file does not have an admitted, detectable audio format."
            )
        if not probe.extension_matches:
            raise AudioFormatMismatchError(
                "The file extension does not match its detected audio container."
            )
        try:
            audio = _OPENERS[probe.detected_format](path)
        except (MutagenError, OSError, ValueError) as error:
            raise UnsupportedAudioFormatError(
                "The audio file could not be read."
            ) from error
        return _ADAPTERS[probe.detected_format].read(
            path, audio, probe, file_attributes
        )

    def snapshot(self, path: Path) -> SemanticTagSnapshot:
        return _snapshot_from_document(self.read(path))

    def custom_tags(
        self,
        current: ReadAudioDocument,
        policy: AudioWritePolicy,
    ) -> tuple[CustomTagValue, ...]:
        audio_format = current.probe.detected_format
        if audio_format is None:
            return ()
        reserved = _custom_tag_reserved_keys(
            audio_format,
            _resolved_wav_policy(current, policy),
        )
        values: dict[str, list[str]] = {}
        names: dict[str, str] = {}
        for raw in current.raw_tags:
            if raw.value_kind != "text" or raw.key.casefold() in reserved:
                continue
            name = raw.key
            for prefix in ("TXXX:", _MP4_PREFIX, "RIFF_INFO:"):
                if name.casefold().startswith(prefix.casefold()):
                    name = name[len(prefix) :]
                    break
            if not name:
                continue
            folded = name.casefold()
            names.setdefault(folded, name)
            output = values.setdefault(folded, [])
            for value in raw.values:
                if value not in output:
                    output.append(value)
        return tuple(
            CustomTagValue(name=names[folded], values=tuple(values[folded]))
            for folded in sorted(values)
        )

    def plan(
        self,
        current: ReadAudioDocument,
        desired: DesiredAudioDocument,
        policy: AudioWritePolicy,
    ) -> AudioWritePlan:
        audio_format = current.probe.detected_format
        if audio_format is None or not current.probe.extension_matches:
            raise AudioFormatMismatchError(
                "A write plan requires a matching admitted audio container."
            )
        capabilities = FORMAT_CAPABILITIES[audio_format]
        wav_tag_policy = _resolved_wav_policy(current, policy)
        desired_names: set[str] = set()
        preserved_names = {value.casefold() for value in policy.preserve_fields}
        mutations: list[AudioFieldMutation] = []
        blockers: list[str] = []
        warnings_out = list(capabilities.representation_warnings)

        for requested in desired.fields:
            if requested.name in desired_names:
                raise ValueError(f"{requested.name} is planned more than once")
            if requested.name not in AUDIO_MANAGED_FIELD_NAMES:
                raise ValueError(f"{requested.name} is not a writable semantic field")
            desired_names.add(requested.name)
            before = current.metadata.value_for(requested.name)
            operation = requested.action
            after = before
            if requested.name.casefold() in preserved_names:
                operation = "preserve"
            elif operation == "set":
                if requested.value is None:
                    raise ValueError(f"{requested.name} set requires a value")
                _validate_desired_value(requested.name, requested.value)
                after = requested.value
            elif operation == "clear":
                after = _clear_field_value(requested.name)
            elif operation == "merge":
                if requested.name not in _ORDERED_FIELDS:
                    raise ValueError(f"{requested.name} does not support merge")
                _validate_desired_value(requested.name, requested.value)
                after = _merge_field_values(before, requested.value)
            elif operation not in {"unchanged", "preserve"}:
                raise ValueError(f"Unknown write action: {operation}")

            if operation not in {"unchanged", "preserve"} and after == before:
                operation = "unchanged"
            representation_loss: str | None = None
            explicitly_allowed_loss = False
            changes_value = operation not in {"unchanged", "preserve"}
            constrained_genre = (
                requested.name == "genre"
                and isinstance(after, tuple)
                and len(after) > 1
                and policy.constrained_genres_primary_only
                and (
                    (
                        audio_format in {"mp3", "wav"}
                        and policy.id3_version == "2.3"
                        and not (
                            audio_format == "wav" and wav_tag_policy == "riff_info"
                        )
                    )
                    or (audio_format == "wav" and wav_tag_policy == "riff_info")
                )
            )
            if changes_value and constrained_genre:
                after = after[:1]
                representation_loss = (
                    "genre was explicitly limited to its primary value for the "
                    "selected constrained representation"
                )
                explicitly_allowed_loss = True
            if (
                changes_value
                and requested.name != "genre"
                and requested.name not in capabilities.supported_fields
            ):
                blockers.append(
                    f"{requested.name} is not supported by the {audio_format} adapter"
                )
            if (
                changes_value
                and audio_format in {"mp3", "wav"}
                and policy.id3_version == "2.3"
                and not (audio_format == "wav" and wav_tag_policy == "riff_info")
            ):
                if requested.name in _ID3V23_UNSUPPORTED:
                    blockers.append(
                        f"{requested.name} has no Picard ID3v2.3 representation"
                    )
                if (
                    representation_loss is None
                    and isinstance(after, tuple)
                    and len(after) > 1
                ):
                    representation_loss = (
                        f"{requested.name} values will be joined with "
                        f"{policy.id3v23_join_delimiter!r} for ID3v2.3"
                    )
                    after = (policy.id3v23_join_delimiter.join(after),)
            if (
                changes_value
                and audio_format == "wav"
                and wav_tag_policy == "riff_info"
                and requested.name not in _RIFF_INFO_FIELDS
            ):
                blockers.append(
                    f"{requested.name} has no verified RIFF INFO representation"
                )
            if (
                changes_value
                and audio_format == "wav"
                and wav_tag_policy == "riff_info"
                and isinstance(after, tuple)
                and len(after) > 1
            ):
                if representation_loss is None:
                    representation_loss = (
                        f"{requested.name} has only a scalar RIFF INFO representation"
                    )
                    after = (policy.id3v23_join_delimiter.join(after),)
            if representation_loss:
                if policy.strict_capability_gate and not explicitly_allowed_loss:
                    blockers.append(representation_loss)
                else:
                    warnings_out.append(representation_loss)
            mutations.append(
                AudioFieldMutation(
                    name=requested.name,
                    operation=operation,
                    before=before,
                    after=after,
                    representation_loss=representation_loss,
                )
            )

        custom_mutations: list[AudioCustomTagMutation] = []
        custom_names: set[str] = set()
        custom_output_characters = 0
        reserved_custom_keys = _custom_tag_reserved_keys(audio_format, wav_tag_policy)
        if len(desired.custom_tags) > 64:
            blockers.append("the tagging scripts produced too many custom tags")
        for requested in desired.custom_tags:
            folded_name = requested.name.casefold()
            if folded_name in custom_names:
                raise ValueError(
                    f"custom tag {requested.name} is planned more than once"
                )
            custom_names.add(folded_name)
            if (
                not requested.name
                or "\x00" in requested.name
                or len(requested.name) > 255
                or len(requested.values) > 100
                or any(
                    "\x00" in value or len(value) > 8_192 for value in requested.values
                )
            ):
                raise ValueError(f"custom tag {requested.name!r} exceeds safety bounds")
            custom_output_characters += sum(len(value) for value in requested.values)
            native_key = _custom_tag_native_key(
                audio_format, requested.name, wav_tag_policy
            )
            before, binary = _custom_tag_before(current, native_key)
            if binary:
                blockers.append(
                    f"custom tag {requested.name} conflicts with binary metadata"
                )
            if native_key.casefold() in reserved_custom_keys or (
                audio_format == "aac"
                and requested.name.casefold().startswith("cover art (")
            ):
                blockers.append(
                    f"custom tag {requested.name} conflicts with a managed native tag"
                )
            if (
                audio_format == "wav"
                and wav_tag_policy == "riff_info"
                and (len(requested.name) != 4 or not requested.name.isascii())
            ):
                blockers.append(
                    f"custom tag {requested.name} is not a four-character RIFF INFO key"
                )
            if requested.action == "set":
                after = tuple(dict.fromkeys(requested.values))
            elif requested.action == "append":
                after = tuple(dict.fromkeys((*before, *requested.values)))
            elif requested.action == "delete":
                if requested.values:
                    raise ValueError("custom tag delete cannot carry values")
                after = ()
            else:
                raise ValueError(f"unknown custom tag action: {requested.action}")
            operation = requested.action
            if folded_name in preserved_names or after == before:
                operation = "preserve"
                after = before
            if (
                operation != "preserve"
                and len(after) > 1
                and (
                    (
                        audio_format in {"mp3", "wav"}
                        and wav_tag_policy != "riff_info"
                        and policy.id3_version == "2.3"
                    )
                    or (audio_format == "wav" and wav_tag_policy == "riff_info")
                )
            ):
                representation_loss = (
                    f"custom tag {requested.name} values require scalar flattening"
                )
                after = (policy.id3v23_join_delimiter.join(after),)
                if policy.strict_capability_gate:
                    blockers.append(representation_loss)
                else:
                    warnings_out.append(representation_loss)
            custom_mutations.append(
                AudioCustomTagMutation(
                    name=requested.name,
                    operation=operation,
                    native_key=native_key,
                    before=before,
                    after=after,
                )
            )
        if custom_output_characters > 65_536:
            blockers.append("custom tag output exceeds the safety limit")

        preserve_artwork = (
            not policy.scrub_unmanaged_tags or policy.preserve_embedded_art_during_scrub
        )
        changed_semantic_names = {
            mutation.name
            for mutation in mutations
            if mutation.operation not in {"unchanged", "preserve"}
        }
        scrub_preserved_names = {
            *preserved_names,
            *(
                name
                for name in ENRICHMENT_AUDIO_FIELDS
                if name not in changed_semantic_names
            ),
        }
        preserved_native_keys = _preserved_native_keys(
            audio_format, scrub_preserved_names
        )
        preserved_raw: list[str] = []
        scrubbed_raw: list[str] = []
        for raw in current.raw_tags:
            preserve_raw = (
                not policy.scrub_unmanaged_tags
                or _raw_key_is_preserved(
                    raw.key, scrub_preserved_names, preserved_native_keys
                )
                or (preserve_artwork and _is_artwork_raw_key(raw.key))
            )
            (preserved_raw if preserve_raw else scrubbed_raw).append(raw.key)

        if audio_format == "wav":
            changed_names = {
                mutation.name
                for mutation in mutations
                if mutation.operation not in {"unchanged", "preserve"}
            }
            id3_keys = _preserved_native_keys("mp3", changed_names)
            riff_keys = {
                key
                for key in _preserved_native_keys("wav", changed_names)
                if key.startswith("riff_info:")
            }
            conflicting_keys = id3_keys if wav_tag_policy == "riff_info" else riff_keys
            for raw in current.raw_tags:
                if raw.key.casefold() not in conflicting_keys:
                    continue
                if raw.key in preserved_raw:
                    preserved_raw.remove(raw.key)
                if raw.key not in scrubbed_raw:
                    scrubbed_raw.append(raw.key)
            for mutation in custom_mutations:
                if mutation.operation == "preserve":
                    continue
                other_key = (
                    f"TXXX:{mutation.name}"
                    if wav_tag_policy == "riff_info"
                    else f"RIFF_INFO:{mutation.name}"
                )
                for raw in current.raw_tags:
                    if raw.key.casefold() != other_key.casefold():
                        continue
                    if raw.key in preserved_raw:
                        preserved_raw.remove(raw.key)
                    if raw.key not in scrubbed_raw:
                        scrubbed_raw.append(raw.key)

        cleanup_actions: list[AudioCleanupAction] = []
        native = current.native_tags
        if (
            audio_format == "flac"
            and policy.remove_id3_from_flac
            and native.auxiliary_encoded_id3 is not None
        ):
            cleanup_actions.append("remove_id3_from_flac")
        if (
            audio_format == "mp3"
            and policy.mp3_apev2_policy == "remove"
            and native.auxiliary_entries
        ):
            cleanup_actions.append("remove_apev2_from_mp3")
        if (
            audio_format == "aac"
            and policy.raw_aac_tag_policy == "remove_apev2"
            and native.entries
        ):
            cleanup_actions.append("remove_apev2_from_aac")

        removes_raw_aac_tags = "remove_apev2_from_aac" in cleanup_actions
        if removes_raw_aac_tags:
            preserved_raw = []
            scrubbed_raw = [raw.key for raw in current.raw_tags]

        changes = any(
            mutation.operation not in {"unchanged", "preserve"}
            for mutation in mutations
        ) or any(mutation.operation != "preserve" for mutation in custom_mutations)
        desired_artwork = (
            (
                ()
                if removes_raw_aac_tags
                else current.artwork
                if preserve_artwork
                else ()
            )
            if desired.artwork is None
            else desired.artwork
        )
        artwork_changes = desired_artwork != current.artwork
        requires_write = bool(
            changes or artwork_changes or scrubbed_raw or cleanup_actions
        )
        if audio_format == "aac" and policy.raw_aac_tag_policy == "do_not_write":
            if requires_write:
                blockers.append("the raw AAC profile is configured not to write tags")
        if audio_format == "aac" and policy.raw_aac_tag_policy == "remove_apev2":
            if changes or (desired.artwork is not None and bool(desired.artwork)):
                blockers.append(
                    "raw AAC APEv2 removal cannot also publish metadata or artwork"
                )
        if (
            audio_format in {"mp3", "wav"}
            and policy.id3_version == "2.3"
            and policy.id3_text_encoding == "utf8"
            and not (audio_format == "wav" and wav_tag_policy == "riff_info")
        ):
            blockers.append("ID3v2.3 cannot encode text as UTF-8")
        if (
            audio_format == "wav"
            and policy.wav_tag_policy == "preserve_existing"
            and current.probe.tag_version is None
            and current.probe.tag_format != "RIFF INFO"
            and requires_write
        ):
            blockers.append("the existing WAV tag representation could not be verified")
        if (
            audio_format == "m4a"
            and desired.artwork is not None
            and any(image.image_type != "front" for image in desired_artwork)
        ):
            blockers.append("MP4 covr cannot preserve non-front artwork types")
        if requires_write and _has_opaque_native_value(native):
            blockers.append(
                "the file contains a native tag value without a restorable adapter"
            )

        return AudioWritePlan(
            audio_format=audio_format,
            adapter_name=capabilities.adapter_name,
            mutations=tuple(mutations),
            custom_tag_mutations=tuple(custom_mutations),
            preserved_raw_keys=tuple(preserved_raw),
            scrubbed_raw_keys=tuple(scrubbed_raw),
            preserve_embedded_artwork=preserve_artwork,
            desired_artwork=desired_artwork,
            artist_display=(
                desired.artist_display
                if desired.artist_display is not None
                else current.metadata.artist_display
            ),
            album_artist_display=(
                desired.album_artist_display
                if desired.album_artist_display is not None
                else current.metadata.album_artist_display
            ),
            cleanup_actions=tuple(cleanup_actions),
            compatibility=_write_compatibility(
                audio_format,
                policy,
                wav_tag_policy=wav_tag_policy,
            ),
            preserve_timestamps=policy.preserve_timestamps,
            preserve_permissions=policy.preserve_permissions,
            snapshot=_snapshot_from_document(current),
            blockers=tuple(dict.fromkeys(blockers)),
            warnings=tuple(dict.fromkeys(warnings_out)),
            requires_write=requires_write,
        )

    def apply(self, staged_path: Path, plan: AudioWritePlan) -> AudioWriteResult:
        from infrastructure.audio.metadata_writer import apply_plan

        return apply_plan(self, staged_path, plan)

    def restore(
        self, staged_path: Path, snapshot: SemanticTagSnapshot
    ) -> AudioWriteResult:
        from infrastructure.audio.metadata_writer import restore_snapshot

        return restore_snapshot(self, staged_path, snapshot)


def legacy_audio_projection(
    document: ReadAudioDocument,
) -> tuple[AudioTag, AudioInfo]:
    metadata = document.metadata
    artist_names = metadata.strings_for("artist")
    album_artist_names = metadata.strings_for("album_artist")
    artist_ids = metadata.strings_for("musicbrainz_artist_id")
    album_artist_ids = metadata.strings_for("musicbrainz_album_artist_id")
    genres = metadata.strings_for("genre")

    def scalar(name: str) -> str | None:
        value = metadata.value_for(name)
        return value if isinstance(value, str) else None

    def integer(name: str, default: int) -> int:
        value = metadata.value_for(name)
        return (
            value if isinstance(value, int) and not isinstance(value, bool) else default
        )

    compilation = metadata.value_for("compilation")
    tag = AudioTag(
        title=scalar("title") or "",
        artist=metadata.artist_display or (artist_names[0] if artist_names else ""),
        album=scalar("album") or "",
        album_artist=metadata.album_artist_display,
        track_number=integer("track_number", 0),
        disc_number=integer("disc_number", 1),
        year=(
            int(scalar("date")[:4])
            if scalar("date") and scalar("date")[:4].isdigit()
            else None
        ),
        genre="; ".join(genres) or None,
        musicbrainz_release_group_id=scalar("musicbrainz_release_group_id"),
        musicbrainz_release_id=scalar("musicbrainz_release_id"),
        musicbrainz_recording_id=scalar("musicbrainz_recording_id"),
        musicbrainz_release_track_id=scalar("musicbrainz_release_track_id"),
        musicbrainz_artist_id=artist_ids[0] if artist_ids else None,
        musicbrainz_album_artist_id=(album_artist_ids[0] if album_artist_ids else None),
        acoustid_id=scalar("acoustid_id"),
        compilation=compilation if isinstance(compilation, bool) else False,
        title_sort=scalar("title_sort"),
        artist_sort=(metadata.strings_for("artist_sort") or (None,))[0],
        album_sort=scalar("album_sort"),
        album_artist_sort=(metadata.strings_for("album_artist_sort") or (None,))[0],
        disc_subtitle=scalar("disc_subtitle"),
        original_release_date=scalar("original_date"),
        replaygain_track_gain=(
            metadata.value_for("replaygain_track_gain")
            if isinstance(metadata.value_for("replaygain_track_gain"), float)
            else None
        ),
        replaygain_album_gain=(
            metadata.value_for("replaygain_album_gain")
            if isinstance(metadata.value_for("replaygain_album_gain"), float)
            else None
        ),
        replaygain_track_peak=(
            metadata.value_for("replaygain_track_peak")
            if isinstance(metadata.value_for("replaygain_track_peak"), float)
            else None
        ),
        replaygain_album_peak=(
            metadata.value_for("replaygain_album_peak")
            if isinstance(metadata.value_for("replaygain_album_peak"), float)
            else None
        ),
        genres=list(genres),
        artists=[
            AudioArtistCredit(
                name=name,
                credited_name=name,
                sort_name=(
                    metadata.strings_for("artist_sort")[position]
                    if position < len(metadata.strings_for("artist_sort"))
                    else None
                ),
                musicbrainz_artist_id=(
                    artist_ids[position] if position < len(artist_ids) else None
                ),
            )
            for position, name in enumerate(artist_names)
        ],
        album_artists=[
            AudioArtistCredit(
                name=name,
                credited_name=name,
                sort_name=(
                    metadata.strings_for("album_artist_sort")[position]
                    if position < len(metadata.strings_for("album_artist_sort"))
                    else None
                ),
                musicbrainz_artist_id=(
                    album_artist_ids[position]
                    if position < len(album_artist_ids)
                    else None
                ),
            )
            for position, name in enumerate(album_artist_names)
        ],
        musicbrainz_artist_ids=list(artist_ids),
        musicbrainz_album_artist_ids=list(album_artist_ids),
    )
    technical = document.technical
    info = AudioInfo(
        duration_seconds=technical.duration_seconds,
        bitrate=technical.bitrate_bps // 1000,
        sample_rate=technical.sample_rate_hz,
        channels=technical.channels,
        file_format=document.probe.detected_format or "unknown",
        file_size_bytes=technical.file_size_bytes,
        bit_depth=(
            technical.bit_depth
            if document.probe.detected_format in {"flac", "wav"}
            else None
        ),
    )
    return tag, info
