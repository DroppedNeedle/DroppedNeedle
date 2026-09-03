"""Staged target scan APIs; mounted only after the authorized offline cutover."""

from __future__ import annotations

import time
from pathlib import PurePosixPath

from fastapi import APIRouter, Query
from fastapi.responses import StreamingResponse

from api.v1.schemas.library_scan_target import (
    DeferredIdentificationJobSummary,
    IdentificationControlRequestBody,
    IdentificationControlResponse,
    LibraryActivityItem,
    LibraryActivityResponse,
    ScanControlRequestBody,
    ScanControlResponse,
    ScanEstimateResponse,
    ScanRunCurrentResponse,
    ScanRunDetailResponse,
    ScanRunFailureItem,
    ScanRunFailuresResponse,
    ScanRunHistoryResponse,
    ScanRunRequestBody,
    ScanRunRequestedResponse,
)
from core.dependencies import (
    LibraryAdministrativeWorkServiceDep,
    LibraryPolicyReconciliationServiceDep,
    LibraryPolicyResolverDep,
    MbProviderAvailabilityDep,
    TargetIdentificationQueueDep,
    TargetLibraryScanCoordinatorDep,
)
from core.exceptions import ValidationError
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from middleware import CurrentAdminDep, CurrentUserDep
from models.library_work import LibraryWorkItem, ScanRequest, ScanScope
from services.native.library_activity_events import activity_events

router = APIRouter(
    route_class=MsgSpecRoute, prefix="/library", tags=["library-scan-target"]
)

_IDENTIFICATION_PRIORITY_LABELS = {
    20: "New and changed albums",
    30: "Administrator retries",
    40: "Existing-library backlog",
    50: "Supporting maintenance",
}


def _selected_scopes(
    body: ScanRunRequestBody, resolver: LibraryPolicyResolverDep
) -> list[ScanScope]:
    if body.expected_policy_revision != resolver.policy_revision:
        raise ValidationError(
            "The library policy changed. Refresh this page and try again."
        )
    selected = set(body.scope_ids)
    candidates: list[ScanScope] = []
    matched: set[str] = set()
    for root in resolver.settings.library_roots:
        selected_rules = [rule for rule in root.rules if rule.id in selected]
        matched.update(rule.id for rule in selected_rules)
        if root.id in selected:
            matched.add(root.id)
        if not selected or root.id in selected:
            candidates.append(
                ScanScope(
                    root_id=root.id,
                    scope_id=root.id,
                    relative_path=".",
                    effective_policy=root.policy,
                    policy_revision=resolver.policy_revision,
                )
            )
            continue
        for rule in root.rules:
            if rule.id in selected:
                candidates.append(
                    ScanScope(
                        root_id=root.id,
                        scope_id=rule.id,
                        relative_path=rule.relative_path,
                        effective_policy=rule.policy,
                        policy_revision=resolver.policy_revision,
                    )
                )
    if selected and matched != selected:
        raise ValidationError("One or more selected library scopes no longer exist.")
    scopes: list[ScanScope] = []
    for candidate in sorted(
        candidates,
        key=lambda scope: (
            scope.root_id,
            len(PurePosixPath(scope.relative_path).parts),
            scope.relative_path,
        ),
    ):
        candidate_path = PurePosixPath(candidate.relative_path)
        if any(
            existing.root_id == candidate.root_id
            and (
                existing.relative_path == "."
                or candidate_path.is_relative_to(PurePosixPath(existing.relative_path))
            )
            for existing in scopes
        ):
            continue
        scopes.append(candidate)
    return scopes


IDENTIFICATION_DRAIN_WORK_ITEM_ID = "identification-drain"
"""Synthetic work-item id for the post-scan identification drain.

Never persisted: scan-run history/detail/failure lookups hit the scan-runs
tables (get_scan_run raises ResourceNotFoundError for unknown ids), so the
synthetic id 404s there instead of resolving. Only the activity work_items
list ever carries it, flagged with synthetic=True.
"""

@router.get("/activity", response_model=LibraryActivityResponse)
async def library_activity(
    current_user: CurrentUserDep,
    coordinator: TargetLibraryScanCoordinatorDep,
    identification: TargetIdentificationQueueDep,
    administrative_work: LibraryAdministrativeWorkServiceDep,
    mb_provider_available: MbProviderAvailabilityDep,
) -> LibraryActivityResponse:
    revisions = await identification.stream_revisions()
    runs = await coordinator.current()
    recent_history = await coordinator.history(limit=1)
    latest_failure = next(
        (
            run
            for run in recent_history
            if run.state == "failed"
            and run.terminal_at is not None
            and time.time() - run.terminal_at < 24 * 60 * 60
        ),
        None,
    )
    items: list[LibraryActivityItem] = []
    work_items: list[LibraryWorkItem] = []
    for run_index, run in enumerate(runs):
        snapshot = await coordinator.snapshot(run.id)
        discovering = run.state == "discovering"
        total = (
            None
            if discovering
            else snapshot.counters.get("total_count")
            or snapshot.counters.get("discovered_count")
        )
        processed = snapshot.counters.get(
            "discovered_count" if discovering else "inspected_count", 0
        )
        phase = str(getattr(run, "phase", run.state))
        work_processed = (
            total if phase == "reconciling" and total is not None else processed
        )
        if run_index == 0:
            items.append(
                LibraryActivityItem(
                    kind="scan",
                    state=run.state,
                    label="Updating the local library",
                    processed=processed,
                    total=total,
                    indeterminate=discovering or not bool(total),
                    updated_at=run.updated_at,
                    started_at=run.started_at,
                    failure_event_id=latest_failure.id if latest_failure else None,
                    failure_at=latest_failure.terminal_at if latest_failure else None,
                )
            )
        work_items.append(
            LibraryWorkItem(
                id=str(run.id),
                kind="scan",
                state=str(run.state),
                phase=phase,
                effect="catalog_only",
                processed=int(work_processed or 0),
                total=int(total) if total is not None else None,
                unit="files",
                indeterminate=discovering or not bool(total),
                started_at=run.started_at,
                updated_at=float(run.updated_at),
                scope_label=(
                    "Whole library"
                    if current_user.role == "admin"
                    and str(getattr(run, "aggregate_scope", "")) == "all"
                    else "Selected library scopes"
                    if current_user.role == "admin"
                    else None
                ),
                new_count=int(snapshot.counters.get("new_count", 0)),
                changed_count=int(snapshot.counters.get("changed_count", 0)),
                missing_count=int(snapshot.counters.get("missing_count", 0)),
                failed_count=int(snapshot.counters.get("errored_count", 0)),
                priority=20 if run_index == 0 and run.state != "queued" else 70,
            )
        )
    if runs and latest_failure is not None:
        failed_snapshot = await coordinator.snapshot(latest_failure.id)
        failed_total = failed_snapshot.counters.get(
            "total_count"
        ) or failed_snapshot.counters.get("discovered_count")
        work_items.append(
            LibraryWorkItem(
                id=str(latest_failure.id),
                kind="scan",
                state="failed",
                phase=str(getattr(latest_failure, "phase", "failed")),
                effect="attention",
                processed=int(failed_snapshot.counters.get("inspected_count", 0)),
                total=int(failed_total) if failed_total is not None else None,
                unit="files",
                indeterminate=not bool(failed_total),
                started_at=latest_failure.started_at,
                updated_at=float(latest_failure.updated_at),
                failed_count=int(failed_snapshot.counters.get("errored_count", 0)),
                priority=0,
                failure_event_id=str(latest_failure.id),
                failure_at=float(latest_failure.terminal_at),
            )
        )
    if not runs and latest_failure is not None:
        snapshot = await coordinator.snapshot(latest_failure.id)
        total = snapshot.counters.get("total_count") or snapshot.counters.get(
            "discovered_count"
        )
        items.append(
            LibraryActivityItem(
                kind="scan",
                state="failed",
                label="Updating the local library",
                processed=snapshot.counters.get("inspected_count", 0),
                total=total,
                indeterminate=not bool(total),
                updated_at=latest_failure.updated_at,
                started_at=latest_failure.started_at,
                failure_event_id=latest_failure.id,
                failure_at=latest_failure.terminal_at,
            )
        )
        work_items.append(
            LibraryWorkItem(
                id=str(latest_failure.id),
                kind="scan",
                state="failed",
                phase=str(getattr(latest_failure, "phase", "failed")),
                effect="attention",
                processed=int(snapshot.counters.get("inspected_count", 0)),
                total=int(total) if total is not None else None,
                unit="files",
                indeterminate=not bool(total),
                started_at=latest_failure.started_at,
                updated_at=float(latest_failure.updated_at),
                failed_count=int(snapshot.counters.get("errored_count", 0)),
                priority=0,
                failure_event_id=str(latest_failure.id),
                failure_at=float(latest_failure.terminal_at),
            )
        )
    identification_snapshot = await identification.activity_snapshot()
    counts = identification_snapshot["counts"]
    waiting = sum(counts.get(state, 0) for state in ("queued", "running", "paused"))
    foreground_operations = identification_snapshot["foreground_operation_count"]
    completed = sum(
        counts.get(state, 0) for state in ("succeeded", "needs_review", "failed")
    )
    if (
        waiting
        or foreground_operations
        or identification_snapshot["failure_event_id"] is not None
    ):
        control_state = identification_snapshot["control_state"]
        if control_state == "paused" and counts.get("running", 0):
            state = "pausing"
        elif control_state == "paused":
            state = "paused"
        elif counts.get("running", 0) or identification_snapshot["claimable_count"]:
            state = "running"
        elif identification_snapshot["failure_event_id"] is not None:
            state = "failed"
        else:
            state = "idle"
        total = completed + waiting
        active_priority = identification_snapshot.get("active_priority")
        items.append(
            LibraryActivityItem(
                kind="identification",
                state=state,
                label="Identifying albums",
                processed=completed,
                total=total or None,
                indeterminate=not bool(total),
                updated_at=float(
                    identification_snapshot["updated_at"]
                    or identification_snapshot["failure_at"]
                    or 0.0
                ),
                started_at=identification_snapshot["started_at"],
                waiting_count=waiting,
                identified_count=counts.get("succeeded", 0),
                kept_local_count=identification_snapshot.get("kept_local_count", 0),
                needs_review_count=counts.get("needs_review", 0),
                failed_count=counts.get("failed", 0),
                deferred_count=identification_snapshot["deferred_count"],
                deferred_reason_counts=identification_snapshot[
                    "deferred_reason_counts"
                ],
                deferred_jobs=[
                    DeferredIdentificationJobSummary(**row)
                    for row in identification_snapshot.get("deferred_jobs", [])
                ],
                attention_count=identification_snapshot["attention_count"],
                priority_band=(
                    _IDENTIFICATION_PRIORITY_LABELS.get(active_priority, "Queued work")
                    if active_priority is not None
                    else None
                ),
                oldest_backlog_at=identification_snapshot["started_at"],
                provider_unavailable=not mb_provider_available(),
                control_revision=identification_snapshot["control_revision"],
                failure_event_id=identification_snapshot["failure_event_id"],
                failure_at=identification_snapshot["failure_at"],
                foreground_operation_count=foreground_operations,
            )
        )
        if waiting or identification_snapshot["failure_event_id"] is not None:
            work_items.append(
                LibraryWorkItem(
                    id="identification",
                    kind="identification",
                    state=state,
                    phase="identifying_albums",
                    mode=(
                        _IDENTIFICATION_PRIORITY_LABELS.get(
                            active_priority, "Queued work"
                        )
                        if active_priority is not None
                        else None
                    ),
                    effect=(
                        "attention"
                        if state == "failed" and not waiting
                        else "catalog_only"
                    ),
                    processed=0,
                    total=None,
                    unit="albums",
                    indeterminate=bool(waiting),
                    remaining_count=waiting,
                    started_at=identification_snapshot["started_at"],
                    updated_at=float(
                        identification_snapshot["updated_at"]
                        or identification_snapshot["failure_at"]
                        or 0.0
                    ),
                    warning_count=int(identification_snapshot["deferred_count"]),
                    failed_count=int(counts.get("failed", 0)),
                    priority=0 if state == "failed" and not waiting else 90,
                    failure_event_id=(
                        identification_snapshot["failure_event_id"]
                        if state == "failed" and not waiting
                        else None
                    ),
                    failure_at=(
                        identification_snapshot["failure_at"]
                        if state == "failed" and not waiting
                        else None
                    ),
                )
            )
            if waiting and identification_snapshot["failure_event_id"] is not None:
                work_items.append(
                    LibraryWorkItem(
                        id=(
                            "identification-failure:"
                            + str(identification_snapshot["failure_event_id"])
                        ),
                        kind="identification",
                        state="failed",
                        phase="identifying_albums",
                        effect="attention",
                        processed=0,
                        total=None,
                        unit="albums",
                        indeterminate=True,
                        updated_at=float(
                            identification_snapshot["failure_at"]
                            or identification_snapshot["updated_at"]
                            or 0.0
                        ),
                        failed_count=int(counts.get("failed", 0)),
                        priority=0,
                        failure_event_id=identification_snapshot["failure_event_id"],
                        failure_at=identification_snapshot["failure_at"],
                    )
                )
    if not runs:
        deferred = int(identification_snapshot["deferred_count"])
        if waiting + deferred > 0:
            drain_control_state = identification_snapshot["control_state"]
            if drain_control_state == "paused" and counts.get("running", 0):
                drain_state = "pausing"
            elif drain_control_state == "paused":
                drain_state = "paused"
            elif counts.get("running", 0) or identification_snapshot["claimable_count"]:
                drain_state = "running"
            else:
                drain_state = "idle"
            work_items.append(
                LibraryWorkItem(
                    id=IDENTIFICATION_DRAIN_WORK_ITEM_ID,
                    kind="identification",
                    state=drain_state,
                    phase="awaiting_identification",
                    effect="catalog_only",
                    processed=0,
                    total=None,
                    unit="albums",
                    indeterminate=True,
                    remaining_count=waiting,
                    started_at=identification_snapshot["started_at"],
                    updated_at=float(
                        identification_snapshot["updated_at"]
                        or identification_snapshot["failure_at"]
                        or 0.0
                    ),
                    warning_count=deferred,
                    failed_count=int(counts.get("failed", 0)),
                    priority=90,
                    synthetic=True,
                    catalog_settled=False,
                    pending_identification=waiting,
                )
            )
    if current_user.role == "admin":
        work_items.extend(await administrative_work.active())
    work_items.sort(key=lambda item: (item.priority, -item.updated_at, item.id))
    return LibraryActivityResponse(
        items=items, work_items=work_items, revisions=revisions
    )


@router.get("/activity/stream")
async def library_activity_stream(
    _: CurrentUserDep,
    identification: TargetIdentificationQueueDep,
):
    return StreamingResponse(
        activity_events(identification),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache",
            "Connection": "keep-alive",
            "X-Accel-Buffering": "no",
        },
    )


async def _identification_control_response(
    identification: TargetIdentificationQueueDep,
    revision: int,
) -> IdentificationControlResponse:
    snapshot = await identification.activity_snapshot()
    counts = snapshot["counts"]
    state = str(snapshot["control_state"])
    if state == "paused" and counts.get("running", 0):
        state = "pausing"
    return IdentificationControlResponse(state=state, row_revision=revision)


@router.post("/identification/pause", response_model=IdentificationControlResponse)
async def pause_identification(
    admin: CurrentAdminDep,
    identification: TargetIdentificationQueueDep,
    body: IdentificationControlRequestBody = MsgSpecBody(
        IdentificationControlRequestBody
    ),
) -> IdentificationControlResponse:
    revision = await identification.pause(
        admin.id, expected_revision=body.expected_revision
    )
    return await _identification_control_response(identification, revision)


@router.post("/identification/resume", response_model=IdentificationControlResponse)
async def resume_identification(
    _: CurrentAdminDep,
    identification: TargetIdentificationQueueDep,
    body: IdentificationControlRequestBody = MsgSpecBody(
        IdentificationControlRequestBody
    ),
) -> IdentificationControlResponse:
    revision = await identification.resume(expected_revision=body.expected_revision)
    return await _identification_control_response(identification, revision)


@router.get("/operations/stream")
async def library_operations_stream(
    _: CurrentAdminDep,
    identification: TargetIdentificationQueueDep,
):
    return await library_activity_stream(_, identification)


@router.post("/scan-runs", response_model=ScanRunRequestedResponse, status_code=202)
async def request_scan_run(
    current_admin: CurrentAdminDep,
    coordinator: TargetLibraryScanCoordinatorDep,
    resolver: LibraryPolicyResolverDep,
    reconciliation: LibraryPolicyReconciliationServiceDep,
    body: ScanRunRequestBody = MsgSpecBody(ScanRunRequestBody),
) -> ScanRunRequestedResponse:
    if body.kind == "policy_reconcile":
        # F-TARGETCATALOG-02: a policy Apply must run the frozen pending
        # transition scopes (removed roots, deleted rules, same-ID re-paths)
        # through the reconciliation service instead of rebuilding scopes from
        # current settings. The service owns the expected-revision gate and
        # keeps trigger="policy_apply".
        result = await reconciliation.apply(
            body.scope_ids,
            expected_policy_revision=body.expected_policy_revision,
            requested_by_user_id=current_admin.id,
        )
    else:
        result = await coordinator.request_run(
            ScanRequest(
                kind=body.kind,
                trigger="manual",
                scopes=_selected_scopes(body, resolver),
                requested_by_user_id=current_admin.id,
                policy_revision=resolver.policy_revision,
            )
        )
    return ScanRunRequestedResponse(
        run_id=result.run_id,
        disposition=result.disposition,
        state=result.state,
        row_revision=result.row_revision,
        queued_reason=result.queued_reason,
        conflicting_kind=result.conflicting_kind,
    )


@router.get("/scan-runs/current", response_model=ScanRunCurrentResponse)
async def current_scan_runs(
    _: CurrentAdminDep,
    coordinator: TargetLibraryScanCoordinatorDep,
) -> ScanRunCurrentResponse:
    runs = await coordinator.current()
    return ScanRunCurrentResponse(
        active=next((run for run in runs if run.state != "queued"), None),
        queued=next((run for run in runs if run.state == "queued"), None),
    )


@router.get("/scan-runs", response_model=ScanRunHistoryResponse)
async def scan_run_history(
    _: CurrentAdminDep,
    coordinator: TargetLibraryScanCoordinatorDep,
    limit: int = Query(default=50, ge=1, le=50),
    cursor: str | None = Query(default=None),
) -> ScanRunHistoryResponse:
    items, next_cursor = await coordinator.history_page(limit=limit, cursor=cursor)
    return ScanRunHistoryResponse(items=items, next_cursor=next_cursor)


@router.get("/scan-runs/estimate", response_model=ScanEstimateResponse)
async def estimate_scan_run(
    _: CurrentAdminDep,
    coordinator: TargetLibraryScanCoordinatorDep,
    resolver: LibraryPolicyResolverDep,
    scope_ids: list[str] = Query(default=[]),
) -> ScanEstimateResponse:
    scopes = _selected_scopes(
        ScanRunRequestBody(
            scope_ids=scope_ids,
            expected_policy_revision=resolver.policy_revision,
        ),
        resolver,
    )
    count, estimated_at = await coordinator.estimate(scopes)
    return ScanEstimateResponse(
        approximate=True,
        estimated_file_count=count,
        estimated_at=estimated_at,
    )


@router.get("/scan-runs/{run_id}", response_model=ScanRunDetailResponse)
async def scan_run_detail(
    run_id: str,
    _: CurrentAdminDep,
    coordinator: TargetLibraryScanCoordinatorDep,
) -> ScanRunDetailResponse:
    return ScanRunDetailResponse(snapshot=await coordinator.snapshot(run_id))


@router.get("/scan-runs/{run_id}/failures", response_model=ScanRunFailuresResponse)
async def scan_run_failures(
    run_id: str,
    _: CurrentAdminDep,
    coordinator: TargetLibraryScanCoordinatorDep,
    limit: int = Query(default=50, ge=1, le=200),
    cursor: int | None = Query(default=None, ge=1),
) -> ScanRunFailuresResponse:
    items, next_cursor = await coordinator.scan_run_failures(
        run_id, limit=limit, cursor_rowid=cursor
    )
    return ScanRunFailuresResponse(
        items=[
            ScanRunFailureItem(
                root_id=item.root_id,
                relative_path=item.relative_path,
                failure_code=item.failure_code,
                failure_detail=item.failure_detail,
                phase=item.phase,
                recorded_at=item.recorded_at,
            )
            for item in items
        ],
        next_cursor=next_cursor,
    )


async def _control(
    run_id: str,
    action: str,
    body: ScanControlRequestBody,
    coordinator: TargetLibraryScanCoordinatorDep,
) -> ScanControlResponse:
    result = await coordinator.control(run_id, action, body.expected_revision)
    return ScanControlResponse(
        run_id=result.run_id,
        state=result.state,
        row_revision=result.row_revision,
        event_revision=result.event_revision,
        stream_revision=result.stream_revision,
    )


@router.post("/scan-runs/{run_id}/pause", response_model=ScanControlResponse)
async def pause_scan_run(
    run_id: str,
    _: CurrentAdminDep,
    coordinator: TargetLibraryScanCoordinatorDep,
    body: ScanControlRequestBody = MsgSpecBody(ScanControlRequestBody),
) -> ScanControlResponse:
    return await _control(run_id, "pause", body, coordinator)


@router.post("/scan-runs/{run_id}/resume", response_model=ScanControlResponse)
async def resume_scan_run(
    run_id: str,
    _: CurrentAdminDep,
    coordinator: TargetLibraryScanCoordinatorDep,
    body: ScanControlRequestBody = MsgSpecBody(ScanControlRequestBody),
) -> ScanControlResponse:
    return await _control(run_id, "resume", body, coordinator)


@router.post("/scan-runs/{run_id}/stop", response_model=ScanControlResponse)
async def stop_scan_run(
    run_id: str,
    _: CurrentAdminDep,
    coordinator: TargetLibraryScanCoordinatorDep,
    body: ScanControlRequestBody = MsgSpecBody(ScanControlRequestBody),
) -> ScanControlResponse:
    return await _control(run_id, "stop", body, coordinator)
