"""The single conservative evidence engine for target library identification."""

from __future__ import annotations

import re
import unicodedata
from collections import Counter
from difflib import SequenceMatcher

from unidecode import unidecode

from models.identification import (
    id_only_reassignments,
    AlbumCandidate,
    CandidateEvidence,
    CandidateTrack,
    GroupingTrack,
    IdentificationDecision,
    TrackEvidence,
)
from repositories.edition_policy import evidence_key
from services.native.edition_suffix import strip_edition_suffix
from services.native.local_album_grouper import _hungarian_min
from services.native.match_scoring_core import (
    WINNER_MARGIN_FLOOR,
    LocalTrack,
    MBTrack,
    _ReleaseMeta,
    _runner_up_supported,
    score_release,
)

MATCHER_VERSION = "quiet-reconfirm-v1"
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
# M-06: filename-parsed claims count at half strength. Pair and album costs
# from `parsed` tracks are doubled, so exact parses still identify while
# tag-vs-parsed ties always favor tags. Tuned per the 1.6 procedure (start
# 0.5); pinned by `_PINNED_PARSED_DAMPENING` in
# tests/services/native/test_album_evidence_engine.py.
PARSED_DAMPENING = 0.5
_PRESENT_PROVENANCE = ("tag", "parsed")


def _dampen_parsed_cost(cost: float, provenance: str) -> float:
    """Scale a `parsed` cost contribution by the dampening factor (2.0 is the
    hard-conflict scale, so dampened costs never outrank a veto signal). The
    minimum-uncertainty floor keeps even an exact parse weaker than an exact
    tag, so tag-vs-parsed ties always favor tags."""
    if provenance != "parsed":
        return cost
    return min(
        2.0, max(cost / PARSED_DAMPENING, (1.0 - PARSED_DAMPENING) * 0.25)
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


def _unanimous_embedded_release(
    candidate: AlbumCandidate, local_tracks: list[GroupingTrack]
) -> bool:
    """Every local file embeds this very release, and no file names another release group."""

    if not candidate.release_mbid or not local_tracks:
        return False
    release = candidate.release_mbid.casefold()
    group = candidate.release_group_mbid.casefold()
    return all(
        track.release_mbid
        and track.release_mbid.casefold() == release
        and (not track.release_group_mbid or track.release_group_mbid.casefold() == group)
        for track in local_tracks
    )


def _reconcile_embedded_id_conflicts(
    candidate: AlbumCandidate,
    local_tracks: list[GroupingTrack],
    track_evidence: list[TrackEvidence],
    missing: list[str],
) -> bool:
    """Treat identifier-only conflicts as supported when the files unanimously name this release.

    MusicBrainz keeps separate recording and release-track entries per edition, so a file
    tagged from another edition of the same release group carries IDs that "conflict" with
    the edition its album-level tags name. When every file embeds this release, every
    conflict is identifier-only with a unique title-and-position match, and the conflicts
    are the minority, the album-level claim decides: those tracks are marked supported and
    the matching expected tracks are no longer missing. Anything else keeps its conflict.
    Returns whether any track was reconciled.
    """

    if not _unanimous_embedded_release(candidate, local_tracks):
        return False
    reassigned = id_only_reassignments(track_evidence, missing, len(local_tracks))
    if not reassigned:
        return False
    supported = sum(item.classification == "supported" for item in track_evidence)
    if supported * 2 <= len(track_evidence):
        return False
    for item in reassigned:
        item.classification = "supported"
        item.evidence_kinds = ["embedded_ids_reconciled"]
    missing.clear()
    return True


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
    elif (
        not local.recording_mbid
        and local.fingerprint_recording_mbid
        and candidate.recording_mbid
        and local.fingerprint_recording_mbid == candidate.recording_mbid
    ):
        identity_kinds.append("fingerprint_recording_mbid")
    if identity_kinds:
        if (
            (
                "recording_mbid" in identity_kinds
                or "fingerprint_recording_mbid" in identity_kinds
            )
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
    if local.title_provenance in ("placeholder", "absent"):
        # M-06: placeholder/absent titles abstain on descriptive grounds -
        # never support, never contradict. (MBID identity in `_pair` above
        # still applies: provider proof is independent of title strength.)
        return _Pair(1.0, False, ["incomparable"])
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
    cost = _dampen_parsed_cost(cost, local.title_provenance)
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


# Quiet-reconfirm: collaboration separators for the artist subset rule below.
# Word separators need boundaries so e.g. "Withers" never splits; single
# characters split anywhere (they cannot appear inside a folded token).
_ARTIST_WORD_SEPARATOR = re.compile(r"\b(?:feat|ft|featuring|with|vs)\b\.?", re.IGNORECASE)
_ARTIST_CHAR_SEPARATORS = frozenset({";", "×", "&", "+", ",", "/"})
# Authoritative per-track proof for the subset rule: embedded/stored MBIDs
# only. Fingerprint agreement emits its own kind in _pair and stays
# support-only, never authoritative proof.
_IDENTITY_TRACK_KINDS = frozenset({"release_track_mbid", "recording_mbid"})


def _artist_name_tokens(value: str) -> frozenset[str]:
    """Split a credit string into folded artist tokens (empty set when blank)."""
    cleaned = _ARTIST_WORD_SEPARATOR.sub(";", value)
    tokens = set()
    current: list[str] = []
    for character in cleaned:
        if character in _ARTIST_CHAR_SEPARATORS:
            tokens.add(_fold("".join(current)))
            current = []
        else:
            current.append(character)
    tokens.add(_fold("".join(current)))
    tokens.discard("")
    return frozenset(tokens)


def _artist_subset_match(local: str, candidate: str) -> bool:
    """True when one credit's artist set contains the other's.

    Catches "Bad Omens; Poppy" vs "Bad Omens" (and reversed/order variants)
    while disjoint credits ("Tribute Band" vs "Michael Jackson") fail.
    """
    local_tokens = _artist_name_tokens(local)
    candidate_tokens = _artist_name_tokens(candidate)
    if not local_tokens or not candidate_tokens:
        return False
    return local_tokens <= candidate_tokens or candidate_tokens <= local_tokens


def _has_full_track_mbid_proof(
    track_evidence: list[TrackEvidence], local_tracks: list[GroupingTrack]
) -> bool:
    """True when every present-claim track is MBID-supported.

    Present-claim-only like the other quorums: placeholder/absent tracks
    abstain instead of failing the gate.
    """
    present = [
        item
        for item, track in zip(track_evidence, local_tracks, strict=True)
        if track.title_provenance in _PRESENT_PROVENANCE
    ]
    if not present:
        return False
    return all(
        item.classification == "supported"
        and bool(_IDENTITY_TRACK_KINDS.intersection(item.evidence_kinds))
        for item in present
    )


def _album_artist_class(
    local: str,
    candidate: str,
    *,
    track_evidence: list[TrackEvidence],
    local_tracks: list[GroupingTrack],
) -> str:
    """Artist gate: hard distance check with a proof-gated subset escape.

    The 0.20 veto stays for genuinely different artists (tribute/wrong-artist
    noise). The escape fires only when the credits are subset-related AND
    every present track already carries MBID proof of these tracks - the
    provider proof outranks the credit-string difference. Recording-level
    proof can match across editions of the same recordings; any residual
    edition ambiguity is still policed by the margin and RG-tier rules.
    """
    base = _album_metadata_class(local, candidate)
    if base != "contradictory":
        return base
    if not _artist_subset_match(local, candidate):
        return "contradictory"
    if not _has_full_track_mbid_proof(track_evidence, local_tracks):
        return "contradictory"
    return "supported"


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
    # M-06: the quorum counts present (`tag`/`parsed`) claims only -
    # abstaining tracks are excluded from the totals, never misses.
    present_ids = {
        track.local_track_id
        for track in local_tracks
        if track.title_provenance in _PRESENT_PROVENANCE
    }
    items = [
        item
        for item in evidence.track_evidence
        if item.local_track_id in present_ids
    ]
    supported = sum(item.classification == "supported" for item in items)
    comparable = sum(item.classification != "unknown" for item in items)
    contradictions = sum(item.classification == "contradictory" for item in items)
    unknown = len(items) - comparable
    unknown_limit = (
        ORDINARY_UNKNOWN_LIMIT
        if len(present_ids) <= ORDINARY_ALBUM_MAX_FILES
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

def _lone_eligible_supported(
    local_tracks: list[GroupingTrack], best: CandidateEvidence
) -> bool:
    """Step 2.6 (N-01) lone-eligible rule: a sole eligible candidate identifies
    only on a present-claim quorum (>=2 present `tag`/`parsed` supporting
    tracks) or non-descriptive proof (any local recording/release-track MBID,
    the same bar as the 2.5 identify-gate `_has_non_descriptive_proof`).

    `placeholder`/`absent` tracks never count toward the two: they abstain
    from descriptive support, so a lone descriptive track plus stems must not
    reach the margin-1.0 default identify below.
    """
    present_ids = {
        track.local_track_id
        for track in local_tracks
        if track.title_provenance in _PRESENT_PROVENANCE
    }
    present_supported = sum(
        1
        for item in best.track_evidence
        if item.classification == "supported"
        and item.local_track_id in present_ids
    )
    if present_supported >= 2:
        return True
    return _has_local_track_mbid(local_tracks)


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
    CONFLICTING_TRACK_EVIDENCE veto. Album title stays a hard gate and the
    artist gate keeps its proof-gated subset escape narrow, so tribute-album
    noise (wrong artist) still vetoes above.
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


# Step 2.3b (M-04 lane-A migration): thin scan adapter. Projects the scan
# lane's GroupingTrack/CandidateTrack rows onto the shared core's
# LocalTrack/MBTrack shapes so lane-A margin checks run on the same geometry
# lane B scores (mirrors drop_import_service._to_local). Fields the scan lane
# never carries (year) stay None and the core skips them; only the embedded
# recording MBID projects (fingerprint MBIDs are support-only in lane A, never
# authoritative proof, so the shared margin answers the descriptive question
# both lanes price). Consumes the core - never restructures it.
_VARIOUS_ARTIST_NAMES = frozenset({"various artists", "various", "va"})


def _scan_track_to_core(track: GroupingTrack) -> LocalTrack:
    return LocalTrack(
        path=track.local_track_id,
        title=track.title or "",
        artist=track.artist_name or track.album_artist_name or "",
        album=track.album_title or "",
        track_number=track.track_number or 0,
        disc_number=track.disc_number or 1,
        duration_seconds=track.duration_seconds,
        recording_mbid=track.recording_mbid,
    )


def _scan_release_year(release_date: str | None) -> int | None:
    if not release_date:
        return None
    try:
        prefix = release_date[:4]
    except TypeError:
        return None
    if not isinstance(prefix, str) or len(prefix) != 4 or not prefix.isdigit():
        return None
    return int(prefix)


def _scan_candidate_to_core(
    local_tracks: list[GroupingTrack], candidate: AlbumCandidate
) -> tuple[_ReleaseMeta, list[MBTrack]]:
    artist = candidate.album_artist_name or ""
    return _ReleaseMeta(
        release_group_mbid=candidate.release_group_mbid,
        release_mbid=candidate.release_mbid or "",
        album_title=candidate.album_title or "",
        artist=artist,
        is_various=any(track.is_compilation for track in local_tracks)
        or artist.strip().lower() in _VARIOUS_ARTIST_NAMES,
        artist_mbid=candidate.artist_mbid,
        year=_scan_release_year(candidate.release_date),
        primary_type=(candidate.release_type or "").lower() or None,
        secondary_types=frozenset(
            secondary.lower() for secondary in candidate.secondary_types
        ),
    ), [
        MBTrack(
            title=track.title or "",
            position=track.position or 0,
            disc=track.disc_number or 1,
            absolute_position=track.absolute_position or track.position or 0,
            length_ms=int(track.duration_seconds * 1000)
            if track.duration_seconds
            else None,
            recording_mbid=track.recording_mbid,
            release_track_mbid=track.release_track_mbid,
        )
        for track in candidate.tracks
    ]


def _shared_margin_decisive(
    local_tracks: list[GroupingTrack],
    eligible: list[tuple[AlbumCandidate, CandidateEvidence]],
) -> bool:
    """The shared winner-margin rule over the scan-eligible set: the best core
    distance wins outright only with an exclusive-below 0.05 gap AND every
    runner-up mapped pair supported (title <= 0.35, pair <= 0.40 - the scan
    lane's own pair-sufficiency values, so both lanes mean the same thing by
    "supported"). Mirrors select_decisive_winner; the acceptance gate is
    deliberately NOT reused here (SUPPORTED stays the lane-A gate) - only the
    margin question is shared, which is what the lane-equivalence oracle pins.
    """
    locals_ = [_scan_track_to_core(track) for track in local_tracks]
    scored = []
    for candidate, _evidence in eligible:
        meta, mb_tracks = _scan_candidate_to_core(local_tracks, candidate)
        scored.append((meta, mb_tracks, score_release(locals_, mb_tracks, meta)))
    scored.sort(key=lambda entry: entry[2].distance)
    if len(scored) == 1:
        return True
    best = scored[0][2]
    runner_tracks, runner = scored[1][1], scored[1][2]
    if runner.distance - best.distance < WINNER_MARGIN_FLOOR:
        return False
    return _runner_up_supported(locals_, runner_tracks)


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
            # M-06: contradiction requires two present claims. Title,
            # track-number, and duration comparison need a present
            # (`tag`/`parsed`) title; embedded MBIDs are provider proof and
            # always count.
            descriptive_present = (
                local.title_provenance in _PRESENT_PROVENANCE
            )
            comparable = bool(
                local.recording_mbid
                or (
                    descriptive_present
                    and (
                        local.title.strip()
                        or local.track_number > 0
                        or local.duration_seconds is not None
                    )
                )
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
        # M-06: `placeholder`/`absent` album claims abstain from the
        # first-non-blank selection, and the quorum below counts present
        # claims only.
        album_title, album_title_provenance = next(
            (
                (track.album_title, track.album_title_provenance)
                for track in local_tracks
                if track.album_title.strip()
                and track.album_title_provenance in _PRESENT_PROVENANCE
            ),
            ("", "absent"),
        )
        album_artist, album_artist_provenance = next(
            (
                (track.album_artist_name, track.album_artist_provenance)
                for track in local_tracks
                if track.album_artist_name.strip()
                and track.album_artist_provenance in _PRESENT_PROVENANCE
            ),
            ("", "absent"),
        )
        title_class = _album_title_class(album_title, candidate.album_title)
        artist_class = _album_artist_class(
            album_artist,
            candidate.album_artist_name,
            track_evidence=track_evidence,
            local_tracks=local_tracks,
        )
        if title_class != "contradictory" and artist_class != "contradictory":
            _reconcile_embedded_id_conflicts(
                candidate, local_tracks, track_evidence, missing
            )
        present_evidence = [
            item
            for item, track in zip(track_evidence, local_tracks, strict=True)
            if track.title_provenance in _PRESENT_PROVENANCE
        ]
        supported = sum(item.classification == "supported" for item in present_evidence)
        comparable = sum(
            item.classification != "unknown" for item in present_evidence
        )
        contradictions = sum(
            item.classification == "contradictory" for item in present_evidence
        )
        unknown = len(present_evidence) - comparable
        unknown_limit = (
            ORDINARY_UNKNOWN_LIMIT
            if len(present_evidence) <= ORDINARY_ALBUM_MAX_FILES
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
            _dampen_parsed_cost(
                _distance(album_title, candidate.album_title)
                if album_title
                else 0.25,
                album_title_provenance,
            ),
            _dampen_parsed_cost(
                _distance(album_artist, candidate.album_artist_name)
                if album_artist
                else 0.25,
                album_artist_provenance,
            ),
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
                total=len(present_evidence),
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
        require_lone_quorum: bool = True,
    ) -> IdentificationDecision:
        paired = [
            (candidate, self.evaluate_candidate(local_tracks, candidate))
            for candidate in candidates[:MAX_CANDIDATES]
        ]
        evidence = [item for _, item in paired]
        eligible_pairs = sorted(
            (pair for pair in paired if pair[1].reason_code == "SUPPORTED"),
            # Step 2.4 (E-01/E-04): the signed evidence-time order
            # (score -> Official -> date -> XW -> MBID). Scored candidates
            # carry no status/country, so those terms tie and date/MBID
            # decide; margin/tier behavior below is unchanged.
            key=lambda pair: evidence_key(
                pair[1].score,
                None,
                pair[1].release_date,
                None,
                pair[1].release_mbid,
            ),
        )
        eligible = [item for _, item in eligible_pairs]
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
        # Step 2.3b (M-04): the shared margin adopts the core's winner rule
        # (exclusive-below 0.05 floor + fully-supported runner-up) alongside
        # the score margin, so both lanes refuse the same near-ties. The
        # release-type gate and the RG-consensus tier below stay lane-A-only
        # until proven in lane B - they are NOT ported to the shared core.
        if len(eligible) > 1 and (
            margin < CANDIDATE_MARGIN_FLOOR
            or not _shared_margin_decisive(local_tracks, eligible_pairs)
        ):
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
        # Step 2.6 (N-01): the margin-1.0 default above is otherwise reachable
        # by a single present-claim track, so a sole eligible candidate must
        # additionally clear the lone-eligible quorum (present support or
        # provider proof) or the album stays below quorum. Human-verified
        # callers (contribution attachment) opt out: the curator supplies
        # sufficiency and the engine keeps only contradiction detection.
        if (
            require_lone_quorum
            and len(eligible) == 1
            and not _lone_eligible_supported(local_tracks, best)
        ):
            return IdentificationDecision(
                outcome="insufficient_evidence",
                reason_code="INSUFFICIENT_METADATA",
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
            # Step 2.4 (E-01/E-04): within-cohort ranking follows the signed
            # evidence-time order (score -> Official -> date -> XW -> MBID).
            # Group-pinning is untouched: pool, cohort membership, and the
            # RG-unanimity/floor/full_recall rules below are unchanged.
            key=lambda item: evidence_key(
                item.score,
                None,
                item.release_date,
                None,
                item.release_mbid,
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
