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
from repositories.protocols.download_client import TaskHandle


class ExpectedFile(AppStruct):
    """One file the task enqueued. ``filename`` is the correlation key (C2);
    ``duration`` feeds the always-on post-download duration check (D15/B1)."""

    filename: str
    size: int
    duration: float | None = None


class ExpectedTrack(AppStruct):
    """One track of the MusicBrainz tracklist the Usenet import matches enumerated
    files against (D18). The unpacked filenames are unknown until after unpack, so
    Usenet keys the import on ``(disc_number, track_number)`` position validated by
    ``duration_seconds``, rather than on a pre-known filename. ``recording_mbid`` /
    ``title`` are tie-breakers when present."""

    track_number: int
    disc_number: int = 1
    duration_seconds: float | None = None
    recording_mbid: str | None = None
    title: str | None = None


class DownloadManifest(AppStruct):
    """Durable per-task import + correlation record written at enqueue time.

    Carries BOTH ``source_username`` (legacy slskd correlation) and ``handle`` (the
    generalised ``TaskHandle``); both are optional. ``AppStruct`` has no
    ``forbid_unknown_fields``, so msgspec DROPS unknown fields on decode rather than
    raising - a legacy on-disk manifest (``source_username`` only, no ``handle``)
    decodes cleanly, and ``__post_init__`` back-fills a soulseek ``TaskHandle`` so a
    download that was mid-flight at upgrade survives the deploy (in-flight migration).
    """

    task_id: str
    release_group_mbid: str
    artist_name: str
    album_title: str
    naming_template: str
    target_files: list[ExpectedFile]
    source_username: str | None = None
    handle: TaskHandle | None = None
    # Expected MusicBrainz tracklist. The folder-based Usenet import matches enumerated
    # files against it (D18). The slskd path keys the import on ``target_files``
    # filenames instead, but carries the ONE expected track here when the download
    # targets a single known recording (a track download or a 1-track single) - it arms
    # the AcoustID title check and the import-time tag verification (2026-07-05
    # wrong-single incident). Defaulted, so pre-existing on-disk manifests still decode.
    expected_tracks: list[ExpectedTrack] = []
    release_mbid: str | None = None
    artist_mbid: str | None = None
    year: int | None = None
    # True when this is a single-track download whose ``duration`` is the canonical
    # track length (from MusicBrainz). A duration mismatch then means "wrong track for
    # this request" (fail over to another source), not a corrupt file to quarantine.
    is_track: bool = False
    # Set by the last-resort track re-pull (every candidate failed the canonical-
    # duration gate, so the MB length is suspect): a gate failure then HOLDS the file
    # for human review instead of silently importing with the gate off (D9) or
    # discarding it. Defaulted - pre-existing on-disk manifests decode unchanged.
    hold_on_wrong_track: bool = False
    # The owning task's origin ('user' | 'retry' | 'upgrade'). Replace-on-import fires
    # only for 'upgrade' (D18); legacy manifests decode as 'user' (add-only, unchanged).
    origin: str = "user"

    def __post_init__(self) -> None:
        # In-flight back-fill: a legacy manifest decodes with handle=None and
        # source_username set; synthesise the soulseek handle so poll/cancel/import
        # can re-correlate the mid-flight transfers exactly as before.
        if self.handle is None and self.source_username is not None:
            self.handle = TaskHandle(
                source="soulseek",
                username=self.source_username,
                filenames=[f.filename for f in self.target_files],
            )


class ManifestCodec:
    """Encode/decode a ``DownloadManifest`` to/from the on-disk JSON bytes."""

    def encode(self, manifest: DownloadManifest) -> bytes:
        return msgspec.json.encode(manifest)

    def decode(self, data: bytes) -> DownloadManifest:
        return msgspec.json.decode(data, type=DownloadManifest)
