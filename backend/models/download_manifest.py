"""Download manifest - DroppedNeedle-side crash-recovery + correlation state (Phase 7).

slskd has no batch id and no per-request ``externalId`` (C2): a task is tied to
its slskd transfers only by ``source_username`` plus the exact set of filenames it
enqueued. The manifest persists that pair (plus the album metadata the importer
needs) to ``staging/{task_id}/manifest.json`` at enqueue time, so a restart can
re-correlate the task to its transfers and finish the import.

The audio is NOT staged here - slskd writes completed files into its own
``directories.downloads`` (C4); staging holds only this manifest. ``ManifestCodec``
round-trips the struct through ``msgspec.json`` (a plain file, not a DB blob - so
AUD-9's house-codec rule for blob columns does not apply).
"""

import msgspec

from infrastructure.msgspec_fastapi import AppStruct


class ExpectedFile(AppStruct):
    """One file the task enqueued. ``filename`` is the correlation key (C2);
    ``duration`` feeds the always-on post-download duration check (D15/B1)."""

    filename: str
    size: int
    duration: float | None = None


class DownloadManifest(AppStruct):
    """Durable per-task import + correlation record written at enqueue time."""

    task_id: str
    source_username: str
    release_group_mbid: str
    artist_name: str
    album_title: str
    naming_template: str
    target_files: list[ExpectedFile]
    release_mbid: str | None = None
    artist_mbid: str | None = None
    year: int | None = None
    # True when this is a single-track download whose ``duration`` is the canonical
    # track length (from MusicBrainz). A duration mismatch then means "wrong track for
    # this request" (fail over to another source), not a corrupt file to quarantine.
    is_track: bool = False


class ManifestCodec:
    """Encode/decode a ``DownloadManifest`` to/from the on-disk JSON bytes."""

    def encode(self, manifest: DownloadManifest) -> bytes:
        return msgspec.json.encode(manifest)

    def decode(self, data: bytes) -> DownloadManifest:
        return msgspec.json.decode(data, type=DownloadManifest)
