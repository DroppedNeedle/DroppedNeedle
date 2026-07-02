"""HeldImport - a downloaded file that matched a track but failed the AcoustID
recording-identity check, copied aside for a human "import anyway" / "discard"
decision. The download-side counterpart to the scan's manual_review_queue: here the
target track IS known (we matched it by duration), only the identity backstop
disagreed - usually because MusicBrainz's crowd metadata for that recording is wrong."""

from infrastructure.msgspec_fastapi import AppStruct


class HeldImport(AppStruct):
    id: int
    user_id: str
    held_path: str
    reason: str
    source: str
    status: str
    created_at: float
    release_group_mbid: str | None = None
    release_mbid: str | None = None
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
    resolved_at: float | None = None
