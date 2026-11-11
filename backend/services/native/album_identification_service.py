"""Target-only orchestration for one durable album-identification attempt."""

from __future__ import annotations

import asyncio
import logging
from infrastructure.observability.provider_counters import ProviderWorkload, provider_workload_scope
from collections.abc import Sequence

import time
import uuid
from collections.abc import Awaitable, Callable, Sequence
from pathlib import Path

from repositories.musicbrainz_base import capture_mb_source_context
from infrastructure.degradation import (
    clear_degradation_context,
    init_degradation_context,
)
from infrastructure.resilience.retry import CircuitOpenError
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.identification import (
    AlbumCandidate,
    CandidateEvidence,
    GroupingTrack,
    IdentificationAttempt,
    IdentificationDecision,
    IdentificationEvidenceRecord,
    TrackEvidence,
)
from services.native.album_candidate_service import (
    RECALL_SOURCE_KINDS,
    AlbumCandidateService,
)
from services.native.album_evidence_engine import (
    EDITION_UNCERTAIN_REASON,
    MATCHER_VERSION,
    AlbumEvidenceEngine,
    is_edition_uncertain,
)
from infrastructure.observability.library_metrics import LibraryMetrics
from core.exceptions import ExternalServiceError, StaleRevisionError
from infrastructure.queue.priority_queue import RequestPriority
from repositories.edition_policy import (
    AUTO_ACCEPT_EVIDENCE_REASONS,
    auto_accept_decision,
    evidence_key,
)
from repositories.protocols.musicbrainz_management import (
    CanonicalMusicBrainzRepositoryProtocol,
    MbManagementRelease,
)
from services.native.conditional_fingerprint_service import (
    FINGERPRINTER_VERSION,
    ConditionalFingerprintService,
)
from services.native.identification_queue_service import (
    LEASE_SECONDS,
    IdentificationQueueService,
)
from services.native.identification_revisions import (
    album_identity_revision,
    album_input_revisions,
)
from services.native.track_provenance import (
    derive_album_provenance,
    derive_title_artist_provenance,
    parse_names_for_row,
    resolve_provenance,
    track_stem,
)

CacheInvalidator = Callable[[set[str]], Awaitable[None]]
# ST1: scoped hooks also receive the local album ids whose commits triggered
# the invalidation, so providers can resolve entity ids post-commit.
ScopedCacheInvalidator = (
    Callable[[set[str]], Awaitable[None]]
    | Callable[[set[str], Sequence[str] | None], Awaitable[None]]
)
PostIdentificationCallback = Callable[[str, str], Awaitable[object]]
MAX_NEW_FINGERPRINTS_PER_ATTEMPT = 2

logger = logging.getLogger(__name__)

_edition_hold_metrics = LibraryMetrics.for_library_workload()


def _valid_mbid(value: str | None) -> bool:
    if not value:
        return False
    try:
        uuid.UUID(value)
    except ValueError:
        return False
    return True


def _candidate_key(evidence: CandidateEvidence) -> str:
    return f"{evidence.release_group_mbid}:{evidence.release_mbid or ''}"


# Step 2.5 (E-02): bounded canonical I/O for the identify-lane gate - only
# the final top-N by score are ever fetched, mirroring the repair lane.
_IDENTIFY_GATE_TOP_N = 3


def _candidate_completely_maps(
    indexed_ids: set[str], evidence: CandidateEvidence
) -> bool:
    """Service-side mirror of the store's complete-mapping precondition.

    The winner must map every indexed track to a distinct release track with
    recording + release-track MBIDs and positions (repair shape at
    ``identity_repair_service.py:762-765`` via ``_complete_track_identity_mapping``).
    """
    if not evidence.release_mbid:
        return False
    seen: dict[str, TrackEvidence] = {}
    release_track_ids: set[str] = set()
    for item in evidence.track_evidence:
        if item.local_track_id in seen:
            return False
        if (
            not item.recording_mbid
            or not item.release_track_mbid
            or not item.candidate_disc_number
            or not item.candidate_track_position
            or item.release_track_mbid in release_track_ids
        ):
            return False
        seen[item.local_track_id] = item
        release_track_ids.add(item.release_track_mbid)
    return set(seen) == set(indexed_ids)


def _has_non_descriptive_proof(tracks: list[GroupingTrack]) -> bool:
    """Lone-ranked accepts need provider-issued proof beyond description.

    Any local recording or release-track MBID (embedded tag or stored
    identity) counts - the same bar as the W6 lone-eligible rule.
    """
    return any(
        track.recording_mbid or track.release_track_mbid for track in tracks
    )


def _to_grouping_track(row: dict) -> GroupingTrack:
    relative_path = str(row["relative_path"])
    stem = track_stem(relative_path)
    title = str(row["title"] or "")
    artist_name = str(row["artist_name"] or "")
    album_title = str(row["album_title"] or "")
    album_artist_name = str(row["album_artist_name"] or "")
    title_provenance = resolve_provenance(
        row.get("title_provenance"),
        derive_title_artist_provenance(title, artist_name, stem),
    )
    album_title_provenance = resolve_provenance(
        row.get("album_title_provenance"),
        derive_album_provenance(row.get("tag_album_title"), album_title),
    )
    album_artist_provenance = resolve_provenance(
        row.get("album_artist_provenance"),
        derive_album_provenance(
            row.get("tag_album_artist_name"), album_artist_name
        ),
    )
    track_number = int(row["track_number"] or 0)
    if (
        not title
        and title_provenance == "absent"
        and album_title_provenance == "absent"
        and album_artist_provenance == "absent"
    ):
        # M-01 backfill for rows with no display values at all (pre-indexer-
        # fallback vintages and harness rows): parse against the folded
        # parent, mirroring `_prepare_tagged`. Non-empty displays keep their
        # derived provenance above - never re-parse those. A parse that
        # strips nothing (title/album == stem) is not evidence, matching the
        # indexer and the derivation heuristic.
        parsed = parse_names_for_row(relative_path)
        parsed_title = (
            parsed.title
            if parsed.title and parsed.title != stem
            else None
        )
        if parsed_title:
            title, title_provenance = parsed_title, "parsed"
        if parsed.artist:
            artist_name = parsed.artist
            if title and title_provenance == "absent":
                title_provenance = "parsed"
        if parsed.album and parsed.album != stem and not album_title:
            album_title, album_title_provenance = parsed.album, "parsed"
        if not album_artist_name and parsed.artist:
            album_artist_name, album_artist_provenance = (
                parsed.artist,
                "parsed",
            )
        if not track_number and parsed.track_number:
            track_number = parsed.track_number
    return GroupingTrack(
        local_track_id=str(row["id"]),
        root_id=str(row["root_id"]),
        relative_path=relative_path,
        title=title,
        artist_name=artist_name,
        album_title=album_title,
        album_artist_name=album_artist_name,
        title_provenance=title_provenance,
        album_title_provenance=album_title_provenance,
        album_artist_provenance=album_artist_provenance,
        artist_sort_name=row["artist_sort"],
        album_artist_sort_name=row["album_artist_sort"],
        track_number=track_number,
        disc_number=int(row["disc_number"] or 1),
        duration_seconds=row["duration_seconds"],
        recording_mbid=row["embedded_recording_mbid"] or row.get("recording_mbid"),
        release_mbid=row["embedded_release_mbid"],
        release_group_mbid=row["embedded_release_group_mbid"],
        release_track_mbid=(
            row["embedded_release_track_mbid"] or row.get("release_track_mbid")
        ),
        is_compilation=bool(row["is_compilation"]),
        tags_readable=not bool(row["metadata_incomplete"]),
        membership_locked=bool(row["membership_locked"]),
        current_album_id=str(row["local_album_id"]),
    )


def _embedded_decision(
    tracks: list[GroupingTrack], raw_tracks: list[dict]
) -> IdentificationDecision | None:
    embedded_values = [
        value
        for row in raw_tracks
        for value in (
            row["embedded_release_group_mbid"],
            row["embedded_release_mbid"],
            row["embedded_recording_mbid"],
            row["embedded_release_track_mbid"],
            row["embedded_artist_mbid"],
            row["embedded_album_artist_mbid"],
        )
        if value
    ]
    if any(not _valid_mbid(str(value)) for value in embedded_values):
        return IdentificationDecision(
            outcome="contradictory",
            reason_code="INVALID_EMBEDDED_IDS",
        )
    groups = {track.release_group_mbid for track in tracks if track.release_group_mbid}
    releases = {track.release_mbid for track in tracks if track.release_mbid}
    artist_ids = {
        str(row["embedded_album_artist_mbid"])
        for row in raw_tracks
        if row["embedded_album_artist_mbid"]
    }
    recordings = [
        str(row["embedded_recording_mbid"])
        for row in raw_tracks
        if row["embedded_recording_mbid"]
    ]
    release_tracks = [
        str(row["embedded_release_track_mbid"])
        for row in raw_tracks
        if row["embedded_release_track_mbid"]
    ]
    if (
        len(groups) > 1
        or len(releases) > 1
        or len(artist_ids) > 1
        or len(recordings) != len(set(recordings))
        or len(release_tracks) != len(set(release_tracks))
    ):
        return IdentificationDecision(
            outcome="contradictory",
            reason_code="CONFLICTING_EMBEDDED_IDS",
        )
    if not groups:
        return None
    group_id = next(iter(groups))
    release_id = next(iter(releases), None)
    evidence = CandidateEvidence(
        release_group_mbid=group_id,
        release_mbid=release_id,
        album_title=tracks[0].album_title if tracks else "",
        album_artist_name=tracks[0].album_artist_name if tracks else "",
        artist_mbid=next(iter(artist_ids)) if len(artist_ids) == 1 else None,
        local_album_title=tracks[0].album_title if tracks else "",
        local_album_artist_name=tracks[0].album_artist_name if tracks else "",
        album_title_classification="supported",
        album_artist_classification="supported",
        track_evidence=[
            TrackEvidence(
                local_track_id=track.local_track_id,
                classification=(
                    "supported" if row["embedded_recording_mbid"] else "unknown"
                ),
                evidence_kinds=(
                    [
                        "embedded_recording_mbid",
                        *(
                            ["embedded_release_track_mbid"]
                            if row["embedded_release_track_mbid"]
                            else []
                        ),
                    ]
                    if row["embedded_recording_mbid"]
                    else ["embedded_album_identity_only"]
                ),
                recording_mbid=row["embedded_recording_mbid"],
                release_track_mbid=row["embedded_release_track_mbid"],
                candidate_disc_number=track.disc_number,
                candidate_track_position=track.track_number,
            )
            for track, row in zip(tracks, raw_tracks, strict=True)
        ],
        score=1.0,
        margin=1.0,
        reason_code="SUPPORTED_EMBEDDED_IDS",
        matcher_version=MATCHER_VERSION,
    )
    return IdentificationDecision(
        outcome="identified",
        reason_code="SUPPORTED_EMBEDDED_IDS",
        selected_candidate_key=_candidate_key(evidence),
        candidates=[evidence],
    )


def _stored_track_identity_decision(
    raw_tracks: list[dict],
) -> IdentificationDecision | None:
    for row in raw_tracks:
        for embedded_key, current_key in (
            ("embedded_recording_mbid", "recording_mbid"),
            ("embedded_release_track_mbid", "release_track_mbid"),
        ):
            embedded = row[embedded_key]
            current = row[current_key]
            if embedded and current and embedded != current:
                return IdentificationDecision(
                    outcome="contradictory",
                    reason_code="CONFLICTING_EMBEDDED_IDS",
                )
    return None


def _embedded_release_decision(
    tracks: list[GroupingTrack],
) -> IdentificationDecision | None:
    release_ids = [track.release_mbid for track in tracks]
    populated = [release_id for release_id in release_ids if release_id]
    if not populated:
        return None
    if len(populated) != len(release_ids):
        return IdentificationDecision(
            outcome="insufficient_evidence",
            reason_code="INCOMPLETE_EMBEDDED_RELEASE_IDS",
        )
    if len(set(populated)) != 1:
        return IdentificationDecision(
            outcome="contradictory",
            reason_code="CONFLICTING_EMBEDDED_IDS",
        )
    return None


def _populated_releases_agree(tracks: list[GroupingTrack]) -> bool:
    """True when every populated embedded release MBID names one release.

    Blank tracks abstain (matching the release-group seed and the recall
    gate, which both count populated claims only); genuine disagreement
    still returns False.
    """
    populated = {
        str(track.release_mbid).casefold() for track in tracks if track.release_mbid
    }
    return len(populated) == 1


def _enforce_existing_album_identity(
    decision: IdentificationDecision,
    identity: dict | None,
    raw_tracks: list[dict],
) -> None:
    if identity is None or identity["decision_source"] not in {
        "manual",
        "legacy_import",
    }:
        return
    current_group = str(identity["release_group_mbid"] or "")
    current_release = str(identity["release_mbid"] or "")
    embedded_releases = {
        str(row["embedded_release_mbid"])
        for row in raw_tracks
        if row["embedded_release_mbid"]
    }
    provider_canonicalized_current_release = (
        bool(current_release)
        and len(embedded_releases) == 1
        and current_release in embedded_releases
        and all(row["embedded_release_mbid"] for row in raw_tracks)
    )
    selected_conflicts = False
    for candidate in decision.candidates:
        conflicts = bool(current_group) and (
            candidate.release_group_mbid != current_group
        )
        if current_release and candidate.release_mbid != current_release:
            conflicts = conflicts or not provider_canonicalized_current_release
        if not conflicts:
            continue
        candidate.reason_code = "CONFLICTING_TRACK_EVIDENCE"
        selected_conflicts = (
            selected_conflicts
            or decision.selected_candidate_key == _candidate_key(candidate)
        )
    if selected_conflicts:
        decision.outcome = "contradictory"
        decision.reason_code = "MANUAL_IDENTITY_STALE"
        decision.selected_candidate_key = None


def _enforce_raw_track_identities(
    decision: IdentificationDecision,
    raw_tracks: list[dict],
) -> None:
    rows = {str(row["id"]): row for row in raw_tracks}
    embedded_groups = {
        str(row["embedded_release_group_mbid"])
        for row in raw_tracks
        if row["embedded_release_group_mbid"]
    }
    selected_conflicts = False
    for candidate in decision.candidates:
        candidate_conflicts = bool(
            embedded_groups and embedded_groups != {candidate.release_group_mbid}
        )
        for item in candidate.track_evidence:
            row = rows.get(item.local_track_id)
            if row is None or "embedded_ids_reconciled" in item.evidence_kinds:
                continue
            recording_values = {
                str(value)
                for value in (
                    row["embedded_recording_mbid"],
                    row["recording_mbid"],
                )
                if value
            }
            accepted_recordings = {
                str(value)
                for value in (item.recording_mbid, *item.recording_mbid_redirects)
                if value
            }
            release_track_values = {
                str(value)
                for value in (
                    row["embedded_release_track_mbid"],
                    row["release_track_mbid"],
                )
                if value
            }
            conflict_kinds: list[str] = []
            ambiguous_occurrence = (
                "ambiguous_release_track_identity" in item.evidence_kinds
            )
            if (
                recording_values
                and accepted_recordings
                and not ambiguous_occurrence
                and not recording_values.issubset(accepted_recordings)
            ):
                conflict_kinds.append("recording_mbid_conflict")
            if release_track_values and (
                item.release_track_mbid is None
                or release_track_values != {item.release_track_mbid}
            ):
                conflict_kinds.append("release_track_mbid_conflict")
            if not conflict_kinds:
                continue
            item.classification = "contradictory"
            item.evidence_kinds = list(
                dict.fromkeys([*item.evidence_kinds, *conflict_kinds])
            )
            candidate_conflicts = True
        if not candidate_conflicts:
            continue
        candidate.reason_code = "CONFLICTING_TRACK_EVIDENCE"
        selected_conflicts = (
            selected_conflicts
            or decision.selected_candidate_key == _candidate_key(candidate)
        )
    if selected_conflicts:
        decision.outcome = "contradictory"
        decision.reason_code = "CONFLICTING_TRACK_EVIDENCE"
        decision.selected_candidate_key = None


_SIBLING_TRIAL_OUTCOMES = ("ambiguous", "insufficient_evidence", "edition_uncertain")


def _sibling_trial_release_groups(
    decision: IdentificationDecision,
    recalled: list[AlbumCandidate],
) -> list[str]:
    """EditionsEtc Phase 2 within-group sibling trial derivation.

    A group qualifies when its best evidence in this decision is still not
    SUPPORTED (the recalled edition failed to back the album), its
    candidates carry at most one distinct release MBID (no sibling edition
    was present yet), and the attempt actually recalled the group through
    the bounded search path - never an exact-release fast path, whose
    identity is pinned rather than evidence-ranked. Empty output means the
    owner-approved budget of at most one extra full-release fetch per
    qualifying group is spent nowhere.
    """
    best_by_group: dict[str, CandidateEvidence] = {}
    editions_by_group: dict[str, set[str]] = {}
    for item in decision.candidates:
        best = best_by_group.get(item.release_group_mbid)
        if best is None or item.score > best.score:
            best_by_group[item.release_group_mbid] = item
        if item.release_mbid:
            editions_by_group.setdefault(item.release_group_mbid, set()).add(
                item.release_mbid.casefold()
            )
    recalled_groups = {
        candidate.release_group_mbid
        for candidate in recalled
        if RECALL_SOURCE_KINDS.intersection(candidate.source_kinds)
    }
    return [
        group
        for group, best in best_by_group.items()
        if best.reason_code != "SUPPORTED"
        and len(editions_by_group.get(group, ())) <= 1
        and group in recalled_groups
    ]


# F-IDENT-02: deterministic payload-shape failures defer under this stable
# code instead of PROVIDER_TEMPORARILY_UNAVAILABLE. The spelling is part of
# the persisted contract (last_failure_code / attention_cause) and the API/UI.
UNMAPPABLE_PROVIDER_PAYLOAD = "UNMAPPABLE_PROVIDER_PAYLOAD"


class AlbumIdentificationService:
    def __init__(
        self,
        store: NativeLibraryStore,
        queue: IdentificationQueueService,
        candidates: AlbumCandidateService,
        evidence_engine: AlbumEvidenceEngine,
        fingerprints: ConditionalFingerprintService,
        invalidate: ScopedCacheInvalidator | None = None,
        on_identified: PostIdentificationCallback | None = None,
        provider_available: Callable[[], bool] | None = None,
        canonical_provider: CanonicalMusicBrainzRepositoryProtocol | None = None,
        edition_opt_in: Callable[[str], bool] | None = None,
    ) -> None:
        self._store = store
        self._queue = queue
        self._candidates = candidates
        self._evidence_engine = evidence_engine
        self._fingerprints = fingerprints
        self._invalidate = invalidate
        self._on_identified = on_identified
        self._provider_available = provider_available
        # Step 2.5 (E-02): the same canonical handle the repair lane uses,
        # plus the profile-level opt-in resolver. None keeps the pre-auto
        # behavior byte-for-byte (gate off everywhere).
        self._canonical_provider = canonical_provider
        self._edition_opt_in = edition_opt_in

    async def _evaluate_edition_gate(
        self,
        *,
        job: dict,
        worker_id: str,
        decision: IdentificationDecision,
        tracks: list[GroupingTrack],
        raw_tracks: list[dict],
        timestamp: float,
    ) -> tuple[str, CandidateEvidence | None, list[dict[str, object]]]:
        """Step 2.5 (E-02) identify-lane auto-accept gate (D-EDITION-AUTO).

        Runs pre-persist, after both backstops and the transient-defer block.
        Returns ``(disposition, winner, ranking)`` where disposition is
        ``"legacy"`` (gate off or nothing exact to gate - persist as today),
        ``"accept"`` (winner sealed via the accept tx in ``finish``), a
        ``"veto:<reason>"`` (decision demoted in place to RG-only +
        ``edition_to_confirm``), or ``"deferred"`` (canonical fetch failed -
        the job was deferred under ``PROVIDER_TEMPORARILY_UNAVAILABLE``,
        never review).
        """
        album_id = str(job["local_album_id"])
        if self._edition_opt_in is None or self._canonical_provider is None:
            logger.debug(
                "identify edition gate: legacy (gate unwired) album=%s",
                album_id,
            )
            return "legacy", None, []
        root_ids = {track.root_id for track in tracks}
        if len(root_ids) != 1 or not self._edition_opt_in(next(iter(root_ids))):
            # Single-root albums only (repair :796-801): multi-root albums
            # stay legacy even when every root opted in.
            logger.debug(
                "identify edition gate: legacy (opt-in off or multi-root=%s) album=%s",
                sorted(root_ids),
                album_id,
            )
            return "legacy", None, []
        if decision.outcome != "identified" or not decision.selected_candidate_key:
            logger.debug(
                "identify edition gate: legacy (outcome=%s) album=%s",
                decision.outcome,
                album_id,
            )
            return "legacy", None, []
        selected = next(
            (
                candidate
                for candidate in decision.candidates
                if _candidate_key(candidate) == decision.selected_candidate_key
                and candidate.release_mbid
            ),
            None,
        )
        if selected is None:
            logger.debug(
                "identify edition gate: legacy (no selectable winner) album=%s",
                album_id,
            )
            return "legacy", None, []
        if any(
            str(row.get("track_identity_source")) in ("manual", "legacy_import")
            for row in raw_tracks
        ):
            return self._veto_edition_gate(
                decision,
                selected,
                [],
                "PROTECTED_TRACKS",
                album_id,
                {"reason": "manual/legacy track rows present"},
            )
        top = sorted(
            decision.candidates,
            key=lambda item: evidence_key(
                float(item.score),
                None,
                item.release_date,
                None,
                str(item.release_mbid or ""),
            ),
        )[:_IDENTIFY_GATE_TOP_N]
        indexed_ids = {str(row["id"]) for row in raw_tracks}
        fetched: list[tuple[CandidateEvidence, MbManagementRelease]] = []
        for candidate in top:
            if not candidate.release_mbid:
                continue
            try:
                release = await self._canonical_provider.get_canonical_release(
                    str(candidate.release_mbid),
                    includes=("media",),
                    priority=RequestPriority.BACKGROUND_SYNC,
                )
            except (ExternalServiceError, CircuitOpenError) as error:
                logger.debug(
                    "identify edition gate: defer (canonical fetch failed) album=%s release=%s",
                    album_id,
                    candidate.release_mbid,
                )
                await self._queue.defer(
                    job,
                    worker_id,
                    "PROVIDER_TEMPORARILY_UNAVAILABLE",
                    now=timestamp,
                    retry_after_seconds=getattr(error, "retry_after_seconds", None),
                )
                return "deferred", None, []
            if release is None:
                # Unfetchable candidates are skipped from auto-accept, not failed.
                logger.debug(
                    "identify edition gate: candidate unfetchable, skipped album=%s release=%s",
                    album_id,
                    candidate.release_mbid,
                )
                continue
            fetched.append((candidate, release))
        pool = [
            (candidate, release)
            for candidate, release in fetched
            if candidate.reason_code in AUTO_ACCEPT_EVIDENCE_REASONS
            and (candidate.release_group_mbid or "").casefold()
            == (selected.release_group_mbid or "").casefold()
            and _candidate_completely_maps(indexed_ids, candidate)
        ]
        ranked = sorted(
            (
                (
                    evidence_key(
                        float(candidate.score),
                        release.status,
                        release.date,
                        release.country,
                        str(candidate.release_mbid),
                    ),
                    float(candidate.score),
                    candidate,
                )
                for candidate, release in pool
            ),
            key=lambda item: item[0],
        )
        ranking = [
            {
                "key": list(key),
                "score": score,
                "release_mbid": str(candidate.release_mbid),
            }
            for key, score, candidate in ranked
        ]
        gate_ok, gate_reason = auto_accept_decision(
            [(key, score) for key, score, _ in ranked]
        )
        winner = ranked[0][2] if ranked else None
        detail: dict[str, object] = {
            "scores": [score for _, score, _ in ranked],
            "releases": [str(item.release_mbid) for _, _, item in ranked],
            "reason": gate_reason,
        }
        if gate_ok and winner is not None and str(
            winner.release_mbid
        ) != str(selected.release_mbid):
            # The canonical tie-breaks reordered past the decided edition:
            # never seal a different release than the decision selected.
            gate_ok, gate_reason = False, "WINNER_MISMATCH"
            detail["reason"] = gate_reason
        if (
            gate_ok
            and len(ranked) == 1
            and not _has_non_descriptive_proof(tracks)
        ):
            gate_ok, gate_reason = False, "LONE_WITHOUT_PROOF"
            detail["reason"] = gate_reason
        if not gate_ok or winner is None:
            return self._veto_edition_gate(
                decision, selected, ranking, gate_reason, album_id, detail
            )
        logger.debug(
            "identify edition gate: accept album=%s release=%s scores=%s",
            album_id,
            winner.release_mbid,
            detail["scores"],
            extra={"album_id": album_id, "gate": "AUTO_ACCEPT", **detail},
        )
        return "accept", winner, ranking

    @staticmethod
    def _veto_edition_gate(
        decision: IdentificationDecision,
        selected: CandidateEvidence,
        ranking: list[dict[str, object]],
        reason: str,
        album_id: str,
        detail: dict[str, object],
    ) -> tuple[str, None, list[dict[str, object]]]:
        """Demote a vetoed identified decision to RG-only + edition_to_confirm.

        Reuses the P2 tier persist shape: the release group pins, the exact
        edition stays unproven, and the review row carries the ranked keys.
        """
        logger.debug(
            "identify edition gate: veto album=%s reason=%s detail=%s",
            album_id,
            reason,
            detail,
            extra={"album_id": album_id, "gate": reason, **detail},
        )
        selected_key = f"{selected.release_group_mbid}:{selected.release_mbid or ''}"
        decision.outcome = "edition_uncertain"
        decision.reason_code = EDITION_UNCERTAIN_REASON
        decision.selected_candidate_key = selected_key
        decision.edition_uncertain = True
        decision.release_group_mbid = selected.release_group_mbid
        decision.ranked_edition_keys = [
            f"{selected.release_group_mbid}:{item['release_mbid']}"
            for item in ranking
        ] or [selected_key]
        return f"veto:{reason}", None, ranking

    @provider_workload_scope(ProviderWorkload.IDENTITY)
    async def run_claimed_job(
        self,
        job: dict,
        worker_id: str,
        *,
        now: float | None = None,
    ) -> str:
        timestamp = time.time() if now is None else now
        operation_source_context = capture_mb_source_context()
        context = await self._store.get_album_identification_context(
            str(job["local_album_id"])
        )
        if context is None:
            # The album row is gone or retired: durable catalog state, so fail
            # terminally (auditable) instead of deferring until the cap.
            await self._queue.fail(
                job, worker_id, "SUBJECT_NOT_AVAILABLE", now=timestamp
            )
            return "attention"
        raw_tracks: list[dict] = [
            row for row in context["tracks"] if row["availability"] == "indexed"
        ]
        if not raw_tracks:
            await self._queue.defer(
                job, worker_id, "SUBJECT_NOT_AVAILABLE", now=timestamp
            )
            return "provider_deferred"
        identity_revision = album_identity_revision(context["identity"], raw_tracks)
        tracks = [_to_grouping_track(row) for row in raw_tracks]
        degradation = init_degradation_context()
        # P1-A: snapshot before any provider call so later delta isolates
        # failures recorded during this attempt.
        degraded_before = set(degradation.degraded_summary())
        deterministic_before = set(degradation.deterministic_sources())

        def _full_recall() -> bool:
            # P2 tier gate: a lone RELEASE_TYPE-blocked edition pins its RG only
            # on proven full recall, i.e. an empty P1 transient delta since the
            # snapshots above (no transient provider gap during this attempt).
            degraded_delta = set(degradation.degraded_summary()) - degraded_before
            transient_delta = {
                source
                for source in degraded_delta
                if source not in degradation.deterministic_sources()
            }
            return not transient_delta

        decision: IdentificationDecision | None = None

        async def checkpoint() -> bool:
            # F-057: renew the 60s claim lease on every phase checkpoint so a
            # long recall + fpcalc pass cannot outlive the lease if a second
            # claimant ever appears; failures stay inert while the
            # single-claimer invariant holds (finish matches owner+revision).
            try:
                fresh_revision = await self._store.heartbeat_identification_job(
                    str(job["id"]),
                    worker_id,
                    now=time.time(),
                    lease_seconds=LEASE_SECONDS,
                )
                if fresh_revision is not None:
                    job["row_revision"] = fresh_revision
            except Exception:  # noqa: BLE001 - heartbeat must never fail a run
                logger.debug("Identification lease heartbeat failed", exc_info=True)
            return not await self._queue.is_paused()

        try:
            local_metadata_only = all(
                row["applied_policy"] == "local_metadata" for row in raw_tracks
            )
            release_decision = _embedded_release_decision(tracks)
            decision = _stored_track_identity_decision(raw_tracks)
            # Partial-unanimous embedded release tags name one release while
            # the remaining tracks merely lack tags (no conflict): attempt
            # the agreed release through recall instead of holding. Scoring,
            # the lone quorum, and the edition gate still validate it.
            # Local-metadata albums keep the hold - with no provider to
            # fetch and check the agreed release, the gap is unresolvable.
            attempt_partial_unanimous = (
                not local_metadata_only
                and release_decision is not None
                and release_decision.reason_code == "INCOMPLETE_EMBEDDED_RELEASE_IDS"
                and _populated_releases_agree(tracks)
            )
            if (
                decision is None
                and release_decision is not None
                and not attempt_partial_unanimous
            ):
                decision = release_decision
            elif decision is None and local_metadata_only:
                decision = _embedded_decision(tracks, raw_tracks)
            elif decision is None and any(track.release_mbid for track in tracks):
                decision = None
            elif decision is None:
                decision = _embedded_decision(tracks, raw_tracks)
            decision_source = "embedded" if decision is not None else "automatic"
            if decision is None:
                if local_metadata_only:
                    decision = IdentificationDecision(
                        outcome="no_candidate",
                        reason_code="NO_EXTERNAL_RESULT",
                    )
            if decision is None:
                # Fail fast while the provider is down: defer (with the queue's
                # backoff) instead of recalling candidates only for every call to
                # short-circuit on the open breaker. Embedded/local-metadata
                # decisions above are unaffected and keep draining.
                if (
                    self._provider_available is not None
                    and not self._provider_available()
                ):
                    await self._queue.defer(
                        job,
                        worker_id,
                        "PROVIDER_TEMPORARILY_UNAVAILABLE",
                        now=timestamp,
                    )
                    return "provider_deferred"
                cached_release_groups: list[str] = []
                cached_outcomes: dict[str, object] = {}
                for track, row in zip(tracks, raw_tracks, strict=True):
                    cached = await self._store.get_fingerprint_outcome(
                        track.local_track_id,
                        str(row["stat_revision"]),
                        FINGERPRINTER_VERSION,
                    )
                    if cached is not None:
                        cached_outcomes[track.local_track_id] = cached
                        # 4.10b lane A: a partial decode never seeds support
                        # or recall alone - it needs descriptive corroboration
                        # (tags + quorum still identify; full prints flow
                        # through unchanged below).
                        cached_partial = bool(
                            getattr(cached, "partial_decode", False)
                        )
                        if not cached_partial:
                            cached_release_groups.extend(
                                cached.release_group_ids
                            )
                        if (
                            not track.recording_mbid
                            and not track.fingerprint_recording_mbid
                            and cached.state == "matched"
                            and cached.recording_mbid
                            and not cached_partial
                        ):
                            track.fingerprint_recording_mbid = cached.recording_mbid
                recalled = await self._candidates.recall(
                    tracks,
                    cached_fingerprint_release_groups=list(
                        dict.fromkeys(cached_release_groups)
                    ),
                    explicit=bool(job["requested_by_user_id"]),
                    checkpoint=checkpoint,
                )
                if await self._queue.is_paused():
                    await self._pause(job, worker_id, "candidate_search")
                    return "paused"
                decision = self._evidence_engine.decide(
                    tracks, recalled, full_recall=_full_recall()
                )
                new_release_groups: list[str] = []
                if decision.outcome in ("ambiguous", "insufficient_evidence"):
                    requested = 0
                    for track, row in zip(tracks, raw_tracks, strict=True):
                        supported_recordings = {
                            item.recording_mbid
                            for candidate in decision.candidates
                            for item in candidate.track_evidence
                            if item.local_track_id == track.local_track_id
                            and item.classification == "supported"
                            and item.recording_mbid
                        }
                        needed = (
                            not (track.recording_mbid or track.fingerprint_recording_mbid)
                            and len(supported_recordings) != 1
                        )
                        if not needed:
                            # 4.9: route unneeded tracks through the service
                            # so it writes/reuses the
                            # write-once-per-stat_revision skipped row.
                            # did_work stays False, so the budget stays free.
                            await self._fingerprints.fingerprint_if_needed(
                                local_track_id=track.local_track_id,
                                path=Path(str(row["file_path"])),
                                stat_revision=str(row["stat_revision"]),
                                needed=False,
                                now=timestamp,
                                checkpoint=checkpoint,
                            )
                            continue
                        cached = cached_outcomes.get(track.local_track_id)
                        cache_hit = cached is not None and getattr(
                            cached, "state", ""
                        ) in ("matched", "no_match", "skipped")
                        if not cache_hit and (
                            requested >= MAX_NEW_FINGERPRINTS_PER_ATTEMPT
                        ):
                            break
                        outcome, did_work = (
                            await self._fingerprints.fingerprint_if_needed(
                                local_track_id=track.local_track_id,
                                path=Path(str(row["file_path"])),
                                stat_revision=str(row["stat_revision"]),
                                needed=needed,
                                now=timestamp,
                                checkpoint=checkpoint,
                            )
                        )
                        # F-042: an instant terminal cache hit did no fpcalc or
                        # lookup work, so it must not consume budget slots that
                        # later tracks need. F-07: `did_work` is the only
                        # honest no-work signal - a reused cached `failed` row
                        # reads cache_hit=False but did no work either.
                        if did_work:
                            requested += 1
                        if await self._queue.is_paused():
                            await self._pause(job, worker_id, "fingerprinting")
                            return "paused"
                        # F-07: a reused cached failure bypasses the defer
                        # branch - it did no new work, so the defer already
                        # happened (if ever) on the attempt that did the work.
                        # Fresh failures on load-bearing tracks keep deferring.
                        if (
                            outcome is not None
                            and outcome.state == "failed"
                            and did_work
                        ):
                            # F-MATCH-04: a local fpcalc failure is NOT a
                            # provider outage. Defer under its own honest code
                            # so the row never becomes eligible for the
                            # provider-only reset/resurrection gates.
                            if outcome.failure_code == "FINGERPRINT_LOCAL_FAILURE":
                                await self._queue.defer(
                                    job,
                                    worker_id,
                                    "FINGERPRINT_LOCAL_FAILURE",
                                    now=timestamp,
                                )
                                return "provider_deferred"
                            await self._queue.defer(
                                job,
                                worker_id,
                                "PROVIDER_TEMPORARILY_UNAVAILABLE",
                                now=timestamp,
                            )
                            return "provider_deferred"
                        if outcome is not None and outcome.recording_mbid:
                            # 4.10b lane A: partial-derived MBIDs cannot seed
                            # supported_recordings or recall alone. Full
                            # prints (False) keep today's behavior; short
                            # tracks that only ever yield partials still
                            # identify via tags + quorum.
                            if bool(getattr(outcome, "partial_decode", False)):
                                pass
                            else:
                                if (
                                    not track.recording_mbid
                                    and not track.fingerprint_recording_mbid
                                ):
                                    track.fingerprint_recording_mbid = (
                                        outcome.recording_mbid
                                    )
                                new_release_groups.extend(
                                    outcome.release_group_ids
                                )
                    if new_release_groups:
                        recalled = await self._candidates.recall(
                            tracks,
                            cached_fingerprint_release_groups=list(
                                dict.fromkeys(
                                    [*cached_release_groups, *new_release_groups]
                                )
                            ),
                            explicit=bool(job["requested_by_user_id"]),
                            checkpoint=checkpoint,
                        )
                        if await self._queue.is_paused():
                            await self._pause(job, worker_id, "candidate_search")
                            return "paused"
                        decision = self._evidence_engine.decide(
                    tracks, recalled, full_recall=_full_recall()
                )
                # P1-A/C(a): skip the extra sibling fetch whenever transient delta
                # is nonempty (saves 1 fetch exactly under throttle).
                _sibling_det_delta = set(degradation.deterministic_sources()) - deterministic_before
                _sibling_deg_delta = set(degradation.degraded_summary()) - degraded_before
                _sibling_transient = {source for source in _sibling_deg_delta if source not in degradation.deterministic_sources()}
                if (
                    decision.outcome in _SIBLING_TRIAL_OUTCOMES
                    and not (_sibling_det_delta and not decision.candidates)
                    and not (_sibling_deg_delta and not decision.candidates)
                    and not _sibling_transient
                    and (self._provider_available is None or self._provider_available())
                ):
                    # EditionsEtc Phase 2 within-group sibling trial: when the
                    # wrong sibling edition was recalled, evidence stays
                    # ambiguous/insufficient even though a usable edition
                    # exists in the same release group. Retry ONCE per attempt,
                    # including ranked siblings for the qualifying groups only.
                    sibling_release_groups = _sibling_trial_release_groups(
                        decision, recalled
                    )
                    if sibling_release_groups:
                        recalled = await self._candidates.recall(
                            tracks,
                            cached_fingerprint_release_groups=list(
                                dict.fromkeys(
                                    [*cached_release_groups, *new_release_groups]
                                )
                            ),
                            explicit=bool(job["requested_by_user_id"]),
                            checkpoint=checkpoint,
                            sibling_release_group_ids=sibling_release_groups,
                        )
                        if await self._queue.is_paused():
                            await self._pause(job, worker_id, "candidate_search")
                            return "paused"
                        decision = self._evidence_engine.decide(
                    tracks, recalled, full_recall=_full_recall()
                )

            # P2 RG edition-uncertain tier mapping point: an edition-uncertain
            # decision is already a non-review terminal (RG pin + ranked keys).
            # It flows through both backstops below unchanged: embedded or
            # manual/legacy conflicts still flip it to contradictory review.
            _enforce_raw_track_identities(decision, raw_tracks)
            degraded = degradation.degraded_summary()
            # P1-A: delta since init splits deterministic vs transient failures
            # during this attempt. Deterministic keeps the UNMAPPABLE path;
            # transient-only delta defers even with partial candidates. Empty
            # delta (genuine empty, embedded/local-metadata fast paths with no
            # provider calls) stays terminal.
            deterministic_delta = set(degradation.deterministic_sources()) - deterministic_before
            degraded_delta = set(degraded) - degraded_before
            transient_delta = {source for source in degraded_delta if source not in degradation.deterministic_sources()}
            if deterministic_delta and not decision.candidates:
                # F-IDENT-02: a typed payload-shape failure is deterministic,
                # not an outage. Defer under the honest code so the row keeps
                # the ordinary bounded backoff but never provider-resurrects.
                await self._queue.defer(
                    job,
                    worker_id,
                    UNMAPPABLE_PROVIDER_PAYLOAD,
                    now=timestamp,
                )
                return "provider_deferred"
            if transient_delta:
                # P1-A force-review cap: >=6 attempts with zero candidates falls
                # through to terminal review instead of another defer, preventing
                # a defer-loop on genuinely-empty albums.
                try:
                    _attempt_count = int(job.get("attempt_count", 1) or 1)
                except (TypeError, ValueError):
                    _attempt_count = 1
                if not (_attempt_count >= 6 and not decision.candidates):
                    await self._queue.defer(
                        job,
                        worker_id,
                        "PROVIDER_TEMPORARILY_UNAVAILABLE",
                        now=timestamp,
                    )
                    return "provider_deferred"
            _enforce_existing_album_identity(decision, context["identity"], raw_tracks)
            # Step 2.5 (E-02) identify-lane auto-accept gate: pre-persist,
            # after both backstops and the transient-defer block above (gating
            # at the first-decide site would pin decisions later flipped to
            # contradictory or deferred). Embedded exact-tag decisions keep the
            # legacy path: file facts need no edition gate.
            gate_winner: CandidateEvidence | None = None
            gate_ranking: list[dict[str, object]] = []
            gate_reason: str | None = None
            if decision_source == "automatic":
                gate_disposition, gate_winner, gate_ranking = (
                    await self._evaluate_edition_gate(
                        job=job,
                        worker_id=worker_id,
                        decision=decision,
                        tracks=tracks,
                        raw_tracks=raw_tracks,
                        timestamp=timestamp,
                    )
                )
                if gate_disposition == "deferred":
                    return "provider_deferred"
                if gate_disposition.startswith("veto:"):
                    gate_reason = gate_disposition[len("veto:") :]
                    gate_winner = None
            # P2 tier terminal (evaluated after both backstops above): the RG pin
            # and ranked keys persist RG-only; a flipped contradictory outcome
            # clears the predicate and takes the ordinary review path instead.
            # A gate veto demotes the decision above, so it flows through here.
            tier_terminal = is_edition_uncertain(decision)
            evidence_records = [
                IdentificationEvidenceRecord(
                    id=str(uuid.uuid4()),
                    attempt_id="",
                    candidate_key=_candidate_key(candidate),
                    evidence=candidate,
                    created_at=timestamp,
                )
                for candidate in decision.candidates
            ]
            attempt_id = str(uuid.uuid4())
            for record in evidence_records:
                record.attempt_id = attempt_id
            tag_revision, file_revision, policy_revision = album_input_revisions(
                raw_tracks
            )
            attempt = IdentificationAttempt(
                id=attempt_id,
                local_album_id=str(job["local_album_id"]),
                trigger=str(job["kind"]),
                requested_by_user_id=job["requested_by_user_id"],
                input_tag_revision=tag_revision,
                input_file_revision=file_revision,
                input_policy_revision=policy_revision,
                input_identity_revision=identity_revision,
                matcher_version=MATCHER_VERSION,
                state=decision.outcome,
                terminal_reason_code=decision.reason_code,
                selected_candidate_key=decision.selected_candidate_key,
                candidate_count=len(decision.candidates),
                degradation_flags=[
                    f"{source}:{status}" for source, status in sorted(degraded.items())
                ]
                + ([f"auto_gate:{gate_reason}"] if gate_reason is not None else []),
                started_at=timestamp,
                completed_at=timestamp,
            )
            gate_evidence_id = (
                next(
                    (
                        record.id
                        for record in evidence_records
                        if gate_winner is not None
                        and record.candidate_key == _candidate_key(gate_winner)
                    ),
                    None,
                )
                if gate_winner is not None
                else None
            )
            current_job = await self._store.get_identification_job_row(str(job["id"]))
            await self._store.finish_identification_job(
                str(job["id"]),
                worker_id=worker_id,
                expected_job_revision=int(
                    current_job["row_revision"]
                    if current_job is not None
                    else job["row_revision"]
                ),
                expected_album_revision=int(context["album"]["row_revision"]),
                expected_input_revision=":".join(
                    (tag_revision, file_revision, policy_revision)
                ),
                attempt=attempt,
                evidence=evidence_records,
                outcome=decision.outcome,
                review_id=str(uuid.uuid4()),
                completed_at=timestamp,
                decision_source=decision_source,
                selected_by_user_id=(
                    str(job["requested_by_user_id"])
                    if decision_source == "manual" and job["requested_by_user_id"]
                    else None
                ),
                source_context=operation_source_context,
                edition_uncertain=tier_terminal,
                release_group_mbid=(
                    decision.release_group_mbid if tier_terminal else None
                ),
                ranked_edition_keys=(
                    list(decision.ranked_edition_keys) if tier_terminal else None
                ),
                auto_accept_evidence=gate_winner,
                auto_accept_evidence_id=gate_evidence_id,
                auto_accept_ranking=gate_ranking or None,
            )
            if decision.outcome == "identified" and self._on_identified is not None:
                try:
                    await self._on_identified(
                        str(job["local_album_id"]), policy_revision
                    )
                except Exception:  # noqa: BLE001 - identification is already committed
                    logger.warning(
                        "Automatic scan-discovered management scheduling failed",
                        exc_info=True,
                    )
                    # F-061: durable marker so the gap is queryable (and can be
                    # swept later) instead of silently relying on a full rescan.
                    try:
                        await self._store.mark_management_schedule_pending(
                            str(job["local_album_id"])
                        )
                    except Exception:  # noqa: BLE001 - never mask the original
                        logger.exception("Failed to record management_schedule_pending")
            prior_identity = context["identity"] or {}
            # Mirrors the store's hold branch exactly (same pre-commit inputs
            # the tx revision-guards): tier params complete, prior sealed
            # automatic exact, same release group. Anything less complete
            # never reaches the tier branch, so it must not signal.
            tier_complete = bool(
                tier_terminal
                and decision.release_group_mbid
                and decision.ranked_edition_keys
            )
            prior_exact = bool(
                prior_identity.get("decision_source") == "automatic"
                and prior_identity.get("release_mbid")
            )
            same_group = (
                str(prior_identity.get("release_group_mbid") or "").casefold()
                == str(decision.release_group_mbid or "").casefold()
            )
            if tier_complete and prior_exact and same_group:
                # The sealed exact survived a weak re-match. Deliberately
                # quiet - no review row, no identified side effects; the
                # attempt flag is the audit.
                _edition_hold_metrics.increment("tier_hold:kept_exact")
                logger.info(
                    "identify edition hold: kept sealed exact album=%s release=%s",
                    str(job["local_album_id"]),
                    str(prior_identity["release_mbid"]),
                )
            elif tier_complete and prior_exact:
                # Tier for a different group: the old exact demotes to the
                # new group pin with a tier row for review (the contradiction
                # stays visible by design).
                _edition_hold_metrics.increment("tier_demotion:rg_only")
            if self._invalidate is not None:
                # ST1: thread the local album id so the provider hook can
                # resolve rg/artist entity ids from the committed row.
                # Runs after the hold/demotion signal: the commit already
                # landed, so a post-commit invalidation failure must not lose
                # the metric for it.
                await self._invalidate(
                    {
                        "library",
                        "artist",
                        "search",
                        "home",
                        "discover",
                        "compatibility",
                        "artwork",
                        "review",
                    },
                    [str(job["local_album_id"])],
                )
            return str(decision.outcome)
        except asyncio.CancelledError:
            # R-04: release the 60s claim so the job is immediately
            # reclaimable instead of sitting running until lease expiry.
            # release() is NOT idempotent (revision bumps) - called exactly
            # once here; never defer() on the cancel path (30s floor +
            # MAX_DEFERRALS_EXCEEDED counting). StaleRevisionError means the
            # finish commit already landed → commit wins, swallow.
            # (R-03: no worker-level cleanup - this job-level release runs
            # before the cancel propagates to the worker, which must not
            # double-release.)
            try:
                await self._queue.release(job, worker_id, now=timestamp)
            except StaleRevisionError:
                pass
            raise
        except CircuitOpenError as exc:
            # Defer with breaker deadline, not just queue backoff, per F-PERF-01
            retry_after = getattr(exc, "retry_after_seconds", None)
            await self._queue.defer(
                job,
                worker_id,
                "PROVIDER_TEMPORARILY_UNAVAILABLE",
                now=timestamp,
                retry_after_seconds=retry_after,
            )
            return "provider_deferred"
        finally:
            clear_degradation_context()

    async def _pause(self, job: dict, worker_id: str, phase: str) -> None:
        """F-058: the pause checkpoint keeps ONLY the phase label and matcher
        version for observability. The serialized candidate evidence was dead
        weight implying replay semantics that do not exist - restoring
        ``AlbumCandidate`` objects from post-decision ``CandidateEvidence``
        would be lossy (no candidate-side titles/durations), so resume
        deliberately re-runs recall under the queue's backoff bounds."""
        current = await self._store.get_identification_job_row(str(job["id"]))
        await self._queue.checkpoint_pause(
            job,
            worker_id,
            {"phase": phase, "matcher_version": MATCHER_VERSION},
            expected_job_revision_override=(
                int(current["row_revision"]) if current is not None else None
            ),
        )
