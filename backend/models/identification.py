"""Normalized identification attempts and evidence."""

from __future__ import annotations

from typing import Literal

import msgspec

from infrastructure.msgspec_fastapi import AppStruct

EvidenceClass = Literal["supported", "unknown", "contradictory"]
IdentificationOutcome = Literal[
    "identified",
    "edition_uncertain",
    "no_candidate",
    "ambiguous",
    "contradictory",
    "insufficient_evidence",
    "kept_tagged",
    "provider_deferred",
    "failed",
]


class CandidateTrack(AppStruct):
    title: str
    position: int
    disc_number: int = 1
    absolute_position: int = 0
    duration_seconds: float | None = None
    recording_mbid: str | None = None
    release_track_mbid: str | None = None


class AlbumCandidate(AppStruct):
    release_group_mbid: str
    release_mbid: str | None
    album_title: str
    album_artist_name: str
    tracks: list[CandidateTrack] = msgspec.field(default_factory=list)
    artist_mbid: str | None = None
    release_type: str | None = None
    secondary_types: list[str] = msgspec.field(default_factory=list)
    release_date: str | None = None
    source_kinds: list[str] = msgspec.field(default_factory=list)


class ReleaseEdition(AppStruct):
    release_mbid: str
    release_group_mbid: str
    artist_name: str
    title: str
    date: str | None = None
    country: str | None = None
    status: str | None = None
    packaging: str | None = None
    media_formats: list[str] = msgspec.field(default_factory=list)
    disc_count: int = 0
    track_count: int = 0
    label: str | None = None
    catalogue_number: str | None = None
    barcode: str | None = None
    disambiguation: str | None = None
    musicbrainz_url: str = ""
    score: int = 0


class ReleaseEditionSearchPage(AppStruct):
    items: list[ReleaseEdition] = msgspec.field(default_factory=list)
    total: int = 0
    offset: int = 0
    limit: int = 12


class TrackEvidence(AppStruct):
    local_track_id: str
    classification: EvidenceClass
    evidence_kinds: list[str] = msgspec.field(default_factory=list)
    candidate_track_title: str | None = None
    candidate_disc_number: int | None = None
    candidate_track_position: int | None = None
    recording_mbid: str | None = None
    release_track_mbid: str | None = None
    recording_mbid_redirects: list[str] = msgspec.field(default_factory=list)


class CandidateEvidence(AppStruct):
    release_group_mbid: str
    release_mbid: str | None = None
    album_title: str = ""
    album_artist_name: str = ""
    artist_mbid: str | None = None
    release_type: str | None = None
    release_date: str | None = None
    local_album_title: str = ""
    local_album_artist_name: str = ""
    album_title_classification: EvidenceClass = "unknown"
    album_artist_classification: EvidenceClass = "unknown"
    track_evidence: list[TrackEvidence] = msgspec.field(default_factory=list)
    unmatched_expected_tracks: list[str] = msgspec.field(default_factory=list)
    score: float = 0.0
    margin: float = 0.0
    reason_code: str = ""
    matcher_version: str = ""


# Conflicts that come only from the identifiers embedded in a file (its recording or
# release-track MBID belongs to another edition of the same release group). They say
# nothing about the audio itself, unlike a duration conflict or an ambiguous pairing.
ID_ONLY_CONFLICT_KINDS = frozenset(
    {"recording_mbid_conflict", "release_track_mbid_conflict"}
)


def is_reassignable_track(item: TrackEvidence) -> bool:
    """A contradictory track whose only disagreement is its embedded identifiers.

    The evidence engine records a proposed candidate track only when exactly one
    candidate track is consistent with the file on descriptive grounds (normalized
    title plus position or duration), without trusting the conflicting identifiers.
    """

    return (
        item.classification == "contradictory"
        and bool(item.evidence_kinds)
        and set(item.evidence_kinds) <= ID_ONLY_CONFLICT_KINDS
        and item.candidate_track_title is not None
        and item.candidate_track_position is not None
        and bool(item.recording_mbid)
        and bool(item.release_track_mbid)
    )


def id_only_reassignments(
    track_evidence: list[TrackEvidence],
    unmatched_expected_tracks: list[str],
    indexed_track_count: int,
) -> list[TrackEvidence]:
    """Identifier-only conflicts that can be assigned to the candidate, or [] if not eligible.

    Every local track is either supported or an identifier-only conflict with a proposed
    edition track, at least one is the latter, no two tracks claim the same edition track,
    and the expected tracks left unmatched are exactly the ones the reassigned tracks fill,
    so a track that is really missing can never be hidden by a reassignment.
    """

    if len(track_evidence) != indexed_track_count:
        return []
    reassigned: list[TrackEvidence] = []
    for item in track_evidence:
        if item.classification == "supported":
            continue
        if not is_reassignable_track(item):
            return []
        reassigned.append(item)
    if not reassigned:
        return []
    slots = [
        (item.candidate_disc_number, item.candidate_track_position)
        for item in track_evidence
        if item.candidate_track_position is not None
    ]
    if len(slots) != len(set(slots)):
        return []
    filled = sorted(
        f"{item.candidate_disc_number}:{item.candidate_track_position}:"
        f"{item.candidate_track_title}"
        for item in reassigned
    )
    if filled != sorted(unmatched_expected_tracks):
        return []
    return reassigned


def track_reassignment_plan(
    evidence: CandidateEvidence, indexed_track_count: int
) -> list[TrackEvidence]:
    """Tracks an administrator may reassign to this edition, or [] when not eligible.

    Eligible only when the album title and artist are not contradicted and the tracks
    satisfy `id_only_reassignments`.
    """

    if (
        evidence.album_title_classification == "contradictory"
        or evidence.album_artist_classification == "contradictory"
    ):
        return []
    return id_only_reassignments(
        evidence.track_evidence,
        evidence.unmatched_expected_tracks,
        indexed_track_count,
    )


class IdentificationDecision(AppStruct):
    outcome: IdentificationOutcome
    reason_code: str
    selected_candidate_key: str | None = None
    candidates: list[CandidateEvidence] = msgspec.field(default_factory=list)
    edition_uncertain: bool = False
    release_group_mbid: str | None = None
    ranked_edition_keys: list[str] = msgspec.field(default_factory=list)


TrackProvenance = Literal["tag", "parsed", "placeholder", "absent"]


class GroupingTrack(AppStruct):
    local_track_id: str
    root_id: str
    relative_path: str
    title: str = ""
    artist_name: str = ""
    album_title: str = ""
    album_artist_name: str = ""
    title_provenance: TrackProvenance = "absent"
    album_title_provenance: TrackProvenance = "absent"
    album_artist_provenance: TrackProvenance = "absent"
    artist_sort_name: str | None = None
    album_artist_sort_name: str | None = None
    track_number: int = 0
    disc_number: int = 1
    duration_seconds: float | None = None
    recording_mbid: str | None = None
    # Support-only AcoustID evidence, never authoritative proof.
    fingerprint_recording_mbid: str | None = None
    release_mbid: str | None = None
    release_group_mbid: str | None = None
    release_track_mbid: str | None = None
    is_compilation: bool = False
    tags_readable: bool = True
    membership_locked: bool = False
    current_album_id: str | None = None


class ProposedLocalAlbum(AppStruct):
    grouping_key: str
    title: str
    album_artist_name: str
    track_ids: list[str]
    reason_code: str
    retained_album_id: str | None = None
    continuity_reason_code: str | None = None


class ExistingAlbumMembership(AppStruct):
    local_album_id: str
    track_ids: list[str]
    created_at: float = 0.0


class GroupingApplication(AppStruct):
    group: ProposedLocalAlbum
    local_album_id: str
    local_artist_id: str


class IdentificationAttempt(AppStruct):
    id: str
    local_album_id: str | None = None
    local_track_id: str | None = None
    trigger: str = "automatic"
    requested_by_user_id: str | None = None
    input_tag_revision: str = ""
    input_policy_revision: str = ""
    input_file_revision: str = ""
    input_identity_revision: str = ""
    matcher_version: str = ""
    state: str = "completed"
    terminal_reason_code: str = ""
    selected_candidate_key: str | None = None
    candidate_count: int = 0
    degradation_flags: list[str] = msgspec.field(default_factory=list)
    started_at: float = 0.0
    completed_at: float = 0.0


class IdentificationEvidenceRecord(AppStruct):
    id: str
    attempt_id: str
    candidate_key: str
    evidence: CandidateEvidence
    created_at: float = 0.0


class FingerprintOutcome(AppStruct):
    id: str
    local_track_id: str
    stat_revision: str
    fingerprinter_version: str
    state: Literal["matched", "no_match", "failed", "disabled", "skipped", "deferred"]
    fingerprint: str | None = None
    duration_seconds: float | None = None
    recording_mbid: str | None = None
    release_group_ids: list[str] = msgspec.field(default_factory=list)
    score: float | None = None
    failure_code: str | None = None
    attempt_count: int = 1
    first_attempt_at: float = 0.0
    last_attempt_at: float = 0.0
    retry_after: float | None = None
    row_revision: int = 1
    partial_decode: bool = False


class AlbumCoverage(AppStruct):
    local_album_id: str
    musicbrainz_release_group_id: str | None = None
    identity_source: str | None = None
    stale: bool = False
    manual: bool = False
    supported: list[TrackEvidence] = msgspec.field(default_factory=list)
    unknown: list[TrackEvidence] = msgspec.field(default_factory=list)
    contradictory: list[TrackEvidence] = msgspec.field(default_factory=list)
    missing_expected_tracks: list[str] = msgspec.field(default_factory=list)
    evidence_revision: str = ""
    last_evaluated_at: float | None = None


class EvidenceProjection(AppStruct):
    supported_track_ids: list[str] = msgspec.field(default_factory=list)
    unknown_track_ids: list[str] = msgspec.field(default_factory=list)
    contradictory_track_ids: list[str] = msgspec.field(default_factory=list)
    reason_code: str = ""
