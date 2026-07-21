"""Paginated review projections and revision-safe administrator actions."""

from __future__ import annotations

import base64
import binascii
import hashlib
import time
import uuid
from collections.abc import Callable
from typing import Any

import msgspec

from api.v1.schemas.library_operations import (
    BulkReviewApplyRequest,
    BulkReviewPreviewRequest,
    BulkReviewPreviewResponse,
    CandidateAcceptanceRequest,
    OperationResponse,
    ReviewActionRequest,
    ReviewActionResponse,
    ReviewCandidateDetail,
    ReviewDetailResponse,
    ReviewHistoryItem,
    ReviewListItem,
    ReviewListResponse,
    ReviewTrackDetail,
)
from core.exceptions import ResourceNotFoundError, StaleRevisionError, ValidationError
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.identification import CandidateEvidence
from models.library_work import OperationJob
from services.native.identification_revisions import album_input_revisions
from services.native.library_policy_resolver import LibraryPolicyResolver

DEFAULT_PAGE_SIZE = 50
MAX_PAGE_SIZE = 200
PREVIEW_TTL_SECONDS = 15 * 60
AUTOMATIC_SAFE_EVIDENCE_REASONS = frozenset(
    {"SUPPORTED", "ACCEPTED", "SUPPORTED_EMBEDDED_IDS"}
)
REVIEW_STATES = frozenset({"needs_review", "keep_tagged", "excluded", "resolved"})
REVIEW_POLICIES = frozenset({"automatic", "local_metadata", "excluded"})
ACTIVE_JOB_STATES = frozenset({"queued", "running", "paused"})


def _encode_cursor(sort: str, value: object, row_id: str) -> str:
    payload = msgspec.json.encode([sort, value, row_id])
    return base64.urlsafe_b64encode(payload).decode().rstrip("=")


def _decode_cursor(
    cursor: str | None, sort: str
) -> tuple[float | str | int | None, str | None]:
    if cursor is None:
        return None, None
    try:
        payload = cursor + "=" * (-len(cursor) % 4)
        decoded = msgspec.json.decode(base64.urlsafe_b64decode(payload))
        if not isinstance(decoded, list) or len(decoded) != 3 or decoded[0] != sort:
            raise ValueError
        value = decoded[1]
        if not isinstance(value, (str, int, float)):
            raise ValueError
        return value, str(decoded[2])
    except (binascii.Error, ValueError, TypeError, msgspec.DecodeError) as error:
        raise ValidationError("The review cursor is invalid.") from error


def _preview_token(action: str, selection: object, issued_at: int, nonce: str) -> str:
    encoded = msgspec.json.encode(
        {
            "action": action,
            "selection": selection,
            "issued_at": issued_at,
            "nonce": nonce,
        },
        order="deterministic",
    )
    digest = hashlib.sha256(encoded).hexdigest()
    return f"{issued_at}.{nonce}.{digest}"


def _validate_preview_token(
    token: str, action: str, selection: object, now: float
) -> None:
    try:
        issued_text, nonce, _ = token.split(".", 2)
        issued_at = int(issued_text)
    except (ValueError, TypeError) as error:
        raise ValidationError("The bulk preview token is invalid.") from error
    if issued_at > int(now) or now - issued_at > PREVIEW_TTL_SECONDS:
        raise StaleRevisionError(
            "The bulk preview expired. Preview the selection again."
        )
    if token != _preview_token(action, selection, issued_at, nonce):
        raise StaleRevisionError("The bulk selection changed after preview.")


class LibraryReviewService:
    def __init__(
        self,
        store: NativeLibraryStore,
        resolver_getter: Callable[[], LibraryPolicyResolver] | None = None,
    ) -> None:
        self._store = store
        self._resolver_getter = resolver_getter

    def _resolve_selection_filter(
        self, normalized_filter: dict[str, str]
    ) -> dict[str, str]:
        resolved = dict(normalized_filter)
        scope_ids_json = resolved.pop("scope_ids", None)
        scope_revision = resolved.pop("scope_revision", None)
        if scope_ids_json is None and scope_revision is None:
            return resolved
        if self._resolver_getter is None:
            raise ValidationError("Scoped review work is not configured.")
        resolver = self._resolver_getter()
        if scope_revision != resolver.policy_revision:
            raise StaleRevisionError(
                "Library settings changed. Reload the scopes and preview again."
            )
        if scope_ids_json is None:
            return resolved
        try:
            decoded = msgspec.json.decode(scope_ids_json.encode())
        except msgspec.DecodeError as error:
            raise ValidationError("The selected library scopes are invalid.") from error
        if (
            not isinstance(decoded, list)
            or not decoded
            or any(not isinstance(scope_id, str) for scope_id in decoded)
        ):
            raise ValidationError("Choose at least one valid library scope.")
        scope_ids = sorted(set(decoded))
        root_scopes = {
            root.id: {"root_id": root.id, "relative_path": "."}
            for root in resolver.settings.library_roots
        }
        rule_scopes = {
            rule.id: {"root_id": root.id, "relative_path": rule.relative_path}
            for root in resolver.settings.library_roots
            for rule in root.rules
        }
        unknown = [
            scope_id
            for scope_id in scope_ids
            if scope_id not in root_scopes and scope_id not in rule_scopes
        ]
        if unknown:
            raise ValidationError("A selected library scope no longer exists.")
        selected_rules = [scope_id for scope_id in scope_ids if scope_id in rule_scopes]
        if selected_rules and len(scope_ids) != 1:
            raise ValidationError(
                "Choose one nested policy path or one or more whole roots."
            )
        scopes = [
            root_scopes.get(scope_id, rule_scopes.get(scope_id))
            for scope_id in scope_ids
        ]
        resolved["scopes"] = msgspec.json.encode(scopes).decode()
        return resolved

    async def list_reviews(
        self,
        *,
        cursor: str | None = None,
        limit: int = DEFAULT_PAGE_SIZE,
        state: str | None = None,
        reason_code: str | None = None,
        root_id: str | None = None,
        policy: str | None = None,
        search: str | None = None,
        metadata_incomplete: bool | None = None,
        candidate_available: bool | None = None,
        job_state: str | None = None,
        sort: str = "newest",
        created_from: float | None = None,
        created_to: float | None = None,
        updated_from: float | None = None,
        updated_to: float | None = None,
    ) -> ReviewListResponse:
        if limit < 1 or limit > MAX_PAGE_SIZE:
            raise ValidationError(
                f"Review page size must be between 1 and {MAX_PAGE_SIZE}."
            )
        allowed_sorts = {
            "newest",
            "oldest",
            "album",
            "artist",
            "root",
            "track_count",
            "reason",
        }
        if sort not in allowed_sorts:
            raise ValidationError("The requested review sort is not supported.")
        if state is not None and state not in REVIEW_STATES:
            raise ValidationError("The requested review state is not supported.")
        if policy is not None and policy not in REVIEW_POLICIES:
            raise ValidationError("The requested library policy is not supported.")
        if job_state is not None and job_state not in ACTIVE_JOB_STATES:
            raise ValidationError(
                "The requested identification job state is not supported."
            )
        if (
            created_from is not None
            and created_to is not None
            and created_from > created_to
        ):
            raise ValidationError("The review creation time range is invalid.")
        if (
            updated_from is not None
            and updated_to is not None
            and updated_from > updated_to
        ):
            raise ValidationError("The review update time range is invalid.")
        if search is not None and len(search) > 200:
            raise ValidationError("The review search is too long.")
        cursor_updated_at, cursor_id = _decode_cursor(cursor, sort)
        result = await self._store.list_identification_reviews(
            limit=limit,
            cursor_updated_at=cursor_updated_at,
            cursor_id=cursor_id,
            sort=sort,
            state=state,
            reason_code=reason_code,
            root_id=root_id,
            policy=policy,
            search=search,
            metadata_incomplete=metadata_incomplete,
            candidate_available=candidate_available,
            job_state=job_state,
            created_from=created_from,
            created_to=created_to,
            updated_from=updated_from,
            updated_to=updated_to,
        )
        items = [self._to_list_item(row) for row in result["rows"]]
        next_cursor = None
        if result["has_more"] and items:
            next_cursor = _encode_cursor(
                sort, result["rows"][-1]["cursor_value"], items[-1].id
            )
        return ReviewListResponse(
            items=items,
            next_cursor=next_cursor,
            has_more=bool(result["has_more"]),
            filtered_total=int(result["filtered_total"]),
            counts_by_state={
                str(key): int(value) for key, value in result["counts_by_state"].items()
            },
            counts_by_reason={
                str(key): int(value)
                for key, value in result["counts_by_reason"].items()
            },
            catalog_revision=await self._store.get_catalog_revision(),
        )

    async def detail(self, review_id: str) -> ReviewDetailResponse:
        detail = await self._store.get_identification_review_detail(review_id)
        if detail is None:
            raise ResourceNotFoundError("Review item not found.")
        review = detail["review"]
        album = detail["album"] or {}
        tracks = detail["tracks"]
        first_track = tracks[0] if tracks else {}
        projection = self._to_list_item(
            {
                **review,
                "album_title": album.get("title", first_track.get("album_title", "")),
                "album_artist_name": album.get(
                    "album_artist_name", first_track.get("album_artist_name", "")
                ),
                "year": album.get("year", first_track.get("year")),
                "root_id": album.get("root_id", first_track.get("root_id", "")),
                "relative_path": first_track.get("relative_path", ""),
                "track_count": len(tracks),
                "metadata_incomplete_count": sum(
                    bool(track["metadata_incomplete"]) for track in tracks
                ),
                "effective_policy": first_track.get("applied_policy", "automatic"),
                "manual_excluded": first_track.get("manual_excluded", 0),
                "release_group_mbid": (detail["identity"] or {}).get(
                    "release_group_mbid"
                ),
                "identity_source": (detail["identity"] or {}).get("decision_source"),
                "candidate_count": detail["attempts"][0]["candidate_count"]
                if detail["attempts"]
                else 0,
                "active_job_state": (detail["job"] or {}).get("state"),
            }
        )
        evidence: CandidateEvidence | None = None
        evidence_revision = ""
        candidates: list[ReviewCandidateDetail] = []
        selected_candidate_key = (
            detail["attempts"][0]["selected_candidate_key"]
            if detail["attempts"]
            else None
        )
        selected_evidence: CandidateEvidence | None = None
        selected_evidence_revision = ""
        for raw in detail["evidence"]:
            candidate = msgspec.json.decode(
                bytes(raw["evidence_json"]), type=CandidateEvidence
            )
            candidates.append(
                ReviewCandidateDetail(
                    candidate_key=str(raw["candidate_key"]),
                    evidence_revision=str(raw["id"]),
                    evidence=candidate,
                    automatic_safe=candidate.reason_code
                    in AUTOMATIC_SAFE_EVIDENCE_REASONS,
                )
            )
            if raw["candidate_key"] == selected_candidate_key:
                selected_evidence = candidate
                selected_evidence_revision = str(raw["id"])
            if evidence is None:
                evidence = candidate
                evidence_revision = str(raw["id"])
        if selected_evidence is not None:
            evidence = selected_evidence
            evidence_revision = selected_evidence_revision
        candidates.sort(
            key=lambda item: (
                not item.automatic_safe,
                -item.evidence.score,
                item.candidate_key,
            )
        )
        history = [
            ReviewHistoryItem(
                id=str(attempt["id"]),
                kind="attempt",
                state=str(attempt["state"]),
                reason_code=str(attempt["terminal_reason_code"]),
                created_at=float(attempt["completed_at"]),
                actor_user_id=attempt["requested_by_user_id"],
            )
            for attempt in detail["attempts"]
        ]
        history.extend(
            ReviewHistoryItem(
                id=str(action["id"]),
                kind="action",
                state=str(action["action_kind"]),
                reason_code=str(action["reason_code"] or ""),
                created_at=float(action["created_at"]),
                actor_user_id=action["actor_user_id"],
            )
            for action in detail["actions"]
        )
        actions = self._available_actions(projection, detail["identity"], evidence)
        return ReviewDetailResponse(
            review=projection,
            tracks=[
                ReviewTrackDetail(
                    id=str(track["id"]),
                    title=str(track["title"]),
                    artist_name=str(track["artist_name"] or ""),
                    local_artist_id=track["local_artist_id"],
                    relative_path=str(track["relative_path"]),
                    disc_number=int(track["disc_number"]),
                    track_number=int(track["track_number"]),
                    availability=str(track["availability"]),
                    membership_locked=bool(track["membership_locked"]),
                    recording_mbid=track["recording_mbid"],
                )
                for track in tracks
            ],
            current_evidence=evidence,
            candidates=candidates,
            supported=[]
            if evidence is None
            else [
                item
                for item in evidence.track_evidence
                if item.classification == "supported"
            ],
            unknown=[]
            if evidence is None
            else [
                item
                for item in evidence.track_evidence
                if item.classification == "unknown"
            ],
            contradictory=[]
            if evidence is None
            else [
                item
                for item in evidence.track_evidence
                if item.classification == "contradictory"
            ],
            history=history,
            available_actions=actions,
            catalog_revision=await self._store.get_catalog_revision(),
            album_revision=album.get("row_revision"),
            identity_revision=(
                int(detail["identity"]["row_revision"])
                if detail["identity"] is not None
                else None
            ),
            input_revision=":".join(album_input_revisions(tracks)),
            evidence_revision=evidence_revision,
            job_revision=(detail["job"] or {}).get("row_revision"),
        )

    async def act(
        self,
        review_id: str,
        action: str,
        request: ReviewActionRequest,
        actor_user_id: str,
        *,
        now: float | None = None,
    ) -> ReviewActionResponse:
        timestamp = time.time() if now is None else now
        if action in {"detach_keep_tagged", "exclude"} and not request.confirmation:
            raise ValidationError("Confirm this catalog change before applying it.")
        result = await self._store.apply_review_decision(
            review_id,
            action=action,
            actor_user_id=actor_user_id,
            expected_review_revision=request.expected_review_revision,
            expected_catalog_revision=request.expected_catalog_revision,
            expected_identity_revision=request.expected_identity_revision,
            action_id=str(uuid.uuid4()),
            idempotency_key=request.idempotency_key,
            now=timestamp,
        )
        review = result["review"]
        return ReviewActionResponse(
            review_id=review_id,
            state=str(review["state"]),
            row_revision=int(review["row_revision"]),
            catalog_revision=int(result["catalog_revision"]),
            action_id=str(result["action_id"]),
            remaining_exclusion_source=result.get("remaining_exclusion_source"),
        )

    async def accept_candidate(
        self,
        review_id: str,
        request: CandidateAcceptanceRequest,
        actor_user_id: str,
        *,
        now: float | None = None,
    ) -> ReviewActionResponse:
        if not request.confirmation:
            raise ValidationError(
                "Confirm the candidate and its evidence before applying it."
            )
        if not request.expected_evidence_revision:
            raise ValidationError(
                "Candidate acceptance requires the evidence revision."
            )
        result = await self._store.accept_review_candidate(
            review_id,
            candidate_key=request.candidate_key,
            manual_override=request.manual_override,
            actor_user_id=actor_user_id,
            expected_review_revision=request.expected_review_revision,
            expected_catalog_revision=request.expected_catalog_revision,
            expected_evidence_revision=request.expected_evidence_revision,
            action_id=str(uuid.uuid4()),
            idempotency_key=request.idempotency_key,
            now=time.time() if now is None else now,
        )
        review = result["review"]
        return ReviewActionResponse(
            review_id=review_id,
            state=str(review["state"]),
            row_revision=int(review["row_revision"]),
            catalog_revision=int(result["catalog_revision"]),
            action_id=str(result["action_id"]),
        )

    async def preview_bulk(
        self, request: BulkReviewPreviewRequest, *, now: float | None = None
    ) -> BulkReviewPreviewResponse:
        timestamp = time.time() if now is None else now
        normalized_filter = self._resolve_selection_filter(
            request.selection.normalized_filter
        )
        selection = msgspec.to_builtins(request.selection)
        selection["normalized_filter"] = normalized_filter
        selection["candidate_key"] = request.candidate_key
        token = _preview_token(
            request.action, selection, int(timestamp), uuid.uuid4().hex
        )
        try:
            await self._store.create_bulk_review_preview(
                review_ids=request.selection.review_ids,
                normalized_filter=normalized_filter,
                preview_token=token,
                action=request.action,
                selection=selection,
                catalog_revision=request.selection.catalog_revision,
                created_at=timestamp,
                expires_at=timestamp + PREVIEW_TTL_SECONDS,
            )
            while True:
                staged = await self._store.stage_bulk_review_preview_batch(token)
                if staged["complete"]:
                    summary = staged["summary"]
                    break
        except ValueError as error:
            raise ValidationError(str(error)) from error
        return BulkReviewPreviewResponse(
            preview_token=token,
            action=request.action,
            **summary,
        )

    async def apply_bulk(
        self,
        request: BulkReviewApplyRequest,
        actor_user_id: str,
        *,
        now: float | None = None,
    ) -> OperationResponse:
        timestamp = time.time() if now is None else now
        normalized_filter = self._resolve_selection_filter(
            request.selection.normalized_filter
        )
        selection = msgspec.to_builtins(request.selection)
        selection["normalized_filter"] = normalized_filter
        selection["candidate_key"] = request.candidate_key
        _validate_preview_token(
            request.preview_token, request.action, selection, timestamp
        )
        action = request.action
        if action == "accept_candidate":
            if not request.candidate_key:
                raise ValidationError(
                    "Choose one candidate before applying this action."
                )
            action = f"accept_candidate:{request.candidate_key}"
        job = OperationJob(
            id=str(uuid.uuid4()),
            kind="bulk_review_apply",
            requested_by_user_id=actor_user_id,
            input_catalog_revision=request.selection.catalog_revision,
            idempotency_key=request.idempotency_key,
            created_at=timestamp,
        )
        try:
            raw = await self._store.materialize_bulk_review_operation(
                job,
                action=action,
                review_ids=request.selection.review_ids,
                expected_revisions=request.selection.expected_revisions,
                normalized_filter=normalized_filter,
                preview_token=request.preview_token,
                preview_action=request.action,
                candidate_key=request.candidate_key,
                confirm_local_metadata=request.confirm_local_metadata,
                created_at=timestamp,
            )
        except ValueError as error:
            raise ValidationError(str(error)) from error
        return OperationResponse(
            **{
                key: raw[key]
                for key in OperationResponse.__struct_fields__
                if key in raw
            }
        )

    @staticmethod
    def _to_list_item(row: dict[str, Any]) -> ReviewListItem:
        exclusion_source = None
        if str(row.get("effective_policy", "")) == "excluded":
            exclusion_source = "directory_policy"
        if bool(row.get("manual_excluded")):
            exclusion_source = "item_decision"
        return ReviewListItem(
            id=str(row["id"]),
            state=str(row["state"]),
            reason_code=str(row["reason_code"]),
            local_album_id=row.get("local_album_id"),
            local_track_id=row.get("local_track_id"),
            album_title=str(row.get("album_title") or ""),
            album_artist_name=str(row.get("album_artist_name") or ""),
            year=row.get("year"),
            track_count=int(row.get("track_count") or 0),
            metadata_incomplete_count=int(row.get("metadata_incomplete_count") or 0),
            root_id=str(row.get("root_id") or ""),
            relative_path=str(row.get("relative_path") or ""),
            effective_policy=str(row.get("effective_policy") or "automatic"),
            exclusion_source=exclusion_source,
            release_group_mbid=row.get("release_group_mbid"),
            identity_source=row.get("identity_source"),
            candidate_count=int(row.get("candidate_count") or 0),
            evidence_summary={},
            active_job_state=row.get("active_job_state"),
            created_at=float(row["created_at"]),
            updated_at=float(row["updated_at"]),
            row_revision=int(row["row_revision"]),
        )

    @staticmethod
    def _available_actions(
        review: ReviewListItem,
        identity: dict[str, Any] | None,
        evidence: CandidateEvidence | None,
    ) -> list[str]:
        if review.state == "excluded" or review.effective_policy == "excluded":
            return ["restore"] if review.exclusion_source == "item_decision" else []
        actions = ["exclude", "retry"]
        if identity is None:
            actions.append("keep_tagged")
        else:
            actions.append("detach_keep_tagged")
        if evidence is not None:
            actions.append(
                "accept_candidate"
                if evidence.reason_code in AUTOMATIC_SAFE_EVIDENCE_REASONS
                else "manual_candidate_override"
            )
        return actions
