"""Durable MusicBrainz-proven artist credit projection and reconciliation."""

from __future__ import annotations

import hashlib
import json
import time
import unicodedata
import uuid
from collections.abc import Awaitable, Callable

import msgspec
import msgspec.json
from infrastructure.resilience.retry import CircuitOpenError
from api.v1.schemas.artist_reconciliation import (
    ArtistCreditEvidence,
    ArtistDuplicateGroupDetail,
    ArtistDuplicateGroupDismissResponse,
    ArtistDuplicateGroupListResponse,
    ArtistDuplicateGroupSummary,
    ArtistOwnedReference,
    ArtistReconciliationMember,
    ArtistReconciliationProgress,
)
from core.exceptions import (
    ConflictError,
    ExternalServiceError,
    ResourceNotFoundError,
    StaleRevisionError,
    ValidationError,
)
from infrastructure.persistence.gh293_calibration import (
    BACKGROUND_TIMESLICE_SECONDS,
)
from infrastructure.persistence.native_library_store import NativeLibraryStore
from infrastructure.validators import is_valid_mbid
from infrastructure.queue.priority_queue import RequestPriority
from models.artist_reconciliation import (
    ProviderAlbumArtistProjection,
    ProviderArtistCredit,
    ProviderTrackArtistProjection,
)
from models.library_management_canonical import CanonicalReleaseDocument
from models.library_work import OperationJob
from repositories.protocols.musicbrainz_management import (
    CanonicalMusicBrainzRepositoryProtocol,
    MbManagementArtistCredit,
    MbManagementRelease,
)
from services.native.background_workload_gate import BackgroundWorkloadGate
from services.native.bootstrap_demand_signal import BootstrapDemandSignal
from services.native.wal_checkpoint_service import WalCheckpointService

ARTIST_RECONCILIATION_PURPOSE = "artist_identity_reconciliation"
ARTIST_RECONCILIATION_VERSION = "musicbrainz-artist-credit-v3"
_BACKFILL_IDEMPOTENCY_KEY = "artist-identity-reconciliation:v3:backfill"
# Provider-deferred work must not be retried immediately: the defer re-queues the
# job, and without a not-before the operation worker instantly re-claims the same
# item and hot-spins against an open circuit breaker. 120 s gives the shared
# MusicBrainz breaker (60 s timeout) a recovery window between attempts.
_PROVIDER_DEFER_RETRY_SECONDS = 120.0
_GROUP_NAMESPACE = uuid.UUID("4f2de7c1-3e43-53bf-9d04-0e0f7755394e")


def _canonical_json(value: object) -> str:
    return json.dumps(
        msgspec.to_builtins(value),
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    )


def _input_revision(context: dict) -> str:
    identity = context["identity"]
    tracks = context["tracks"]
    payload = {
        "projection_version": ARTIST_RECONCILIATION_VERSION,
        "release_mbid": identity.get("release_mbid") if identity else None,
        "album_identity_revision": identity.get("row_revision") if identity else None,
        "canonical_payload_sha256": context.get("canonical_payload_sha256"),
        "tracks": [
            {
                "id": row["id"],
                "applied_policy": row["applied_policy"],
                "release_mbid": row["identity_release_mbid"],
                "release_track_mbid": row["release_track_mbid"],
                "identity_revision": row["identity_revision"],
            }
            for row in tracks
        ],
    }
    return hashlib.sha256(
        json.dumps(payload, separators=(",", ":"), sort_keys=True).encode()
    ).hexdigest()


def _raw_credits(
    values: list[MbManagementArtistCredit],
) -> tuple[ProviderArtistCredit, ...]:
    result: list[ProviderArtistCredit] = []
    for position, value in enumerate(values):
        artist = value.artist
        credited_name = value.name or artist.name
        canonical_name = artist.name or credited_name
        if not artist.id or not credited_name or not canonical_name:
            continue
        result.append(
            ProviderArtistCredit(
                position=position,
                artist_mbid=artist.id,
                canonical_name=canonical_name,
                credited_name=credited_name,
                sort_name=artist.sort_name or canonical_name,
                join_phrase=value.joinphrase,
            )
        )
    return tuple(result)


def _fold_text(value: str) -> str:
    normalized = unicodedata.normalize("NFKD", value.strip().casefold())
    return " ".join(
        "".join(
            character
            for character in normalized
            if not unicodedata.combining(character)
        ).split()
    )


def legacy_identity_has_provider_contradiction(context: dict) -> bool:
    """Reject incomplete legacy exact identities contradicted by every indexed file."""

    identity = context.get("identity")
    if (
        identity is None
        or identity.get("decision_source") != "legacy_import"
        or not identity.get("release_group_mbid")
        or not identity.get("release_mbid")
    ):
        return False
    tracks = [
        track
        for track in context.get("tracks", [])
        if track.get("availability") == "indexed"
    ]
    if not tracks:
        return False
    embedded_groups = {
        str(track["embedded_release_group_mbid"])
        for track in tracks
        if track.get("embedded_release_group_mbid")
    }
    tag_titles = {
        _fold_text(str(track["tag_album_title"]))
        for track in tracks
        if track.get("tag_album_title")
    }
    tag_artists = {
        _fold_text(str(track["tag_album_artist_name"]))
        for track in tracks
        if track.get("tag_album_artist_name")
    }
    complete_embedded_evidence = all(
        track.get("embedded_release_group_mbid") for track in tracks
    )
    complete_tag_evidence = all(
        track.get("tag_album_title") and track.get("tag_album_artist_name")
        for track in tracks
    )
    incomplete_exact_map = any(
        not track.get("release_track_mbid")
        or track.get("identity_release_mbid") != identity.get("release_mbid")
        for track in tracks
    )
    if (
        not complete_embedded_evidence
        or not complete_tag_evidence
        or not incomplete_exact_map
        or len(embedded_groups) != 1
        or len(tag_titles) != 1
        or len(tag_artists) != 1
    ):
        return False
    embedded_group = next(iter(embedded_groups))
    return (
        is_valid_mbid(embedded_group)
        and embedded_group != str(identity["release_group_mbid"])
        and next(iter(tag_titles)) == str(context["album"]["title_folded"])
    )


class ArtistIdentityReconciliationService:
    def __init__(
        self,
        store: NativeLibraryStore,
        provider: CanonicalMusicBrainzRepositoryProtocol,
        workload_gate: BackgroundWorkloadGate | None = None,
        on_catalog_changed: Callable[[], Awaitable[None]] | None = None,
        bootstrap_demand: BootstrapDemandSignal | None = None,
        wal_checkpoint: WalCheckpointService | None = None,
        *,
        clock: Callable[[], float] = time.time,
    ) -> None:
        self._store = store
        self._provider = provider
        self._workload_gate = workload_gate
        self._on_catalog_changed = on_catalog_changed
        self._bootstrap_demand = bootstrap_demand
        self._wal_checkpoint = wal_checkpoint
        self._clock = clock

    async def enqueue_backfill(self) -> dict:
        now = self._clock()
        job = OperationJob(
            id=str(uuid.uuid4()),
            kind="repair",
            requested_by_user_id=None,
            input_catalog_revision=await self._store.get_catalog_revision(),
            idempotency_key=_BACKFILL_IDEMPOTENCY_KEY,
            created_at=now,
        )
        return await self._store.create_repair_operation(
            job,
            scope={"purpose": ARTIST_RECONCILIATION_PURPOSE, "album_ids": []},
            source_matcher_version=None,
            target_matcher_version=ARTIST_RECONCILIATION_VERSION,
        )

    async def enqueue_album(self, local_album_id: str) -> dict | None:
        context = await self._store.get_artist_reconciliation_context(local_album_id)
        if context is None or not context["tracks"]:
            return None
        revision = _input_revision(context)
        existing = await self._store.find_active_repair_operation_for_album(
            ARTIST_RECONCILIATION_PURPOSE, local_album_id
        )
        if existing is not None:
            return existing
        now = self._clock()
        job = OperationJob(
            id=str(uuid.uuid4()),
            kind="repair",
            requested_by_user_id=None,
            input_catalog_revision=await self._store.get_catalog_revision(),
            idempotency_key=(
                f"artist-identity-reconciliation:v3:{local_album_id}:{revision}"
            ),
            created_at=now,
        )
        return await self._store.create_repair_operation(
            job,
            scope={
                "purpose": ARTIST_RECONCILIATION_PURPOSE,
                "album_ids": [local_album_id],
            },
            source_matcher_version=None,
            target_matcher_version=ARTIST_RECONCILIATION_VERSION,
        )

    async def run_claimed(self, job: dict, worker_id: str) -> dict:
        job_id = str(job["id"])
        started = time.monotonic()
        while True:
            now = self._clock()
            while True:
                try:
                    staged = await self._store.materialize_repair_operation_batch(
                        job_id, worker_id, now=now
                    )
                except StaleRevisionError:
                    # A catalog change while the worklist is unsealed rebases the
                    # SAME static-key job onto the current revision (or fails
                    # closed when progress exists) and resumes next pass.
                    rebased = await self._store.rebase_repair_operation(
                        job_id, worker_id, now=now
                    )
                    return await self._store.yield_operation_job(
                        job_id,
                        worker_id,
                        now=now,
                        reason_code="PIN_REBASED",
                    ) if rebased["rebased"] else rebased["job"]
                if staged["complete"]:
                    break
                if time.monotonic() - started >= BACKGROUND_TIMESLICE_SECONDS:
                    return await self._store.yield_operation_job(
                        job_id,
                        worker_id,
                        now=now,
                        reason_code="BACKGROUND_TIMESLICE_EXPIRED",
                    )
            controlled = await self._store.checkpoint_operation_control(
                job_id, worker_id, now=now
            )
            if controlled is not None and controlled["state"] != "running":
                return controlled
            if (
                self._workload_gate is not None
                and not self._workload_gate.scan_active
            ):
                # Scan-active is handled below by deferring the claimed unit;
                # never block a repair worker inside the scan safety wait.
                await self._workload_gate.wait_until_available()
            if self._bootstrap_demand is not None:
                await self._bootstrap_demand.wait_until_idle()
            if (
                self._wal_checkpoint is not None
                and self._wal_checkpoint.background_suspended
            ):
                return await self._store.yield_operation_job(
                    job_id,
                    worker_id,
                    now=now,
                    reason_code="WAL_BACKPRESSURE",
                )
            # One bounded pass of subjects; control is probed read-only between
            # units and materialization stays sealed (read-only probe).
            control_pending = False
            while time.monotonic() - started < BACKGROUND_TIMESLICE_SECONDS:
                if await self._store.probe_operation_control(job_id, worker_id):
                    control_pending = True
                    break
                now = self._clock()
                work = await self._store.claim_operation_work(
                    job_id, worker_id, now=now
                )
                if work is None:
                    return await self._store.finish_sealed_repair_operation_job(
                        job_id,
                        worker_id,
                        state="succeeded",
                        terminal_code="RECONCILIATION_COMPLETED",
                        now=now,
                    )
                album_id = str(work["local_album_id"])
                context = await self._store.get_artist_reconciliation_context(album_id)
                revision = _input_revision(context) if context is not None else "missing"
                if (
                    self._workload_gate is not None
                    and self._workload_gate.scan_active
                ):
                    return await self._store.defer_artist_reconciliation_work(
                        job_id=job_id,
                        ordinal=int(work["ordinal"]),
                        worker_id=worker_id,
                        local_album_id=album_id,
                        input_revision=revision,
                        reason_code="FILESYSTEM_SCAN_ACTIVE",
                        now=now,
                    )
                if (
                    self._wal_checkpoint is not None
                    and self._wal_checkpoint.background_suspended
                ):
                    return await self._store.yield_operation_job(
                        job_id,
                        worker_id,
                        now=now,
                        reason_code="WAL_BACKPRESSURE",
                    )
                if context is not None and legacy_identity_has_provider_contradiction(
                    context
                ):
                    await self._store.complete_artist_reconciliation_work(
                        job_id=job_id,
                        ordinal=int(work["ordinal"]),
                        worker_id=worker_id,
                        local_album_id=album_id,
                        input_revision=revision,
                        result_state="provider_conflict",
                        reason_code=("LEGACY_IDENTITY_CONTRADICTS_EMBEDDED_RELEASE_GROUP"),
                        now=now,
                        skipped=True,
                    )
                    continue
                if context is None or context["identity"] is None:
                    anchored = await self._store.apply_provider_anchored_artist_convergence(
                        album_id,
                        operation_job_id=job_id,
                        now=now,
                    )
                    retired = list(anchored["retired_artist_ids"])
                    if retired and self._on_catalog_changed is not None:
                        await self._on_catalog_changed()
                    await self._store.complete_artist_reconciliation_work(
                        job_id=job_id,
                        ordinal=int(work["ordinal"]),
                        worker_id=worker_id,
                        local_album_id=album_id,
                        input_revision=revision,
                        result_state=(
                            "resolved_automatically" if retired else "waiting_for_identity"
                        ),
                        reason_code=(
                            "PROVIDER_ANCHORED_ARTISTS_CONVERGED"
                            if retired
                            else "EXACT_RELEASE_NOT_ACCEPTED"
                        ),
                        now=now,
                        skipped=not retired,
                    )
                    continue
                identity = context["identity"]
                release_mbid = identity.get("release_mbid")
                if not release_mbid:
                    anchored = await self._store.apply_provider_anchored_artist_convergence(
                        album_id,
                        operation_job_id=job_id,
                        now=now,
                    )
                    retired = list(anchored["retired_artist_ids"])
                    if retired and self._on_catalog_changed is not None:
                        await self._on_catalog_changed()
                    await self._store.complete_artist_reconciliation_work(
                        job_id=job_id,
                        ordinal=int(work["ordinal"]),
                        worker_id=worker_id,
                        local_album_id=album_id,
                        input_revision=revision,
                        result_state=(
                            "resolved_automatically" if retired else "waiting_for_identity"
                        ),
                        reason_code=(
                            "PROVIDER_ANCHORED_ARTISTS_CONVERGED"
                            if retired
                            else "EXACT_RELEASE_NOT_ACCEPTED"
                        ),
                        now=now,
                        skipped=not retired,
                    )
                    continue
                anchored = await self._store.apply_provider_anchored_artist_convergence(
                    album_id,
                    operation_job_id=job_id,
                    now=now,
                )
                if anchored["retired_artist_ids"] and self._on_catalog_changed is not None:
                    await self._on_catalog_changed()
                previous = context["state"]
                if (
                    previous is not None
                    and previous["input_revision"] == revision
                    and previous["state"] != "provider_deferred"
                ):
                    await self._store.complete_artist_reconciliation_work(
                        job_id=job_id,
                        ordinal=int(work["ordinal"]),
                        worker_id=worker_id,
                        local_album_id=album_id,
                        input_revision=revision,
                        result_state=str(previous["state"]),
                        reason_code="UNCHANGED_PROVIDER_EVIDENCE",
                        now=now,
                        skipped=True,
                    )
                    continue

                release: MbManagementRelease | None = None
                cached_document: CanonicalReleaseDocument | None = None
                automatic_lookup = any(
                    str(track["applied_policy"]) == "automatic"
                    for track in context["tracks"]
                )
                cached_payload = context["canonical_payload_json"]
                if cached_payload:
                    try:
                        cached_document = msgspec.json.decode(
                            cached_payload, type=CanonicalReleaseDocument
                        )
                    except msgspec.DecodeError:
                        cached_document = None
                if cached_document is None and not automatic_lookup:
                    await self._store.complete_artist_reconciliation_work(
                        job_id=job_id,
                        ordinal=int(work["ordinal"]),
                        worker_id=worker_id,
                        local_album_id=album_id,
                        input_revision=revision,
                        result_state="waiting_for_identity",
                        reason_code="LOCAL_METADATA_PROVIDER_LOOKUP_DISABLED",
                        now=now,
                    )
                    continue
                if cached_document is None:
                    try:
                        release = await self._provider.get_canonical_release(
                            str(release_mbid),
                            includes=("artist-credits", "recordings", "release-groups"),
                            priority=RequestPriority.BACKGROUND_SYNC,
                        )
                    except (ExternalServiceError, CircuitOpenError) as exc:
                        retry_after = getattr(exc, "retry_after_seconds", None)
                        delay = _PROVIDER_DEFER_RETRY_SECONDS
                        if retry_after is not None:
                            try:
                                candidate = float(retry_after)
                                import math

                                if math.isfinite(candidate) and candidate > 0:
                                    delay = max(delay, candidate)
                            except (TypeError, ValueError):
                                pass
                        return await self._store.defer_artist_reconciliation_work(
                            job_id=job_id,
                            ordinal=int(work["ordinal"]),
                            worker_id=worker_id,
                            local_album_id=album_id,
                            input_revision=revision,
                            reason_code="PROVIDER_DEFERRED",
                            now=now,
                            retry_not_before=now + delay,
                        )
                try:
                    projection = self._projection(
                        context,
                        revision=revision,
                        release=release,
                        cached_document=cached_document,
                    )
                except ValidationError as error:
                    await self._store.complete_artist_reconciliation_work(
                        job_id=job_id,
                        ordinal=int(work["ordinal"]),
                        worker_id=worker_id,
                        local_album_id=album_id,
                        input_revision=revision,
                        result_state="provider_conflict",
                        reason_code=str(error),
                        now=now,
                        skipped=True,
                    )
                    continue
                try:
                    await self._store.apply_artist_credit_projection(
                        projection,
                        job_id=job_id,
                        ordinal=int(work["ordinal"]),
                        worker_id=worker_id,
                        now=now,
                    )
                    if self._on_catalog_changed is not None:
                        await self._on_catalog_changed()
                except StaleRevisionError:
                    await self._store.complete_artist_reconciliation_work(
                        job_id=job_id,
                        ordinal=int(work["ordinal"]),
                        worker_id=worker_id,
                        local_album_id=album_id,
                        input_revision=revision,
                        result_state="waiting_for_identity",
                        reason_code="STALE_INPUT",
                        now=now,
                        skipped=True,
                    )
                except (ConflictError, ValidationError) as error:
                    await self._store.complete_artist_reconciliation_work(
                        job_id=job_id,
                        ordinal=int(work["ordinal"]),
                        worker_id=worker_id,
                        local_album_id=album_id,
                        input_revision=revision,
                        result_state="provider_conflict",
                        reason_code=str(error),
                        now=now,
                        skipped=True,
                    )
            if control_pending:
                continue  # outer loop performs the control transition
            # Pass budget elapsed with subjects still pending: yield with the
            # persisted cooldown (no immediate reclaim).
            return await self._store.yield_operation_job(
                job_id,
                worker_id,
                now=now,
                reason_code="BACKGROUND_TIMESLICE_EXPIRED",
            )

    @staticmethod
    def _projection(
        context: dict,
        *,
        revision: str,
        release: MbManagementRelease | None,
        cached_document: CanonicalReleaseDocument | None,
    ) -> ProviderAlbumArtistProjection:
        identity = context["identity"]
        release_mbid = str(identity["release_mbid"])
        if cached_document is not None:
            if cached_document.identifiers.release_mbid != release_mbid:
                raise ValidationError("SELECTED_RELEASE_CONFLICT")
            album_credits = tuple(
                ProviderArtistCredit(
                    position=position,
                    artist_mbid=credit.artist_mbid,
                    canonical_name=credit.canonical_name,
                    credited_name=credit.credited_name,
                    sort_name=credit.sort_name,
                    join_phrase=credit.join_phrase,
                )
                for position, credit in enumerate(cached_document.artist_credits)
                if credit.artist_mbid
            )
            provider_tracks = {
                track.identifiers.release_track_mbid: track
                for medium in cached_document.media
                for track in medium.tracks
                if track.identifiers.release_track_mbid
            }
        else:
            if release is None:
                raise ValidationError("SELECTED_RELEASE_UNAVAILABLE")
            if release.id != release_mbid:
                raise ValidationError("SELECTED_RELEASE_CONFLICT")
            album_credits = _raw_credits(release.artist_credit)
            provider_tracks = {
                track.id: track
                for medium in release.media
                for track in medium.tracks
                if track.id
            }
        if not album_credits:
            raise ValidationError("AMBIGUOUS_RELEASE_ARTIST_CREDIT")

        projected_tracks: list[ProviderTrackArtistProjection] = []
        mapped_ids: set[str] = set()
        for row in context["tracks"]:
            release_track_mbid = row["release_track_mbid"]
            if (
                not release_track_mbid
                or row["identity_revision"] is None
                or row["identity_release_mbid"] != release_mbid
            ):
                continue
            if str(release_track_mbid) in mapped_ids:
                raise ValidationError("DUPLICATE_RELEASE_TRACK_MAPPING")
            mapped_ids.add(str(release_track_mbid))
            provider_track = provider_tracks.get(str(release_track_mbid))
            if provider_track is None:
                raise ValidationError("RELEASE_TRACK_MAPPING_NOT_FOUND")
            if cached_document is not None:
                credits = tuple(
                    ProviderArtistCredit(
                        position=position,
                        artist_mbid=credit.artist_mbid,
                        canonical_name=credit.canonical_name,
                        credited_name=credit.credited_name,
                        sort_name=credit.sort_name,
                        join_phrase=credit.join_phrase,
                    )
                    for position, credit in enumerate(provider_track.artist_credits)
                    if credit.artist_mbid
                )
            else:
                credits = _raw_credits(
                    provider_track.artist_credit
                    or provider_track.recording.artist_credit
                    or release.artist_credit
                )
            if not credits:
                raise ValidationError("AMBIGUOUS_TRACK_ARTIST_CREDIT")
            projected_tracks.append(
                ProviderTrackArtistProjection(
                    local_track_id=str(row["id"]),
                    track_revision=int(row["row_revision"]),
                    release_track_mbid=str(release_track_mbid),
                    track_identity_revision=int(row["identity_revision"]),
                    credits=credits,
                )
            )
        evidence_payload = {
            "release_mbid": release_mbid,
            "album_identity_revision": int(identity["row_revision"]),
            "album_credits": album_credits,
            "tracks": projected_tracks,
        }
        evidence_hash = hashlib.sha256(
            _canonical_json(evidence_payload).encode()
        ).hexdigest()
        return ProviderAlbumArtistProjection(
            local_album_id=str(context["album"]["id"]),
            album_revision=int(context["album"]["row_revision"]),
            release_mbid=release_mbid,
            album_identity_revision=int(identity["row_revision"]),
            input_revision=revision,
            evidence_hash=evidence_hash,
            album_credits=album_credits,
            tracks=tuple(projected_tracks),
            incomplete_track_mapping=len(projected_tracks) != len(context["tracks"]),
        )

    async def progress(self) -> ArtistReconciliationProgress:
        raw = await self._store.get_artist_reconciliation_status_data()
        groups = await self._groups()
        counts: dict[str, int] = {}
        for group in groups:
            counts[group.state] = counts.get(group.state, 0) + 1
        job = raw["job"]
        return ArtistReconciliationProgress(
            state=str(job["state"]) if job is not None else "idle",
            completed_count=int(job["completed_count"]) if job is not None else 0,
            expected_count=int(job["expected_work_count"]) if job is not None else 0,
            automatically_resolved_count=int(raw["automatically_resolved_count"]),
            waiting_for_identity_count=counts.get("waiting_for_identity", 0),
            genuine_review_count=sum(
                counts.get(value, 0)
                for value in (
                    "provider_conflict",
                    "ambiguous_credit_structure",
                    "same_name_only",
                )
            ),
            provider_conflict_count=counts.get("provider_conflict", 0),
            ambiguous_credit_structure_count=counts.get(
                "ambiguous_credit_structure", 0
            ),
            same_name_only_count=counts.get("same_name_only", 0),
            operation_job_id=str(job["id"]) if job is not None else None,
        )

    @staticmethod
    def _member(row: dict) -> ArtistReconciliationMember:
        return ArtistReconciliationMember(
            id=str(row["id"]),
            name=str(row["display_name"]),
            sort_name=row["sort_name"],
            row_revision=int(row["row_revision"]),
            provider_mbid=row["provider_artist_id"],
            album_credit_count=int(row["album_credit_count"]),
            track_credit_count=int(row["track_credit_count"]),
            primary_album_count=int(row["primary_album_count"]),
            favorite_count=int(row["favorite_count"]),
            playlist_count=int(row["playlist_count"]),
            history_count=int(row["history_count"]),
            compatibility_id_count=int(row["compatibility_id_count"]),
            proven_credit_count=int(row["proven_credit_count"]),
            active_credit_count=int(row["active_credit_count"]),
        )

    async def _groups(self) -> list[ArtistDuplicateGroupSummary]:
        raw = await self._store.get_artist_duplicate_group_data()
        by_name: dict[str, list[dict]] = {}
        for member in raw["members"]:
            by_name.setdefault(str(member["folded_name"]), []).append(member)
        dismissal_map = {
            (str(row["left_artist_id"]), str(row["right_artist_id"])): row
            for row in raw["dismissals"]
        }
        ambiguous_artist_ids = {
            str(row["album_artist_id"])
            for row in raw["states"]
            if row["state"] == "ambiguous_credit_structure"
        }
        groups: list[ArtistDuplicateGroupSummary] = []
        for members in by_name.values():
            members.sort(key=lambda row: (float(row["created_at"]), str(row["id"])))
            pairs = [
                (left, right)
                for index, left in enumerate(members)
                for right in members[index + 1 :]
            ]
            if pairs and all(
                (
                    dismissal := dismissal_map.get(
                        tuple(sorted((str(left["id"]), str(right["id"]))))
                    )
                )
                and int(dismissal["left_artist_revision"])
                == int(
                    left["row_revision"]
                    if str(left["id"]) < str(right["id"])
                    else right["row_revision"]
                )
                and int(dismissal["right_artist_revision"])
                == int(
                    right["row_revision"]
                    if str(left["id"]) < str(right["id"])
                    else left["row_revision"]
                )
                for left, right in pairs
            ):
                continue
            provider_mbids = sorted(
                {
                    str(value)
                    for member in members
                    for value in [
                        member["provider_artist_id"],
                        *member["proof_mbids"],
                    ]
                    if value
                }
            )
            direct_provider_mbids = {
                str(member["provider_artist_id"])
                for member in members
                if member["provider_artist_id"]
            }
            if len(direct_provider_mbids) > 1:
                state = "provider_conflict"
                reason = "CONFLICTING_PROVIDER_IDENTITIES"
            elif any(str(member["id"]) in ambiguous_artist_ids for member in members):
                state = "ambiguous_credit_structure"
                reason = "AMBIGUOUS_CREDIT_STRUCTURE"
            elif len(provider_mbids) > 1:
                state = "provider_conflict"
                reason = "CONFLICTING_PROVIDER_IDENTITIES"
            elif provider_mbids:
                state = "waiting_for_identity"
                reason = "INCOMPLETE_PROVIDER_PROOF"
            else:
                state = "same_name_only"
                reason = "NAME_MATCH_WITHOUT_PROVIDER_PROOF"
            owner = next(
                (
                    member
                    for member in members
                    if member["provider_artist_id"]
                    and str(member["provider_artist_id"]) in provider_mbids
                ),
                members[0] if provider_mbids else None,
            )
            member_models = [self._member(member) for member in members]
            groups.append(
                ArtistDuplicateGroupSummary(
                    id=str(
                        uuid.uuid5(
                            _GROUP_NAMESPACE,
                            ":".join(sorted(member.id for member in member_models)),
                        )
                    ),
                    display_name=str(members[0]["display_name"]),
                    state=state,
                    member_count=len(member_models),
                    members=member_models,
                    provider_mbids=provider_mbids,
                    recommended_survivor_id=str(owner["id"]) if owner else None,
                    affected_reference_count=sum(
                        member.active_credit_count
                        + member.favorite_count
                        + member.playlist_count
                        + member.history_count
                        + member.compatibility_id_count
                        for member in member_models
                    ),
                    reason_code=reason,
                )
            )
        action_payloads: list[tuple[dict, dict]] = []
        historical_ids: set[str] = set()
        for action in raw["actions"]:
            after = json.loads(str(action["after_json"]))
            retired = [str(value) for value in after.get("retired_artist_ids", [])]
            survivor = after.get("surviving_artist_id")
            if not isinstance(survivor, str) or not retired:
                continue
            historical_ids.update([survivor, *retired])
            action_payloads.append((action, after))
        historical_context = (
            await self._store.get_artist_merge_context(sorted(historical_ids))
            if historical_ids
            else {
                "artists": [],
                "identities": [],
                "reference_counts": {},
                "reference_counts_by_artist": {},
            }
        )
        historical_artists = {
            str(row["id"]): row for row in historical_context["artists"]
        }
        historical_identities = {
            str(row["local_artist_id"]): str(row["provider_artist_id"])
            for row in historical_context["identities"]
        }
        historical_reference_counts = historical_context["reference_counts_by_artist"]
        for action, after in action_payloads:
            survivor_id = str(after["surviving_artist_id"])
            ids = [survivor_id, *[str(value) for value in after["retired_artist_ids"]]]
            member_models = [
                ArtistReconciliationMember(
                    id=artist_id,
                    name=str(historical_artists[artist_id]["display_name"]),
                    sort_name=historical_artists[artist_id]["sort_name"],
                    row_revision=int(historical_artists[artist_id]["row_revision"]),
                    provider_mbid=historical_identities.get(artist_id),
                    album_credit_count=0,
                    track_credit_count=0,
                    primary_album_count=0,
                )
                for artist_id in ids
                if artist_id in historical_artists
            ]
            if len(member_models) < 2:
                continue
            provider_mbid = after.get("provider_artist_mbid")
            groups.append(
                ArtistDuplicateGroupSummary(
                    id=f"resolved:{action['id']}",
                    display_name=next(
                        (
                            member.name
                            for member in member_models
                            if member.id == survivor_id
                        ),
                        member_models[0].name,
                    ),
                    state="resolved_automatically",
                    member_count=len(member_models),
                    members=member_models,
                    provider_mbids=[str(provider_mbid)] if provider_mbid else [],
                    recommended_survivor_id=survivor_id,
                    affected_reference_count=sum(
                        int(count)
                        for artist_id in ids
                        for count in historical_reference_counts.get(
                            artist_id, {}
                        ).values()
                    ),
                    reason_code=str(action["reason_code"]),
                    resolved_at=float(action["created_at"]),
                )
            )
        groups.sort(key=lambda group: (group.display_name.casefold(), group.id))
        return groups

    async def list_groups(
        self,
        *,
        limit: int,
        cursor: str | None,
        state: str | None,
        search: str | None,
    ) -> ArtistDuplicateGroupListResponse:
        if limit < 1 or limit > 100:
            raise ValidationError("Artist group page size must be between 1 and 100.")
        groups = await self._groups()
        counts: dict[str, int] = {}
        for group in groups:
            counts[group.state] = counts.get(group.state, 0) + 1
        filtered = [
            group
            for group in groups
            if (state is None or group.state == state)
            and (
                not search
                or search.casefold() in group.display_name.casefold()
                or any(
                    search.casefold() in member.name.casefold()
                    for member in group.members
                )
            )
        ]
        if cursor is not None:
            try:
                start = (
                    next(
                        index
                        for index, group in enumerate(filtered)
                        if group.id == cursor
                    )
                    + 1
                )
            except StopIteration as error:
                raise ValidationError("The artist group cursor is invalid.") from error
        else:
            start = 0
        page = filtered[start : start + limit]
        has_more = start + limit < len(filtered)
        return ArtistDuplicateGroupListResponse(
            items=page,
            next_cursor=page[-1].id if has_more and page else None,
            has_more=has_more,
            total=len(filtered),
            counts=counts,
        )

    async def group_detail(self, group_id: str) -> ArtistDuplicateGroupDetail:
        group = next(
            (value for value in await self._groups() if value.id == group_id), None
        )
        if group is None:
            raise ResourceNotFoundError("Artist duplicate group not found.")
        references = await self._store.get_artist_duplicate_group_references(
            [member.id for member in group.members]
        )
        reference_counts = {
            "album_credits": sum(member.album_credit_count for member in group.members),
            "track_credits": sum(member.track_credit_count for member in group.members),
            "primary_albums": sum(
                member.primary_album_count for member in group.members
            ),
            "favorites": sum(member.favorite_count for member in group.members),
            "playlist_snapshots": sum(
                member.playlist_count for member in group.members
            ),
            "history": sum(member.history_count for member in group.members),
            "compatibility_ids": sum(
                member.compatibility_id_count for member in group.members
            ),
        }
        return ArtistDuplicateGroupDetail(
            id=group.id,
            display_name=group.display_name,
            state=group.state,
            member_count=group.member_count,
            members=group.members,
            provider_mbids=group.provider_mbids,
            recommended_survivor_id=group.recommended_survivor_id,
            affected_reference_count=group.affected_reference_count,
            reason_code=group.reason_code,
            resolved_at=group.resolved_at,
            evidence=[
                ArtistCreditEvidence(
                    subject_kind=str(row["subject_kind"]),
                    subject_id=str(row["subject_id"]),
                    subject_name=str(row["subject_name"]),
                    source_local_artist_id=row["source_local_artist_id"],
                    local_artist_id=str(row["local_artist_id"]),
                    artist_mbid=str(row["artist_mbid"]),
                    canonical_name=str(row["canonical_name"]),
                    credited_name=str(row["credited_name"]),
                    join_phrase=str(row["join_phrase"]),
                    release_mbid=str(row["release_mbid"]),
                    release_track_mbid=row["release_track_mbid"],
                    album_identity_revision=int(row["album_identity_revision"]),
                    track_identity_revision=(
                        int(row["track_identity_revision"])
                        if row["track_identity_revision"] is not None
                        else None
                    ),
                    evidence_hash=str(row["evidence_hash"]),
                )
                for row in references["evidence"]
            ],
            releases=[
                ArtistOwnedReference(
                    id=str(row["id"]),
                    name=str(row["name"]),
                    row_revision=int(row["row_revision"]),
                    identity_ready=bool(row["identity_ready"]),
                    exact_track_mapping_ready=bool(row["exact_track_mapping_ready"]),
                )
                for row in references["releases"]
            ],
            tracks=[
                ArtistOwnedReference(
                    id=str(row["id"]),
                    name=str(row["name"]),
                    row_revision=int(row["row_revision"]),
                    identity_ready=bool(row["identity_ready"]),
                    exact_track_mapping_ready=bool(row["exact_track_mapping_ready"]),
                )
                for row in references["tracks"]
            ],
            reference_counts=reference_counts,
            member_revisions={
                member.id: member.row_revision for member in group.members
            },
        )

    async def dismiss_group(
        self,
        group_id: str,
        expected_revisions: dict[str, int],
        actor_user_id: str,
    ) -> ArtistDuplicateGroupDismissResponse:
        group = next(
            (value for value in await self._groups() if value.id == group_id), None
        )
        if group is None:
            raise ResourceNotFoundError("Artist duplicate group not found.")
        if group.state == "resolved_automatically":
            raise ValidationError("A resolved artist group cannot be dismissed.")
        if set(expected_revisions) != {member.id for member in group.members}:
            raise StaleRevisionError("The artist group changed before dismissal.")
        count = await self._store.dismiss_artist_duplicate_group(
            artist_ids=[member.id for member in group.members],
            expected_revisions=expected_revisions,
            actor_user_id=actor_user_id,
            now=self._clock(),
        )
        return ArtistDuplicateGroupDismissResponse(
            group_id=group_id, dismissed_pairs=count
        )
