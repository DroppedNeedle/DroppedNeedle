"""Provider-neutral artwork candidates, inspected payloads, and projections."""

from __future__ import annotations

from typing import Literal

import msgspec

ArtworkSource = Literal[
    "cover_art_archive_release",
    "cover_art_archive_release_group",
    "local_files",
    "embedded",
    "audiodb",
]
ArtworkImageType = Literal[
    "front",
    "back",
    "booklet",
    "medium",
    "tray",
    "obi",
    "spine",
    "track",
    "other",
]
ArtworkFormat = Literal["jpeg", "png", "webp", "gif", "pdf"]
ArtworkProcessingFormat = Literal["original", "jpeg", "png", "webp"]
ArtworkOutputKind = Literal["embedded", "external"]


class ArtworkCandidate(msgspec.Struct, frozen=True, kw_only=True):
    candidate_id: str
    source: ArtworkSource
    locator: str
    image_types: tuple[ArtworkImageType, ...]
    approved: bool
    primary: bool
    description: str = ""
    source_entity_mbid: str | None = None
    source_is_exact_release: bool = False
    declared_mime_type: str | None = None
    boundary_root: str | None = None


class InspectedArtwork(msgspec.Struct, frozen=True, kw_only=True):
    candidate: ArtworkCandidate
    content: bytes
    mime_type: str
    format: ArtworkFormat
    width: int | None
    height: int | None
    byte_size: int
    sha256: str
    external_only: bool = False


class ExistingArtworkDescriptor(msgspec.Struct, frozen=True, kw_only=True):
    image_type: ArtworkImageType
    mime_type: str
    width: int | None
    height: int | None
    byte_size: int
    sha256: str


class StoredEmbeddedArtwork(msgspec.Struct, frozen=True, kw_only=True):
    """Embedded artwork of a persisted import file; the bytes sit in the blob store under ``sha256``."""

    image_type: ArtworkImageType
    mime_type: str | None
    description: str
    width: int | None
    height: int | None
    byte_size: int
    sha256: str
    format_supported: bool


class ArtworkOutput(msgspec.Struct, frozen=True, kw_only=True):
    output_kind: ArtworkOutputKind
    image_type: ArtworkImageType
    content: bytes
    mime_type: str
    format: ArtworkFormat
    width: int | None
    height: int | None
    byte_size: int
    sha256: str
    source: ArtworkSource
    source_candidate_id: str
    source_is_exact_release: bool
    description: str = ""


class ArtworkDecision(msgspec.Struct, frozen=True, kw_only=True):
    output_kind: ArtworkOutputKind
    image_type: ArtworkImageType
    action: Literal["replace", "add", "preserve", "skip"]
    reason: str
    candidate_id: str | None = None


class ArtworkProjection(msgspec.Struct, frozen=True, kw_only=True):
    embedded: tuple[ArtworkOutput, ...] = ()
    external: tuple[ArtworkOutput, ...] = ()
    decisions: tuple[ArtworkDecision, ...] = ()
    deferred_sources: tuple[ArtworkSource, ...] = ()
    preserved_existing: bool = False
