"""The single conservative evidence engine for target library identification."""

from __future__ import annotations

import re
import unicodedata
from collections import Counter
from difflib import SequenceMatcher

from unidecode import unidecode

from models.identification import (
    AlbumCandidate,
    CandidateEvidence,
    CandidateTrack,
    GroupingTrack,
    IdentificationDecision,
    TrackEvidence,
)
from services.native.edition_suffix import strip_edition_suffix
from services.native.local_album_grouper import _hungarian_min

MATCHER_VERSION = "feedback-fixes-v2"
PAIR_COST_CEILING = 0.40
ALBUM_DISTANCE_CEILING = 0.35
CANDIDATE_MARGIN_FLOOR = 0.05
ORDINARY_ALBUM_MAX_FILES = 20
ORDINARY_UNKNOWN_LIMIT = 1
LARGE_UNKNOWN_LIMIT = 2
DURATION_GRACE_SECONDS = 10.0
DURATION_HARD_LIMIT_SECONDS = 30.0
MAX_CANDIDATES = 10
NEAR_MISS_SUPPORTED_RATIO = 0.80
NEAR_MISS_MAX_SOFT_CONTRADICTIONS = 2
HARD_CONFLICT_KINDS = frozenset(
    {
        "recording_mbid_conflict",
        "release_track_mbid_conflict",
        "duration_conflict",
        "ambiguous_release_track_identity",
    }
)

# P2 RG edition-uncertain tier (plan 6.1): consensus epsilon and score floor
# for pinning a release GROUP while the exact edition stays unclaimed. The
# 0.95/0.05 exact-write gate in edition_policy.py is untouched.
RG_CONSENSUS_EPSILON = 0.10
EDITION_UNCERTAIN_SCORE_FLOOR = 0.75
EDITION_UNCERTAIN_REASON = "EDITION_UNCERTAIN"

_NON_WORD = re.compile(r"[^\w]+", re.UNICODE)
# F-059: mirrors musicbrainz_matcher's CJK guard - transliterating CJK/Kana is
# lossy and hurts matching (D3), so those stay verbatim through the fold.
_CJK = re.compile("[\u4e00-\u9fff\u3040-\u304f\u30a0-\u30ff]")


def _fold(value: str) -> str:
    """Comparison fold aligned with the recall-side matchers (F-059): NFKD
    strips combining marks, then non-CJK text is transliterated through
    unidecode so ligatures (\u00e6/\u00f8/\u00df) compare equal to the ASCII
    forms that cleared recall, instead of degrading to review."""
    stripped = value.strip()
    if _CJK.search(stripped):
        decomposed = unicodedata.normalize("NFKD", stripped)
        without_marks = "".join(
            character
            for character in decomposed
            if not unicodedata.combining(character)
        )
    else:
        without_marks = unidecode(unicodedata.normalize("NFKD", stripped))
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


def _pair(
    local: GroupingTrack,
    candidate: CandidateTrack,
    *,
    recording_occurrences: int = 1,
    recording_position_occurrences: int = 1,
) -> _Pair:
    identity_kinds: list[str] = []
    if local.release_track_mbid and candidate.release_track_mbid:
        if local.release_track_mbid != candidate.release_track_mbid:
            return _Pair(2.0, True, ["release_track_mbid_conflict"])
        identity_kinds.append("release_track_mbid")
    if local.recording_mbid and candidate.recording_mbid:
        if local.recording_mbid != candidate.recording_mbid:
            return _Pair(2.0, True, ["recording_mbid_conflict"])
        identity_kinds.append("recording_mbid")
    if identity_kinds:
        if (
            "recording_mbid" in identity_kinds
            and "release_track_mbid" not in identity_kinds
            and recording_occurrences > 1
        ):
            position_matches = (
                local.track_number > 0
                and local.disc_number == candidate.disc_number
                and local.track_number
                in {candidate.position, candidate.absolute_position}
                and recording_position_occurrences == 1
            )
            if not position_matches:
                return _Pair(0.75, False, ["ambiguous_release_track_identity"])
            identity_kinds.append("compatible_position")
        return _Pair(0.0, False, identity_kinds)

    return _descriptive_pair(local, candidate)


def _descriptive_pair(
    local: GroupingTrack,
    candidate: CandidateTrack,
) -> _Pair:
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


def _album_title_class(local: str, candidate: str) -> str:
    """Album-title gate with edition-suffix normalization (F-MATCH-01).

    Both operands pass through the shared suffix helper before the existing
    fold/distance pipeline; the 0.20 threshold is unchanged. Applied to album
    titles only - artist names, track titles, and MBIDs never see it.
    """
    return _album_metadata_class(
        strip_edition_suffix(local),
        strip_edition_suffix(candidate),
    )


def _otherwise_supported(
    local_tracks: list[GroupingTrack], evidence: CandidateEvidence
) -> bool:
    """True when evaluate_candidate() would have said SUPPORTED without the release-type gate."""
    supported = sum(
        item.classification == "supported" for item in evidence.track_evidence
    )
    comparable = sum(
        item.classification != "unknown" for item in evidence.track_evidence
    )
    contradictions = sum(
        item.classification == "contradictory" for item in evidence.track_evidence
    )
    unknown = len(evidence.track_evidence) - comparable
    unknown_limit = (
        ORDINARY_UNKNOWN_LIMIT
        if len(local_tracks) <= ORDINARY_ALBUM_MAX_FILES
        else LARGE_UNKNOWN_LIMIT
    )
    return (
        evidence.album_title_classification != "contradictory"
        and evidence.album_artist_classification != "contradictory"
        and supported > 0
        and contradictions == 0
        and unknown <= unknown_limit
        and comparable > 0
        and supported == comparable
        and evidence.score >= 1.0 - ALBUM_DISTANCE_CEILING
    )

def _near_miss_supported(
    *,
    title_class: str,
    artist_class: str,
    supported: int,
    soft_contradictions: int,
    total: int,
    distance: float,
) -> bool:
    """D-238: one soft track miss must not veto a high-support album.

    Only descriptive misses (e.g. no_acceptable_candidate_track from a guest
    suffix or edition bonus-track delta) qualify. Provider-proof conflicts
    (MBID/duration/ambiguous) never reach here: the caller keeps those on the
    CONFLICTING_TRACK_EVIDENCE veto. Album title/artist stay hard gates so
    tribute-album noise (wrong artist) still vetoes above.
    """
    if title_class != "supported" or artist_class != "supported":
        return False
    if soft_contradictions <= 0 or soft_contradictions > NEAR_MISS_MAX_SOFT_CONTRADICTIONS:
        return False
    if total <= 0 or supported <= 0:
        return False
    if supported / total < NEAR_MISS_SUPPORTED_RATIO:
        return False
    return distance <= ALBUM_DISTANCE_CEILING


def _has_local_track_mbid(local_tracks: list[GroupingTrack]) -> bool:
    return any(
        track.recording_mbid or track.release_track_mbid for track in local_tracks
    )


def _tier_is_live_shaped(
    cohort: list[CandidateEvidence], candidates: list[AlbumCandidate]
) -> bool:
    """True when any tier-cohort edition carries a live secondary type."""
    secondary_by_key: dict[tuple[str, str], set[str]] = {}
    for candidate in candidates[:MAX_CANDIDATES]:
        secondary_by_key.setdefault(
            (candidate.release_group_mbid, (candidate.release_mbid or "").casefold()),
            {value.casefold() for value in candidate.secondary_types},
        )
    return any(
        "live"
        in secondary_by_key.get(
            (item.release_group_mbid, (item.release_mbid or "").casefold()), set()
        )
        for item in cohort
    )


def is_edition_uncertain(decision: IdentificationDecision) -> bool:
    """Tier predicate for the persistence task.

    selected_candidate_key carries RG + best-release as a ranked hint only;
    it MUST NOT be persisted as the canonical exact release_mbid.
    """
    return (
        decision.outcome == "edition_uncertain"
        and decision.reason_code == EDITION_UNCERTAIN_REASON
    )


class AlbumEvidenceEngine:
    """Assign tracks once, persist the result, and let every consumer reuse it."""

    def complete_administrator_exact_release_mapping(
        self,
        local_tracks: list[GroupingTrack],
        candidate: AlbumCandidate,
        evidence: CandidateEvidence,
    ) -> bool:
        """Fill a manual exact-release map only from complete position and duration proof."""

        if not local_tracks or len(local_tracks) != len(candidate.tracks):
            return False
        local_by_position: dict[tuple[int, int], GroupingTrack] = {}
        for track in local_tracks:
            key = (track.disc_number, track.track_number)
            if (
                track.disc_number < 1
                or track.track_number < 1
                or key in local_by_position
            ):
                return False
            local_by_position[key] = track
        candidate_by_position: dict[tuple[int, int], CandidateTrack] = {}
        for track in candidate.tracks:
            key = (track.disc_number, track.position)
            if (
                track.disc_number < 1
                or track.position < 1
                or key in candidate_by_position
                or not track.recording_mbid
                or not track.release_track_mbid
            ):
                return False
            candidate_by_position[key] = track
        evidence_kind = "administrator_exact_release_position_duration"
        matched_tracks: list[tuple[GroupingTrack, CandidateTrack]]
        if set(local_by_position) == set(candidate_by_position):
            matched_tracks = [
                (local, candidate_by_position[position])
                for position, local in local_by_position.items()
            ]
        else:
            local_by_absolute_position = {
                track.track_number: track for track in local_tracks
            }
            candidate_by_absolute_position = {
                track.absolute_position: track for track in candidate.tracks
            }
            expected_positions = set(range(1, len(local_tracks) + 1))
            if (
                any(track.disc_number != 1 for track in local_tracks)
                or set(local_by_absolute_position) != expected_positions
                or set(candidate_by_absolute_position) != expected_positions
            ):
                return False
            evidence_kind = "administrator_exact_release_absolute_position_duration"
            matched_tracks = [
                (
                    local_by_absolute_position[position],
                    candidate_by_absolute_position[position],
                )
                for position in sorted(expected_positions)
            ]
        evidence_by_id = {item.local_track_id: item for item in evidence.track_evidence}
        if set(evidence_by_id) != {track.local_track_id for track in local_tracks}:
            return False
        for local, provider_track in matched_tracks:
            if (
                local.duration_seconds is None
                or provider_track.duration_seconds is None
                or abs(local.duration_seconds - provider_track.duration_seconds)
                > DURATION_GRACE_SECONDS
            ):
                return False
        for local, provider_track in matched_tracks:
            item = evidence_by_id[local.local_track_id]
            item.evidence_kinds = list(
                dict.fromkeys(
                    [
                        *item.evidence_kinds,
                        evidence_kind,
                    ]
                )
            )
            item.candidate_track_title = provider_track.title
            item.candidate_disc_number = provider_track.disc_number
            item.candidate_track_position = provider_track.position
            item.recording_mbid = provider_track.recording_mbid
            item.release_track_mbid = provider_track.release_track_mbid
        evidence.unmatched_expected_tracks = []
        return True

    def evaluate_candidate(
        self,
        local_tracks: list[GroupingTrack],
        candidate: AlbumCandidate,
    ) -> CandidateEvidence:
        local_count = len(local_tracks)
        candidate_count = len(candidate.tracks)
        size = local_count + candidate_count
        pair_cache: dict[tuple[int, int], _Pair] = {}
        recording_counts = Counter(
            track.recording_mbid for track in candidate.tracks if track.recording_mbid
        )
        compatible_recording_positions = [
            Counter(
                candidate_track.recording_mbid
                for candidate_track in candidate.tracks
                if candidate_track.recording_mbid
                and local.track_number > 0
                and local.disc_number == candidate_track.disc_number
                and local.track_number
                in {candidate_track.position, candidate_track.absolute_position}
            )
            for local in local_tracks
        ]
        if size:
            costs = [[2_000_000] * size for _ in range(size)]
            for local_index, local in enumerate(local_tracks):
                for candidate_index, candidate_track in enumerate(candidate.tracks):
                    pair = _pair(
                        local,
                        candidate_track,
                        recording_occurrences=recording_counts[
                            candidate_track.recording_mbid
                        ],
                        recording_position_occurrences=compatible_recording_positions[
                            local_index
                        ][candidate_track.recording_mbid],
                    )
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
                        release_track_mbid=candidate_track.release_track_mbid,
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
            candidate_release_tracks = {
                candidate_track.release_track_mbid
                for candidate_track in candidate.tracks
                if candidate_track.release_track_mbid
            }
            explicit_release_track_conflict = bool(
                local.release_track_mbid
                and candidate_release_tracks
                and local.release_track_mbid not in candidate_release_tracks
            )
            conflict_kinds = []
            if explicit_release_track_conflict:
                conflict_kinds.append("release_track_mbid_conflict")
            if explicit_recording_conflict:
                conflict_kinds.append("recording_mbid_conflict")
            if not conflict_kinds:
                ambiguous_pair = next(
                    (
                        candidate_pair
                        for (row, _), candidate_pair in pair_cache.items()
                        if row == local_index
                        and "ambiguous_release_track_identity" in candidate_pair.kinds
                    ),
                    None,
                )
                if ambiguous_pair is not None:
                    conflict_kinds = ambiguous_pair.kinds
                elif pair and pair.hard_conflict:
                    conflict_kinds = pair.kinds
            proposed_tracks = []
            for candidate_track in candidate.tracks:
                descriptive = _descriptive_pair(local, candidate_track)
                exact_release_track = bool(
                    local.release_track_mbid
                    and candidate_track.release_track_mbid
                    and local.release_track_mbid == candidate_track.release_track_mbid
                )
                independently_consistent = "normalized_title" in descriptive.kinds and (
                    "compatible_position" in descriptive.kinds
                    or "compatible_duration" in descriptive.kinds
                )
                if exact_release_track or independently_consistent:
                    proposed_tracks.append(candidate_track)
            proposed_track = proposed_tracks[0] if len(proposed_tracks) == 1 else None
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
                    candidate_track_title=(
                        proposed_track.title if proposed_track is not None else None
                    ),
                    candidate_disc_number=(
                        proposed_track.disc_number
                        if proposed_track is not None
                        else None
                    ),
                    candidate_track_position=(
                        proposed_track.position if proposed_track is not None else None
                    ),
                    recording_mbid=(
                        proposed_track.recording_mbid
                        if proposed_track is not None
                        else None
                    ),
                    release_track_mbid=(
                        proposed_track.release_track_mbid
                        if proposed_track is not None
                        else None
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
        title_class = _album_title_class(album_title, candidate.album_title)
        artist_class = _album_metadata_class(album_artist, candidate.album_artist_name)
        supported = sum(item.classification == "supported" for item in track_evidence)
        comparable = sum(item.classification != "unknown" for item in track_evidence)
        contradictions = sum(
            item.classification == "contradictory" for item in track_evidence
        )
        unknown = len(track_evidence) - comparable
        unknown_limit = (
            ORDINARY_UNKNOWN_LIMIT
            if len(local_tracks) <= ORDINARY_ALBUM_MAX_FILES
            else LARGE_UNKNOWN_LIMIT
        )
        local_compilation = any(track.is_compilation for track in local_tracks)
        secondary = {value.casefold() for value in candidate.secondary_types}
        release_type_requires_confirmation = "live" in secondary or (
            "compilation" in secondary and not local_compilation
        )
        exact_release_track_proof = bool(
            candidate.release_mbid
            and local_tracks
            and all(track.release_track_mbid for track in local_tracks)
            and len({track.release_track_mbid for track in local_tracks})
            == len(local_tracks)
            and len(track_evidence) == len(local_tracks)
            and all(
                item.classification == "supported"
                and "release_track_mbid" in item.evidence_kinds
                for item in track_evidence
            )
        )
        album_costs = [
            _distance(album_title, candidate.album_title) if album_title else 0.25,
            _distance(album_artist, candidate.album_artist_name)
            if album_artist
            else 0.25,
        ]
        mean_pair_cost = sum(pair_costs) / len(pair_costs) if pair_costs else 1.0
        distance = 0.65 * mean_pair_cost + 0.20 * album_costs[0] + 0.15 * album_costs[1]
        reason = "SUPPORTED"
        hard_contradictions = sum(
            1
            for item in track_evidence
            if item.classification == "contradictory"
            and any(kind in HARD_CONFLICT_KINDS for kind in item.evidence_kinds)
        )
        soft_contradictions = contradictions - hard_contradictions
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
        elif release_type_requires_confirmation and not exact_release_track_proof:
            reason = "RELEASE_TYPE_REQUIRES_CONFIRMATION"
        elif comparable == 0 or supported != comparable:
            if _near_miss_supported(
                title_class=title_class,
                artist_class=artist_class,
                supported=supported,
                soft_contradictions=soft_contradictions,
                total=len(track_evidence),
                distance=distance,
            ):
                reason = "SUPPORTED"
            else:
                reason = "INSUFFICIENT_METADATA"
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
        full_recall: bool = False,
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
            elif "RELEASE_TYPE_REQUIRES_CONFIRMATION" in reasons:
                tier = self._rg_consensus_tier(
                    local_tracks, evidence, candidates, full_recall=full_recall
                )
                if tier is not None:
                    return tier
                outcome, reason = (
                    "insufficient_evidence",
                    "RELEASE_TYPE_REQUIRES_CONFIRMATION",
                )
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
            tier = self._rg_consensus_tier(
                local_tracks, evidence, candidates, full_recall=full_recall
            )
            if tier is not None:
                return tier
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

    def _rg_consensus_tier(
        self,
        local_tracks: list[GroupingTrack],
        evidence: list[CandidateEvidence],
        candidates: list[AlbumCandidate],
        full_recall: bool = False,
    ) -> IdentificationDecision | None:
        """Pin the release GROUP when same-album editions disagree on pressing.

        Tier pool is SUPPORTED plus RELEASE_TYPE-blocked-but-otherwise-supported
        (exact-write rule untouched: edition_policy.py keeps 0.95/0.05, reason
        set, recall_key, and TIE handling). Pins only when every candidate in
        the top cohort (best score, within RG_CONSENSUS_EPSILON) shares one
        release group and the best clears EDITION_UNCERTAIN_SCORE_FLOOR; a lone
        RELEASE_TYPE candidate additionally needs full_recall (partial-recall
        singletons never pin: vacuous consensus). Cross-RG cohorts, outscored
        pools, and uncorroborated live shapes return None for existing verdicts.
        """
        pool = [
            item
            for item in evidence
            if item.reason_code == "SUPPORTED"
            or (
                item.reason_code == "RELEASE_TYPE_REQUIRES_CONFIRMATION"
                and _otherwise_supported(local_tracks, item)
            )
        ]
        if not pool:
            return None
        ranked = sorted(
            pool,
            key=lambda item: (
                -item.score,
                item.release_group_mbid,
                item.release_mbid or "",
            ),
        )
        best = ranked[0]
        if best.score < EDITION_UNCERTAIN_SCORE_FLOOR:
            return None
        if any(item.score > best.score for item in evidence):
            return None
        cohort = [
            item for item in ranked if best.score - item.score <= RG_CONSENSUS_EPSILON
        ]
        if any(item.release_group_mbid != best.release_group_mbid for item in cohort):
            return None
        if len(cohort) < 2:
            if cohort[0].reason_code != "RELEASE_TYPE_REQUIRES_CONFIRMATION":
                return None
            if not full_recall:
                return None
        if _tier_is_live_shaped(cohort, candidates) and not (
            _has_local_track_mbid(local_tracks)
            or best.score >= EDITION_UNCERTAIN_SCORE_FLOOR
        ):
            return None
        release_group_mbid = best.release_group_mbid
        return IdentificationDecision(
            outcome="edition_uncertain",
            reason_code=EDITION_UNCERTAIN_REASON,
            selected_candidate_key=f"{release_group_mbid}:{best.release_mbid or ''}",
            candidates=evidence,
            edition_uncertain=True,
            release_group_mbid=release_group_mbid,
            ranked_edition_keys=[
                f"{item.release_group_mbid}:{item.release_mbid or ''}"
                for item in cohort
            ],
        )
