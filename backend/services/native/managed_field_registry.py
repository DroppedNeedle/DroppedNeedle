"""Data-driven ownership contract for every Library Management tag field."""

from __future__ import annotations

from typing import Literal

import msgspec

from api.v1.schemas.library_management import (
    MANAGED_FIELD_NAMES,
    MERGEABLE_MANAGED_FIELD_NAMES,
)
from models.library_management_canonical import (
    CanonicalReleaseDocument,
    CanonicalTrackDocument,
)

ManagedFieldScope = Literal["album", "track"]
ManagedFieldCardinality = Literal["string", "integer", "boolean", "ordered_strings"]
EmptyCanonicalBehavior = Literal["preserve_unless_explicit_clear"]

ADMITTED_MANAGEMENT_FORMATS: tuple[str, ...] = (
    "flac",
    "mp3",
    "ogg",
    "opus",
    "m4a",
    "aac",
    "wav",
    "wma",
)


class FormatAdapterFieldHook(msgspec.Struct, frozen=True, kw_only=True):
    audio_format: str
    adapter_hook: str
    required_capability: str


class ManagedFieldDefinition(msgspec.Struct, frozen=True, kw_only=True):
    name: str
    canonical_source: str
    source_provider: Literal["musicbrainz", "local_verified"]
    scope: ManagedFieldScope
    cardinality: ManagedFieldCardinality
    order_significant: bool
    required_includes: tuple[str, ...]
    enabled_in_picard_preset: bool
    catalog_projection: str | None
    adapter_hooks: tuple[FormatAdapterFieldHook, ...]
    empty_canonical_behavior: EmptyCanonicalBehavior
    allow_override: bool
    allow_preserve: bool
    participates_in_path_rendering: bool
    merge_supported: bool
    max_unique_values: int = 100


def _hooks(name: str, representation: str) -> tuple[FormatAdapterFieldHook, ...]:
    return tuple(
        FormatAdapterFieldHook(
            audio_format=audio_format,
            adapter_hook=f"{audio_format}.{representation}",
            required_capability=f"field.{name}",
        )
        for audio_format in ADMITTED_MANAGEMENT_FORMATS
    )


def _field(
    name: str,
    source: str,
    *,
    scope: ManagedFieldScope,
    cardinality: ManagedFieldCardinality = "string",
    includes: tuple[str, ...] = (),
    catalog: str | None = None,
    representation: str = "text",
    path: bool = False,
    merge: bool | None = None,
    provider: Literal["musicbrainz", "local_verified"] = "musicbrainz",
    max_values: int = 100,
) -> ManagedFieldDefinition:
    return ManagedFieldDefinition(
        name=name,
        canonical_source=source,
        source_provider=provider,
        scope=scope,
        cardinality=cardinality,
        order_significant=cardinality == "ordered_strings",
        required_includes=includes,
        enabled_in_picard_preset=True,
        catalog_projection=catalog,
        adapter_hooks=_hooks(name, representation),
        empty_canonical_behavior="preserve_unless_explicit_clear",
        allow_override=True,
        allow_preserve=True,
        participates_in_path_rendering=path,
        merge_supported=(cardinality == "ordered_strings" if merge is None else merge),
        max_unique_values=max_values,
    )


_FIELDS = (
    _field(
        "album", "release.title", scope="album", catalog="local_albums.title", path=True
    ),
    _field(
        "title", "track.title", scope="track", catalog="local_tracks.title", path=True
    ),
    _field(
        "album_sort",
        "local.album_sort",
        scope="album",
        catalog="local_albums.sort_name",
        provider="local_verified",
    ),
    _field(
        "title_sort",
        "local.title_sort",
        scope="track",
        catalog="local_tracks.sort_name",
        provider="local_verified",
    ),
    _field(
        "artist",
        "track.artist_credits.display_name",
        scope="track",
        cardinality="ordered_strings",
        includes=("artist-credits",),
        catalog="local_track_artists",
        representation="credits",
        path=True,
        merge=True,
    ),
    _field(
        "album_artist",
        "release.artist_credits.display_name",
        scope="album",
        cardinality="ordered_strings",
        includes=("artist-credits",),
        catalog="local_album_artists",
        representation="credits",
        path=True,
        merge=True,
    ),
    _field(
        "artist_sort",
        "track.artist_credits.sort_name",
        scope="track",
        cardinality="ordered_strings",
        includes=("artist-credits",),
        catalog="local_tracks.artist_sort",
        representation="credits",
    ),
    _field(
        "genre",
        "genre_projection",
        scope="track",
        cardinality="ordered_strings",
        catalog="local_track_genres",
        representation="list",
        merge=True,
        provider="local_verified",
    ),
    _field(
        "album_artist_sort",
        "release.artist_credits.sort_name",
        scope="album",
        cardinality="ordered_strings",
        includes=("artist-credits",),
        catalog="local_tracks.album_artist_sort",
        representation="credits",
    ),
    _field(
        "track_number",
        "track.position",
        scope="track",
        cardinality="integer",
        representation="number",
        path=True,
    ),
    _field(
        "total_tracks",
        "medium.track_count",
        scope="track",
        cardinality="integer",
        representation="number",
    ),
    _field(
        "disc_number",
        "medium.position",
        scope="track",
        cardinality="integer",
        representation="number",
        path=True,
    ),
    _field(
        "total_discs",
        "release.media.count",
        scope="album",
        cardinality="integer",
        representation="number",
    ),
    _field(
        "disc_subtitle",
        "medium.title",
        scope="track",
        catalog="local_tracks.disc_subtitle",
    ),
    _field("date", "release.date", scope="album", representation="date", path=True),
    _field(
        "original_date",
        "release_group.first_release_date",
        scope="album",
        representation="date",
    ),
    _field("release_status", "release.status", scope="album"),
    _field("release_country", "release.country", scope="album"),
    _field(
        "release_type",
        "release_group.primary_and_secondary_types",
        scope="album",
        cardinality="ordered_strings",
        includes=("release-groups",),
        representation="list",
    ),
    _field("media", "medium.format", scope="track"),
    _field(
        "compilation",
        "release.artist_credit",
        scope="album",
        cardinality="boolean",
        representation="boolean",
    ),
    _field(
        "label",
        "release.label_info.label.name",
        scope="album",
        cardinality="ordered_strings",
        includes=("labels",),
        representation="list",
    ),
    _field(
        "catalog_number",
        "release.label_info.catalog_number",
        scope="album",
        cardinality="ordered_strings",
        includes=("labels",),
        representation="list",
    ),
    _field("barcode", "release.barcode", scope="album"),
    _field("asin", "release.asin", scope="album"),
    _field(
        "isrc",
        "recording.isrcs",
        scope="track",
        cardinality="ordered_strings",
        includes=("isrcs",),
        representation="identifier_list",
        merge=True,
    ),
    _field(
        "musicbrainz_recording_id",
        "recording.id",
        scope="track",
        representation="identifier",
    ),
    _field(
        "musicbrainz_release_track_id",
        "release_track.id",
        scope="track",
        representation="identifier",
    ),
    _field(
        "musicbrainz_release_id",
        "release.id",
        scope="album",
        representation="identifier",
    ),
    _field(
        "musicbrainz_release_group_id",
        "release_group.id",
        scope="album",
        includes=("release-groups",),
        representation="identifier",
    ),
    _field(
        "musicbrainz_artist_id",
        "track.artist_credits.artist.id",
        scope="track",
        cardinality="ordered_strings",
        includes=("artist-credits",),
        representation="identifier_list",
        merge=True,
    ),
    _field(
        "musicbrainz_album_artist_id",
        "release.artist_credits.artist.id",
        scope="album",
        cardinality="ordered_strings",
        includes=("artist-credits",),
        representation="identifier_list",
        merge=True,
    ),
    _field(
        "work",
        "recording.performance.work.title",
        scope="track",
        includes=("work-rels",),
    ),
    _field(
        "musicbrainz_work_id",
        "recording.performance.work.id",
        scope="track",
        cardinality="ordered_strings",
        includes=("work-rels",),
        representation="identifier_list",
    ),
    _field("movement", "work.movement.title", scope="track", includes=("work-rels",)),
    _field(
        "movement_number",
        "work.movement.position",
        scope="track",
        cardinality="integer",
        includes=("work-rels",),
        representation="number",
    ),
    _field(
        "movement_count",
        "work.movement.total",
        scope="track",
        cardinality="integer",
        includes=("work-rels",),
        representation="number",
    ),
    _field(
        "composer",
        "work.relations.composer",
        scope="track",
        cardinality="ordered_strings",
        includes=("work-rels", "work-level-rels"),
        representation="relationship",
        merge=True,
    ),
    _field(
        "lyricist",
        "work.relations.lyricist",
        scope="track",
        cardinality="ordered_strings",
        includes=("work-rels", "work-level-rels"),
        representation="relationship",
        merge=True,
    ),
    _field(
        "conductor",
        "recording.relations.conductor",
        scope="track",
        cardinality="ordered_strings",
        includes=("recording-level-rels",),
        representation="relationship",
        merge=True,
    ),
    _field(
        "performer",
        "recording.relations.performer",
        scope="track",
        cardinality="ordered_strings",
        includes=("recording-level-rels",),
        representation="relationship",
        merge=True,
        max_values=256,
    ),
    _field(
        "arranger",
        "recording_or_work.relations.arranger",
        scope="track",
        cardinality="ordered_strings",
        includes=("recording-level-rels", "work-level-rels"),
        representation="relationship",
        merge=True,
    ),
    _field(
        "remixer",
        "recording.relations.remixer",
        scope="track",
        cardinality="ordered_strings",
        includes=("recording-level-rels",),
        representation="relationship",
        merge=True,
    ),
    _field(
        "producer",
        "recording.relations.producer",
        scope="track",
        cardinality="ordered_strings",
        includes=("recording-level-rels",),
        representation="relationship",
        merge=True,
    ),
    _field(
        "acoustid_id",
        "local.verified_acoustid_id",
        scope="track",
        provider="local_verified",
        representation="identifier",
    ),
    _field(
        "acoustid_fingerprint",
        "local.verified_acoustid_fingerprint",
        scope="track",
        provider="local_verified",
        representation="fingerprint",
    ),
)

MANAGED_FIELD_REGISTRY: dict[str, ManagedFieldDefinition] = {
    value.name: value for value in _FIELDS
}

if set(MANAGED_FIELD_REGISTRY) != {*MANAGED_FIELD_NAMES, "genre"}:
    missing = sorted(set(MANAGED_FIELD_NAMES) - set(MANAGED_FIELD_REGISTRY))
    extra = sorted(set(MANAGED_FIELD_REGISTRY) - set(MANAGED_FIELD_NAMES))
    raise RuntimeError(
        f"Managed field registry mismatch; missing={missing}, extra={extra}"
    )

if {
    name for name, field in MANAGED_FIELD_REGISTRY.items() if field.merge_supported
} != {*MERGEABLE_MANAGED_FIELD_NAMES, "genre"}:
    raise RuntimeError("Managed field merge capability registry is inconsistent.")


def get_managed_field(name: str) -> ManagedFieldDefinition | None:
    return MANAGED_FIELD_REGISTRY.get(name)


def canonical_track_values(
    release: CanonicalReleaseDocument, track: CanonicalTrackDocument
) -> dict[str, object]:
    relationships: dict[str, list[str]] = {
        role: []
        for role in (
            "composer",
            "lyricist",
            "conductor",
            "performer",
            "arranger",
            "remixer",
            "producer",
        )
    }
    for credit in track.relationship_credits:
        if credit.role in relationships:
            relationships[credit.role].append(credit.display_name)
    release_types = tuple(
        value
        for value in (release.primary_release_type, *release.secondary_release_types)
        if value
    )
    return {
        "album": release.title,
        "title": track.title,
        # MusicBrainz does not supply release/title sort strings. Existing values
        # remain available to profile scripts and are preserved unless transformed.
        "album_sort": None,
        "title_sort": None,
        "artist": tuple(value.display_name for value in track.artist_credits),
        "album_artist": tuple(value.display_name for value in release.artist_credits),
        "artist_sort": tuple(value.sort_name for value in track.artist_credits),
        "album_artist_sort": tuple(value.sort_name for value in release.artist_credits),
        "track_number": track.track_number,
        "total_tracks": track.total_tracks,
        "disc_number": track.disc_number,
        "total_discs": track.total_discs,
        "disc_subtitle": track.disc_subtitle,
        "date": release.date.value if release.date is not None else None,
        "original_date": (
            release.original_date.value if release.original_date is not None else None
        ),
        "release_status": release.release_status,
        "release_country": release.release_country,
        "release_type": release_types,
        "media": track.media_format,
        "compilation": release.compilation,
        "label": tuple(value.name for value in release.labels if value.name),
        "catalog_number": tuple(
            value.catalog_number for value in release.labels if value.catalog_number
        ),
        "barcode": release.barcode,
        "asin": release.asin,
        "isrc": track.identifiers.isrcs,
        "musicbrainz_recording_id": track.identifiers.recording_mbid,
        "musicbrainz_release_track_id": track.identifiers.release_track_mbid,
        "musicbrainz_release_id": release.identifiers.release_mbid,
        "musicbrainz_release_group_id": release.identifiers.release_group_mbid,
        "musicbrainz_artist_id": track.identifiers.artist_mbids,
        "musicbrainz_album_artist_id": release.identifiers.album_artist_mbids,
        "work": track.work_title,
        "musicbrainz_work_id": track.identifiers.work_mbids,
        "movement": track.movement,
        "movement_number": track.movement_number,
        "movement_count": track.movement_count,
        **{name: tuple(values) for name, values in relationships.items()},
        "acoustid_id": None,
        "acoustid_fingerprint": None,
    }
