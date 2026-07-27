"""The single conservative evidence engine for target library identification."""

from __future__ import annotations

import re
import unicodedata
from difflib import SequenceMatcher

from models.identification import (
    AlbumCandidate,
    CandidateEvidence,
    CandidateTrack,
    GroupingTrack,
    IdentificationDecision,
    TrackEvidence,
)
from services.native.local_album_grouper import _hungarian_min

MATCHER_VERSION = "feedback-fixes-v1"
PAIR_COST_CEILING = 0.40
ALBUM_DISTANCE_CEILING = 0.35
CANDIDATE_MARGIN_FLOOR = 0.05
# A tolerated mismatch is one where the tracklists disagree about a title, not one
# where the recording itself is provably different. A guest-artist suffix the release
# does not carry, a romanised title, a bonus track present in one edition and not the
# other: each of those contradicts exactly one track on an otherwise exact release.
# Requiring unanimity there rejects the correct release group over a tagging detail,
# so demand a strong majority instead and let the score carry the rest.
SUPPORTED_RATIO_FLOOR = 0.80
ORDINARY_ALBUM_MAX_FILES = 20
ORDINARY_UNKNOWN_LIMIT = 1
LARGE_UNKNOWN_LIMIT = 2
DURATION_GRACE_SECONDS = 10.0
DURATION_HARD_LIMIT_SECONDS = 30.0
MAX_CANDIDATES = 10

_NON_WORD = re.compile(r"[^\w]+", re.UNICODE)
_UNSAFE_SECONDARY_TYPES = frozenset({"compilation", "live"})
# Evidence that the local file and the candidate track are different recordings, as
# opposed to the same recording described differently. These stay absolute vetoes.
_HARD_CONFLICT_KINDS = frozenset({"recording_mbid_conflict", "duration_conflict"})


def _fold(value: str) -> str:
    decomposed = unicodedata.normalize("NFKD", value.strip())
    without_marks = "".join(
        character for character in decomposed if not unicodedata.combining(character)
    )
    return _NON_WORD.sub("", without_marks.casefold())


def _distance(left: str, right: str) -> float:
    left_folded, right_folded = _fold(left), _fold(right)
    if not left_folded and not right_folded:
        return 0.0
    if not left_folded or not right_folded:
        return 1.0
    return 1.0 - SequenceMatcher(None, left_folded, right_folded).ratio()


def _duration_difference(
    local: GroupingTrack, candidate: CandidateTrack
) -> float | None:
    if local.duration_seconds is None or candidate.duration_seconds is None:
        return None
    return abs(local.duration_seconds - candidate.duration_seconds)


class _Pair:
    def __init__(self, cost: float, hard_conflict: bool, kinds: list[str]) -> None:
        self.cost = cost
        self.hard_conflict = hard_conflict
        self.kinds = kinds


def _pair(local: GroupingTrack, candidate: CandidateTrack) -> _Pair:
    if local.recording_mbid and candidate.recording_mbid:
        if local.recording_mbid == candidate.recording_mbid:
            return _Pair(0.0, False, ["recording_mbid"])
        return _Pair(2.0, True, ["recording_mbid_conflict"])

    title_cost = _distance(local.title, candidate.title) if local.title else 1.0
    duration_difference = _duration_difference(local, candidate)
    if (
        duration_difference is not None
        and duration_difference > DURATION_HARD_LIMIT_SECONDS
    ):
        return _Pair(2.0, True, ["duration_conflict"])
    position_known = local.track_number > 0 and candidate.position > 0
    position_matches = position_known and (
        local.track_number == candidate.position
        or local.track_number == candidate.absolute_position
    )
    disc_matches = local.disc_number == candidate.disc_number
    exact_title = bool(local.title) and _fold(local.title) == _fold(candidate.title)
    kinds: list[str] = []
    if exact_title:
        kinds.append("normalized_title")
    if (
        duration_difference is not None
        and duration_difference <= DURATION_GRACE_SECONDS
    ):
        kinds.append("compatible_duration")
    if position_matches and disc_matches:
        kinds.append("compatible_position")
    duration_cost = (
        0.25
        if duration_difference is None
        else min(1.0, duration_difference / DURATION_HARD_LIMIT_SECONDS)
    )
    position_cost = 0.25 if not position_known else (0.0 if position_matches else 1.0)
    disc_cost = 0.0 if disc_matches else 0.5
    cost = (
        0.60 * title_cost
        + 0.25 * duration_cost
        + 0.10 * position_cost
        + 0.05 * disc_cost
    )
    sufficient = (
        (exact_title and "compatible_duration" in kinds)
        or (exact_title and "compatible_position" in kinds)
        or (cost <= PAIR_COST_CEILING and title_cost <= 0.35)
    )
    return _Pair(cost if sufficient else max(cost, 0.75), False, kinds)


def _album_metadata_class(local: str, candidate: str) -> str:
    if not local.strip():
        return "unknown"
    return "supported" if _distance(local, candidate) <= 0.20 else "contradictory"


class AlbumEvidenceEngine:
    """Assign tracks once, persist the result, and let every consumer reuse it."""

    def evaluate_candidate(
        self,
        local_tracks: list[GroupingTrack],
        candidate: AlbumCandidate,
    ) -> CandidateEvidence:
        local_count = len(local_tracks)
        candidate_count = len(candidate.tracks)
        size = local_count + candidate_count
        pair_cache: dict[tuple[int, int], _Pair] = {}
        if size:
            costs = [[2_000_000] * size for _ in range(size)]
            for local_index, local in enumerate(local_tracks):
                for candidate_index, candidate_track in enumerate(candidate.tracks):
                    pair = _pair(local, candidate_track)
                    pair_cache[(local_index, candidate_index)] = pair
                    costs[local_index][candidate_index] = int(pair.cost * 1_000_000)
                costs[local_index][candidate_count + local_index] = 650_000
            for dummy_row in range(local_count, size):
                for column in range(size):
                    costs[dummy_row][column] = 0
            assignment = _hungarian_min(costs)
        else:
            assignment = []

        used_candidates: set[int] = set()
        track_evidence: list[TrackEvidence] = []
        pair_costs: list[float] = []
        for local_index, local in enumerate(local_tracks):
            column = assignment[local_index]
            pair = pair_cache.get((local_index, column))
            if column < candidate_count and pair is not None and pair.cost < 0.65:
                candidate_track = candidate.tracks[column]
                used_candidates.add(column)
                pair_costs.append(pair.cost)
                track_evidence.append(
                    TrackEvidence(
                        local_track_id=local.local_track_id,
                        classification="supported",
                        evidence_kinds=pair.kinds,
                        candidate_track_title=candidate_track.title,
                        candidate_disc_number=candidate_track.disc_number,
                        candidate_track_position=candidate_track.position,
                        recording_mbid=candidate_track.recording_mbid,
                    )
                )
                continue
            comparable = bool(
                local.recording_mbid
                or local.title.strip()
                or local.track_number > 0
                or local.duration_seconds is not None
            )
            candidate_recordings = {
                candidate_track.recording_mbid
                for candidate_track in candidate.tracks
                if candidate_track.recording_mbid
            }
            explicit_recording_conflict = bool(
                local.recording_mbid
                and candidate_recordings
                and local.recording_mbid not in candidate_recordings
            )
            conflict_kinds = (
                ["recording_mbid_conflict"]
                if explicit_recording_conflict
                else (pair.kinds if pair and pair.hard_conflict else [])
            )
            track_evidence.append(
                TrackEvidence(
                    local_track_id=local.local_track_id,
                    classification="contradictory" if comparable else "unknown",
                    evidence_kinds=conflict_kinds
                    or (
                        ["no_acceptable_candidate_track"]
                        if comparable
                        else ["incomparable"]
                    ),
                )
            )

        missing = [
            f"{track.disc_number}:{track.position}:{track.title}"
            for index, track in enumerate(candidate.tracks)
            if index not in used_candidates
        ]
        album_title = next(
            (track.album_title for track in local_tracks if track.album_title.strip()),
            "",
        )
        album_artist = next(
            (
                track.album_artist_name
                for track in local_tracks
                if track.album_artist_name.strip()
            ),
            "",
        )
        title_class = _album_metadata_class(album_title, candidate.album_title)
        artist_class = _album_metadata_class(album_artist, candidate.album_artist_name)
        supported = sum(item.classification == "supported" for item in track_evidence)
        comparable = sum(item.classification != "unknown" for item in track_evidence)
        contradictions = sum(
            item.classification == "contradictory" for item in track_evidence
        )
        hard_contradictions = sum(
            item.classification == "contradictory"
            and bool(_HARD_CONFLICT_KINDS.intersection(item.evidence_kinds))
            for item in track_evidence
        )
        unknown = len(track_evidence) - comparable
        unknown_limit = (
            ORDINARY_UNKNOWN_LIMIT
            if len(local_tracks) <= ORDINARY_ALBUM_MAX_FILES
            else LARGE_UNKNOWN_LIMIT
        )
        local_compilation = any(track.is_compilation for track in local_tracks)
        secondary = {value.casefold() for value in candidate.secondary_types}
        unsafe_type = (
            bool(secondary & _UNSAFE_SECONDARY_TYPES) and not local_compilation
        )

        album_costs = [
            _distance(album_title, candidate.album_title) if album_title else 0.25,
            _distance(album_artist, candidate.album_artist_name)
            if album_artist
            else 0.25,
        ]
        # Contradicted tracks count against the mean at full cost. Averaging only the
        # tracks that matched would score a candidate covering 8 of 9 identically to
        # one covering all 9, and the tie would be broken arbitrarily.
        cost_samples = len(pair_costs) + contradictions
        mean_pair_cost = (
            (sum(pair_costs) + contradictions) / cost_samples if cost_samples else 1.0
        )
        distance = 0.65 * mean_pair_cost + 0.20 * album_costs[0] + 0.15 * album_costs[1]
        supported_ratio = supported / comparable if comparable else 0.0
        reason = "SUPPORTED"
        if (
            hard_contradictions
            or title_class == "contradictory"
            or artist_class == "contradictory"
        ):
            reason = "CONFLICTING_TRACK_EVIDENCE"
        elif supported == 0:
            reason = "INSUFFICIENT_METADATA"
        elif unknown > unknown_limit:
            reason = "UNKNOWN_EXTRAS_EXCEED_LIMIT"
        elif unsafe_type:
            reason = "UNSAFE_RELEASE_TYPE"
        elif comparable == 0:
            reason = "INSUFFICIENT_METADATA"
        elif supported_ratio < SUPPORTED_RATIO_FLOOR:
            reason = "CONFLICTING_TRACK_EVIDENCE"
        elif distance > ALBUM_DISTANCE_CEILING:
            reason = "INSUFFICIENT_METADATA"

        return CandidateEvidence(
            release_group_mbid=candidate.release_group_mbid,
            release_mbid=candidate.release_mbid,
            album_title=candidate.album_title,
            album_artist_name=candidate.album_artist_name,
            artist_mbid=candidate.artist_mbid,
            release_type=candidate.release_type,
            release_date=candidate.release_date,
            local_album_title=album_title,
            local_album_artist_name=album_artist,
            album_title_classification=title_class,
            album_artist_classification=artist_class,
            track_evidence=track_evidence,
            unmatched_expected_tracks=missing,
            score=max(0.0, 1.0 - distance),
            reason_code=reason,
            matcher_version=MATCHER_VERSION,
        )

    def decide(
        self,
        local_tracks: list[GroupingTrack],
        candidates: list[AlbumCandidate],
    ) -> IdentificationDecision:
        evidence = [
            self.evaluate_candidate(local_tracks, candidate)
            for candidate in candidates[:MAX_CANDIDATES]
        ]
        eligible = sorted(
            (item for item in evidence if item.reason_code == "SUPPORTED"),
            key=lambda item: (
                -item.score,
                item.release_group_mbid,
                item.release_mbid or "",
            ),
        )
        if not evidence:
            return IdentificationDecision(
                outcome="no_candidate",
                reason_code="NO_EXTERNAL_RESULT",
                candidates=[],
            )
        if not eligible:
            reasons = {item.reason_code for item in evidence}
            if "CONFLICTING_TRACK_EVIDENCE" in reasons:
                outcome, reason = "contradictory", "CONFLICTING_TRACK_EVIDENCE"
            elif "UNKNOWN_EXTRAS_EXCEED_LIMIT" in reasons:
                outcome, reason = "insufficient_evidence", "UNKNOWN_EXTRAS_EXCEED_LIMIT"
            elif "UNSAFE_RELEASE_TYPE" in reasons:
                outcome, reason = "insufficient_evidence", "UNSAFE_RELEASE_TYPE"
            else:
                outcome, reason = "insufficient_evidence", "INSUFFICIENT_METADATA"
            return IdentificationDecision(
                outcome=outcome,
                reason_code=reason,
                candidates=evidence,
            )
        best = eligible[0]
        margin = best.score - eligible[1].score if len(eligible) > 1 else 1.0
        best.margin = margin
        if len(eligible) > 1 and margin < CANDIDATE_MARGIN_FLOOR:
            return IdentificationDecision(
                outcome="ambiguous",
                reason_code="MULTIPLE_LIKELY_RELEASES",
                candidates=evidence,
            )
        return IdentificationDecision(
            outcome="identified",
            reason_code="SUPPORTED",
            selected_candidate_key=f"{best.release_group_mbid}:{best.release_mbid or ''}",
            candidates=evidence,
        )
