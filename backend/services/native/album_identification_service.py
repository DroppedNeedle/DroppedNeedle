"""Target-only orchestration for one durable album-identification attempt."""

from __future__ import annotations

import logging
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
from services.native.album_evidence_engine import MATCHER_VERSION, AlbumEvidenceEngine
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


def _to_grouping_track(row: dict) -> GroupingTrack:
    return GroupingTrack(
        local_track_id=str(row["id"]),
        root_id=str(row["root_id"]),
        relative_path=str(row["relative_path"]),
        title=str(row["title"] or ""),
        artist_name=str(row["artist_name"] or ""),
        album_title=str(row["album_title"] or ""),
        album_artist_name=str(row["album_artist_name"] or ""),
        artist_sort_name=row["artist_sort"],
        album_artist_sort_name=row["album_artist_sort"],
        track_number=int(row["track_number"] or 0),
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
            if row is None:
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


_SIBLING_TRIAL_OUTCOMES = ("ambiguous", "insufficient_evidence")


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
    ) -> None:
        self._store = store
        self._queue = queue
        self._candidates = candidates
        self._evidence_engine = evidence_engine
        self._fingerprints = fingerprints
        self._invalidate = invalidate
        self._on_identified = on_identified
        self._provider_available = provider_available

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
            if decision is None and release_decision is not None:
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
                        cached_release_groups.extend(cached.release_group_ids)
                        cached_outcomes[track.local_track_id] = cached
                        if (
                            not track.recording_mbid
                            and cached.state == "matched"
                            and cached.recording_mbid
                        ):
                            track.recording_mbid = cached.recording_mbid
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
                decision = self._evidence_engine.decide(tracks, recalled)
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
                            not track.recording_mbid and len(supported_recordings) != 1
                        )
                        if not needed:
                            continue
                        cached = cached_outcomes.get(track.local_track_id)
                        cache_hit = cached is not None and getattr(
                            cached, "state", ""
                        ) in ("matched", "no_match", "skipped")
                        if not cache_hit and (
                            requested >= MAX_NEW_FINGERPRINTS_PER_ATTEMPT
                        ):
                            break
                        outcome = await self._fingerprints.fingerprint_if_needed(
                            local_track_id=track.local_track_id,
                            path=Path(str(row["file_path"])),
                            stat_revision=str(row["stat_revision"]),
                            needed=needed,
                            now=timestamp,
                            checkpoint=checkpoint,
                        )
                        # F-042: an instant terminal cache hit did no fpcalc or
                        # lookup work, so it must not consume budget slots that
                        # later tracks need.
                        if not cache_hit:
                            requested += 1
                        if await self._queue.is_paused():
                            await self._pause(job, worker_id, "fingerprinting")
                            return "paused"
                        if outcome is not None and outcome.state == "failed":
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
                            track.recording_mbid = outcome.recording_mbid
                            new_release_groups.extend(outcome.release_group_ids)
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
                        decision = self._evidence_engine.decide(tracks, recalled)
                if (
                    decision.outcome in _SIBLING_TRIAL_OUTCOMES
                    and not (
                        degradation.has_deterministic_failure()
                        and not decision.candidates
                    )
                    and not (degradation.degraded_summary() and not decision.candidates)
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
                        decision = self._evidence_engine.decide(tracks, recalled)

            _enforce_raw_track_identities(decision, raw_tracks)
            degraded = degradation.degraded_summary()
            if degradation.has_deterministic_failure() and not decision.candidates:
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
            if degraded and not decision.candidates:
                await self._queue.defer(
                    job,
                    worker_id,
                    "PROVIDER_TEMPORARILY_UNAVAILABLE",
                    now=timestamp,
                )
                return "provider_deferred"
            _enforce_existing_album_identity(decision, context["identity"], raw_tracks)
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
                ],
                started_at=timestamp,
                completed_at=timestamp,
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
            if self._invalidate is not None:
                # ST1: thread the local album id so the provider hook can
                # resolve rg/artist entity ids from the committed row.
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
