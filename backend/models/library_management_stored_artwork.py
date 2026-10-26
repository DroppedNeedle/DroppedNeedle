"""Persisted form of import artwork: rows keep hash references, not image bytes."""

from __future__ import annotations

from collections.abc import Sequence
import hashlib

import msgspec

from models.audio_metadata import EmbeddedArtworkDescriptor
from models.library_management import (
    LibraryManagementImportBundle,
    LibraryManagementImportFile,
)
from models.library_management_artwork import StoredEmbeddedArtwork


def stored_embedded_artwork(
    artwork: Sequence[EmbeddedArtworkDescriptor],
) -> tuple[StoredEmbeddedArtwork, ...]:
    """Reference each image by the hash of its bytes, which is its blob key."""

    return tuple(
        StoredEmbeddedArtwork(
            image_type=image.image_type,
            mime_type=image.mime_type,
            description=image.description,
            width=image.width,
            height=image.height,
            byte_size=image.byte_size,
            sha256=hashlib.sha256(image.content).hexdigest(),
            format_supported=image.format_supported,
        )
        for image in artwork
    )


def embedded_artwork_choices(
    artwork: Sequence[StoredEmbeddedArtwork],
) -> list[dict[str, object]]:
    """Plan-item artwork choices in the shape the preview planner writes."""

    return [
        {
            "output_kind": "embedded",
            "image_type": image.image_type,
            "blob_sha256": image.sha256,
            "mime_type": image.mime_type,
            "format_supported": image.format_supported,
            "width": image.width,
            "height": image.height,
            "byte_size": image.byte_size,
            "description": image.description,
            "destination_relative_path": None,
        }
        for image in artwork
    ]


def persisted_import_file(file: LibraryManagementImportFile) -> LibraryManagementImportFile:
    """The file without artwork bytes; a file already in this form is unchanged.

    ``None`` and empty desired artwork keep their meaning (leave alone versus
    no images), so only a non-empty set is replaced by references.
    """

    desired = file.desired_document
    desired_artwork = file.desired_artwork
    if desired is not None and desired.artwork:
        desired_artwork = stored_embedded_artwork(desired.artwork)
        desired = msgspec.structs.replace(desired, artwork=None)
    return msgspec.structs.replace(
        file,
        desired_document=desired,
        desired_artwork=desired_artwork,
        artifacts=tuple(
            msgspec.structs.replace(
                artifact,
                content=None,
                content_blob_sha256=hashlib.sha256(artifact.content).hexdigest(),
            )
            if artifact.content is not None
            else artifact
            for artifact in file.artifacts
        ),
    )


def persisted_import_bundle(
    bundle: LibraryManagementImportBundle,
) -> LibraryManagementImportBundle:
    return msgspec.structs.replace(
        bundle, files=tuple(persisted_import_file(value) for value in bundle.files)
    )


def stored_artwork_descriptor(
    image: StoredEmbeddedArtwork, content: bytes
) -> EmbeddedArtworkDescriptor:
    return EmbeddedArtworkDescriptor(
        image_type=image.image_type,
        mime_type=image.mime_type,
        description=image.description,
        width=image.width,
        height=image.height,
        byte_size=image.byte_size,
        sha256=image.sha256,
        content=content,
        format_supported=image.format_supported,
    )
