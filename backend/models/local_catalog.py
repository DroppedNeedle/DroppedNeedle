"""Provider-independent native-library catalog domain models."""

from __future__ import annotations

from typing import Literal

import msgspec

from infrastructure.msgspec_fastapi import AppStruct

ArtistKind = Literal["person", "group", "various_artists", "unknown"]
Availability = Literal["indexed", "excluded", "missing"]
StatRevisionKind = Literal["exact", "legacy_float", "legacy_review", "unclassified"]
IdentificationDecisionSource = Literal[
    "embedded", "automatic", "manual", "legacy_import"
]


class LocalArtist(AppStruct):
    id: str
    display_name: str
    folded_name: str
    kind: ArtistKind
    normalized_name: str = ""
    sort_name: str | None = None
    created_at: float = 0.0
    updated_at: float = 0.0
    row_revision: int = 1


class LocalArtistCredit(AppStruct):
    local_artist_id: str
    position: int
    role: str = "primary"
    credited_name: str | None = None
    join_phrase: str = ""


class LocalTrackGenre(AppStruct):
    local_track_id: str
    position: int
    name: str
    folded_name: str
    source: Literal["local", "musicbrainz", "listenbrainz", "lastfm", "override"]
    genre_mbid: str | None = None
    weight: int | None = None
    source_document_revision: str | None = None


class LocalAlbum(AppStruct):
    id: str
    root_id: str
    grouping_key: str
    title: str
    album_artist_id: str
    album_artist_name: str | None = None
    album_artist_sort_name: str | None = None
    year: int | None = None
    original_release_date: str | None = None
    primary_genre: str | None = None
    is_compilation: bool = False
    grouping_source: Literal["automatic", "legacy_import", "manual"] = "automatic"
    grouping_locked: bool = False
    retired_into_album_id: str | None = None
    created_at: float = 0.0
    updated_at: float = 0.0
    row_revision: int = 1


class LocalTrack(AppStruct):
    id: str
    local_album_id: str
    root_id: str
    file_path: str
    relative_path: str
    path_hash: str
    file_size_bytes: int
    file_mtime_ns: int
    stat_revision: str
    title: str
    album_title: str
    stat_revision_kind: StatRevisionKind = "exact"
    disc_number: int = 1
    track_number: int = 0
    artist_name: str | None = None
    album_artist_name: str | None = None
    tag_album_title: str | None = None
    tag_album_artist_name: str | None = None
    year: int | None = None
    genre: str | None = None
    title_sort: str | None = None
    artist_sort: str | None = None
    album_sort: str | None = None
    album_artist_sort: str | None = None
    disc_subtitle: str | None = None
    is_compilation: bool = False
    embedded_release_group_mbid: str | None = None
    embedded_release_mbid: str | None = None
    embedded_recording_mbid: str | None = None
    embedded_release_track_mbid: str | None = None
    embedded_artist_mbid: str | None = None
    embedded_album_artist_mbid: str | None = None
    tag_revision: str | None = None
    tags_read_at: float | None = None
    metadata_incomplete: bool = False
    duration_seconds: float | None = None
    file_format: str = ""
    bit_rate: int | None = None
    sample_rate: int | None = None
    bit_depth: int | None = None
    channels: int | None = None
    replaygain_track_gain: float | None = None
    replaygain_album_gain: float | None = None
    replaygain_track_peak: float | None = None
    replaygain_album_peak: float | None = None
    availability: Availability = "indexed"
    missing_since: float | None = None
    excluded_at: float | None = None
    ingest_source: str = "scan"
    download_task_id: str | None = None
    source_path: str | None = None
    imported_at: float = 0.0
    membership_source: Literal["automatic", "legacy_import", "manual"] = "automatic"
    membership_locked: bool = False
    manual_excluded: bool = False
    desired_policy_revision: str = ""
    applied_policy_revision: str = ""
    applied_policy: Literal["local_metadata", "automatic", "excluded"] = "automatic"
    row_revision: int = 1


class LocalAlbumExternalIdentity(AppStruct):
    local_album_id: str
    release_group_mbid: str
    release_mbid: str | None = None
    decision_source: IdentificationDecisionSource = "automatic"
    matcher_version: str | None = None
    attempt_id: str | None = None
    selected_by_user_id: str | None = None
    selected_at: float = 0.0
    row_revision: int = 1


class LocalArtistExternalIdentity(AppStruct):
    local_artist_id: str
    provider_artist_id: str
    decision_source: IdentificationDecisionSource = "automatic"
    attempt_id: str | None = None
    selected_by_user_id: str | None = None
    selected_at: float = 0.0
    row_revision: int = 1


class LocalTrackExternalIdentity(AppStruct):
    local_track_id: str
    recording_mbid: str
    release_mbid: str | None = None
    release_track_mbid: str | None = None
    medium_position: int | None = None
    release_track_position: int | None = None
    decision_source: IdentificationDecisionSource = "automatic"
    attempt_id: str | None = None
    selected_at: float = 0.0
    row_revision: int = 1


class LocalArtistAlias(AppStruct):
    alias: str
    local_artist_id: str
    kind: Literal["legacy_artist", "merged_artist", "compat_migration"]
    created_at: float = 0.0


class LocalAlbumAlias(AppStruct):
    alias: str
    local_album_id: str
    kind: Literal["legacy_release_group", "merged_album", "compat_migration"]
    created_at: float = 0.0


class LocalArtworkAssociation(AppStruct):
    local_album_id: str
    cover_url: str | None
    source: Literal["embedded", "cover_cache", "manual", "provider"]
    source_locator: str | None = None
    version: int = 1
    updated_at: float = 0.0
    row_revision: int = 1


class CatalogMembership(AppStruct):
    album: LocalAlbum
    artists: list[LocalArtist] = msgspec.field(default_factory=list)
    tracks: list[LocalTrack] = msgspec.field(default_factory=list)
    album_credits: list[LocalArtistCredit] = msgspec.field(default_factory=list)
    track_credits: dict[str, list[LocalArtistCredit]] = msgspec.field(
        default_factory=dict
    )
    track_genres: dict[str, list[LocalTrackGenre]] = msgspec.field(default_factory=dict)
