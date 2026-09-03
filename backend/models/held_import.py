"""A downloaded file kept in app-owned storage until its blocked import is resolved.

Identity holds are reviewed per track. Library Management holds keep the complete
acquisition unit together and are retried or discarded as one album-level action.
"""

from infrastructure.msgspec_fastapi import AppStruct


class HeldImport(AppStruct):
    id: int
    user_id: str
    held_path: str
    reason: str
    source: str
    status: str
    created_at: float
    reason_detail: str | None = None
    release_group_mbid: str | None = None
    release_mbid: str | None = None
    release_track_mbid: str | None = None
    recording_mbid: str | None = None
    track_number: int | None = None
    disc_number: int | None = None
    track_title: str | None = None
    artist_name: str | None = None
    artist_mbid: str | None = None
    album_title: str | None = None
    year: int | None = None
    original_filename: str | None = None
    file_format: str | None = None
    duration_seconds: float | None = None
    # Canonical MusicBrainz length (seconds) at hold time: the file-length vs
    # expected-length comparison on the review card. Nullable for legacy rows.
    expected_duration_seconds: float | None = None
    # What AcoustID confidently identified the audio as (the reason we held it) - shown to
    # the human so "import anyway" is an informed call, not a blind trust.
    evidence_title: str | None = None
    evidence_artist: str | None = None
    evidence_score: float | None = None
    source_task_id: str | None = None
    # the owning task's origin, persisted at hold time - the task row is deletable,
    # and an upgrade's "import anyway" must keep its replace semantics (D10/D18)
    origin: str = "user"
    # the naming template the rest of the album imported under, so "import anyway" places
    # this track consistently with its siblings even if the setting later changes
    naming_template: str | None = None
    management_retry_count: int = 0
    management_next_retry_at: float | None = None
    resolved_at: float | None = None
