import logging
import math
import time as _time
from collections.abc import Callable, Coroutine
from datetime import datetime, timezone
from typing import Any, Optional, TYPE_CHECKING

from api.v1.schemas.requests_page import (
    ActiveRequestItem,
    ActiveRequestsResponse,
    CancelRequestResponse,
    RequestHistoryItem,
    RequestHistoryResponse,
    RetryRequestResponse,
)
from core.exceptions import PermissionDeniedError, ValidationError
from infrastructure.cover_urls import prefer_release_group_cover_url
from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.persistence.request_history import (
    RequestHistoryRecord,
    RequestHistoryStore,
)
from repositories.protocols import LibraryRepositoryProtocol

if TYPE_CHECKING:
    from services.native.download_service import DownloadService

logger = logging.getLogger(__name__)

_CANCELLABLE_STATUSES = {"pending", "downloading"}
_RETRYABLE_STATUSES = {"failed", "cancelled", "incomplete"}
_CLEARABLE_STATUSES = {"imported", "incomplete", "failed", "cancelled"}

_LIBRARY_MBIDS_CACHE_TTL = 30


class RequestsPageService:
    def __init__(
        self,
        library_repo: LibraryRepositoryProtocol,
        request_history: RequestHistoryStore,
        library_mbids_fn: Callable[..., Coroutine[Any, Any, set[str]]],
        on_import_callback: Callable[[RequestHistoryRecord], Coroutine[Any, Any, None]]
        | None = None,
        get_download_service: Optional[Callable[[], "DownloadService"]] = None,
        download_store=None,  # DownloadStore | None - native reconciler source of truth
        acquisition=None,  # noqa: ANN001 - AcquisitionDispatcher | None
    ):
        self._library_repo = library_repo
        self._request_history = request_history
        self._library_mbids_fn = library_mbids_fn
        self._on_import_callback = on_import_callback
        # Resolve the DownloadService fresh at every dispatch: a settings save rebuilds
        # its singleton, so capturing an instance would ignore a saved quality change
        # until restart. Kept for cancel_task; new dispatches go via the dispatcher.
        self._get_download_service = get_download_service
        self._download_store = download_store
        # picks the download client or Free Music per approve/retry dispatch
        self._acquisition = acquisition
        self._library_mbids_cache: set[str] | None = None
        self._library_mbids_cache_time: float = 0

    async def get_active_requests(
        self, user_id: str | None = None
    ) -> ActiveRequestsResponse:
        if user_id is not None:
            active_records = (
                await self._request_history.async_get_active_requests_for_user(user_id)
            )
        else:
            active_records = await self._request_history.async_get_active_requests()
        if not active_records:
            return ActiveRequestsResponse(items=[], count=0)

        library_mbids = await self._fetch_library_mbids()

        items: list[ActiveRequestItem] = []
        for record in active_records:
            # awaiting_approval records have no download task yet
            if record.status == "awaiting_approval":
                items.append(self._build_pending_item(record))
                continue

            completed = await self._check_if_completed(record, library_mbids)
            if completed:
                continue
            items.append(self._build_pending_item(record))

        return ActiveRequestsResponse(items=items, count=len(items))

    async def get_request_history(
        self,
        page: int = 1,
        page_size: int = 20,
        status_filter: Optional[str] = None,
        sort: Optional[str] = None,
        user_id: Optional[str] = None,
    ) -> RequestHistoryResponse:
        if user_id is not None:
            records, total = await self._request_history.async_get_history_for_user(
                user_id=user_id,
                page=page,
                page_size=page_size,
                status_filter=status_filter,
                sort=sort,
            )
        else:
            records, total = await self._request_history.async_get_history(
                page=page, page_size=page_size, status_filter=status_filter, sort=sort
            )

        library_mbids = await self._fetch_library_mbids()

        task_ids = [r.download_task_id for r in records if r.download_task_id]
        reimportable: set[str] = (
            await self._download_store.get_reimportable_task_ids(task_ids)
            if self._download_store is not None
            else set()
        )

        items = [
            RequestHistoryItem(
                musicbrainz_id=r.musicbrainz_id,
                artist_name=r.artist_name,
                album_title=r.album_title,
                artist_mbid=r.artist_mbid,
                year=r.year,
                cover_url=r.cover_url,
                requested_at=datetime.fromisoformat(r.requested_at),
                completed_at=(
                    datetime.fromisoformat(r.completed_at) if r.completed_at else None
                ),
                status=r.status,
                in_library=r.musicbrainz_id.lower() in library_mbids,
                user_id=r.user_id,
                requested_by_name=r.requested_by_name,
                reviewed_by_name=r.reviewed_by_name,
                reviewed_at=(
                    datetime.fromisoformat(r.reviewed_at) if r.reviewed_at else None
                ),
                download_task_id=r.download_task_id,
                can_reimport=r.status == "failed"
                and r.download_task_id in reimportable,
                request_kind=r.request_kind,
                track_title=r.track_title,
                duration_seconds=r.duration_seconds,
                track_release_group_mbid=r.track_release_group_mbid,
            )
            for r in records
        ]

        total_pages = max(1, math.ceil(total / page_size))

        return RequestHistoryResponse(
            items=items,
            total=total,
            page=page,
            page_size=page_size,
            total_pages=total_pages,
        )

    async def get_pending_approvals(self) -> ActiveRequestsResponse:
        records = await self._request_history.async_get_pending_approvals()
        items = [self._build_pending_item(r) for r in records]
        return ActiveRequestsResponse(items=items, count=len(items))

    async def get_pending_approval_count(self) -> int:
        return await self._request_history.async_get_pending_approval_count()

    async def approve_request(
        self, musicbrainz_id: str, reviewer_id: str, reviewer_name: str | None = None
    ) -> CancelRequestResponse:
        record = await self._request_history.async_get_record(musicbrainz_id)
        if not record:
            return CancelRequestResponse(success=False, message="Request not found")
        if record.status != "awaiting_approval":
            return CancelRequestResponse(
                success=False,
                message=f"Request is not awaiting approval (status: {record.status})",
            )
        await self._request_history.async_record_review(
            musicbrainz_id, "pending", reviewer_id, reviewer_name
        )
        # approving dispatches the native pipeline directly; link the new task id
        # (the 'already_in_library' sentinel is guarded)
        if self._acquisition is not None:
            try:
                task_id = await self._dispatch_record(record, origin="user")
            except ValidationError as e:
                # A cap/quota rejection (Feature C) is not a failure of the request:
                # put it BACK in the approval queue (it would otherwise silently
                # vanish into 'failed') and tell the admin exactly why it can't start.
                await self._request_history.async_update_status(
                    musicbrainz_id, "awaiting_approval"
                )
                return CancelRequestResponse(success=False, message=str(e))
            except Exception as e:  # noqa: BLE001
                logger.error(
                    f"Failed to dispatch approved request {musicbrainz_id}: {e}"
                )
                await self._request_history.async_update_status(
                    musicbrainz_id,
                    "failed",
                    completed_at=datetime.now(timezone.utc).isoformat(),
                )
                return CancelRequestResponse(
                    success=False,
                    message=f"Approved but failed to start: {self._record_title(record)}",
                )
            from services.native.download_service import ALREADY_IN_LIBRARY

            if task_id != ALREADY_IN_LIBRARY:
                await self._request_history.async_update_download_task_id(
                    musicbrainz_id, task_id
                )
            else:
                await self._request_history.async_update_status(
                    musicbrainz_id,
                    "imported",
                    completed_at=datetime.now(timezone.utc).isoformat(),
                )
        return CancelRequestResponse(
            success=True, message=f"Approved: {self._record_title(record)}"
        )

    async def reject_request(
        self, musicbrainz_id: str, reviewer_id: str, reviewer_name: str | None = None
    ) -> CancelRequestResponse:
        record = await self._request_history.async_get_record(musicbrainz_id)
        if not record:
            return CancelRequestResponse(success=False, message="Request not found")
        if record.status != "awaiting_approval":
            return CancelRequestResponse(
                success=False,
                message=f"Request is not awaiting approval (status: {record.status})",
            )
        now_iso = datetime.now(timezone.utc).isoformat()
        await self._request_history.async_record_review(
            musicbrainz_id, "rejected", reviewer_id, reviewer_name, completed_at=now_iso
        )
        return CancelRequestResponse(
            success=True, message=f"Rejected: {self._record_title(record)}"
        )

    async def cancel_request(
        self, musicbrainz_id: str, *, user_id: str, user_role: str
    ) -> CancelRequestResponse:
        record = await self._request_history.async_get_record(musicbrainz_id)
        if not record:
            return CancelRequestResponse(success=False, message="Request not found")
        if user_role != "admin":
            if not await self._request_history.async_is_requester(
                user_id, musicbrainz_id
            ):
                raise PermissionDeniedError("Cannot cancel another user's request")
            if await self._request_history.async_requester_count(musicbrainz_id) > 1:
                await self._request_history.async_remove_requester(
                    user_id, musicbrainz_id
                )
                return CancelRequestResponse(
                    success=True,
                    message=(
                        "Removed from your requests. The shared server request "
                        "continues for another listener."
                    ),
                )

        # awaiting_approval requests never dispatched, cancel directly
        if record.status == "awaiting_approval":
            now_iso = datetime.now(timezone.utc).isoformat()
            await self._request_history.async_update_status(
                musicbrainz_id, "cancelled", completed_at=now_iso
            )
            return CancelRequestResponse(
                success=True,
                message=f"Cancelled request for {self._record_title(record)}",
            )

        if record.status not in _CANCELLABLE_STATUSES:
            return CancelRequestResponse(
                success=False,
                message=f"Cannot cancel request with status '{record.status}'",
            )

        # best-effort: a missing/already-terminal task must not block marking cancelled
        download_service = (
            self._get_download_service()
            if self._get_download_service is not None
            else None
        )
        if record.download_task_id and download_service is not None:
            try:
                await download_service.cancel_task(
                    record.download_task_id, user_id, user_role
                )
            except Exception as e:  # noqa: BLE001
                logger.warning(
                    "cancel_request: native task cancel failed for %s: %s",
                    musicbrainz_id,
                    e,
                )

        now_iso = datetime.now(timezone.utc).isoformat()
        await self._request_history.async_update_status(
            musicbrainz_id, "cancelled", completed_at=now_iso
        )

        return CancelRequestResponse(
            success=True,
            message=f"Cancelled download of {self._record_title(record)}",
        )

    async def retry_request(
        self, musicbrainz_id: str, *, user_id: str, user_role: str
    ) -> RetryRequestResponse:
        record = await self._request_history.async_get_record(musicbrainz_id)
        if not record:
            return RetryRequestResponse(success=False, message="Request not found")
        if user_role != "admin" and not await self._request_history.async_is_requester(
            user_id, musicbrainz_id
        ):
            raise PermissionDeniedError("Cannot retry another user's request")

        if record.status not in _RETRYABLE_STATUSES:
            return RetryRequestResponse(
                success=False,
                message=f"Cannot retry request with status '{record.status}'",
            )

        # re-dispatch through the native pipeline (mirrors approve_request); link the
        # new task id (sentinel-guarded)
        if self._acquisition is None:
            return RetryRequestResponse(success=False, message="Downloads unavailable")
        try:
            await self._request_history.async_update_status(musicbrainz_id, "pending")
            # A retry re-dispatches an already-recorded ask, so it is not a new
            # user request for quota purposes (CollectionManagement D20).
            task_id = await self._dispatch_record(
                record,
                origin="retry",
                fallback_user_id=user_id,
                user_id_override=user_id if user_role != "admin" else None,
            )
        except ValidationError as e:
            # cap/quota rejection: restore the pre-retry status (don't strand it as
            # a phantom 'pending') and surface the reason verbatim
            await self._request_history.async_update_status(
                musicbrainz_id, record.status
            )
            return RetryRequestResponse(success=False, message=str(e))
        except Exception as e:  # noqa: BLE001
            logger.error("Retry failed for %s: %s", musicbrainz_id, e)
            return RetryRequestResponse(success=False, message=f"Retry failed: {e}")

        from services.native.download_service import ALREADY_IN_LIBRARY

        if task_id != ALREADY_IN_LIBRARY:
            await self._request_history.async_update_download_task_id(
                musicbrainz_id, task_id
            )
        else:
            await self._request_history.async_update_status(
                musicbrainz_id,
                "imported",
                completed_at=datetime.now(timezone.utc).isoformat(),
            )
        return RetryRequestResponse(
            success=True, message=f"Re-requested {self._record_title(record)}"
        )

    async def clear_history_item(
        self, musicbrainz_id: str, *, user_id: str, user_role: str
    ) -> bool:
        record = await self._request_history.async_get_record(musicbrainz_id)
        if not record:
            return False
        # ownership checked before clearability so a non-owner gets 403, not a
        # misleading 200/False, on another user's row
        if user_role != "admin" and not await self._request_history.async_is_requester(
            user_id, musicbrainz_id
        ):
            raise PermissionDeniedError("Cannot clear another user's request")
        if record.status not in _CLEARABLE_STATUSES:
            return False
        if user_role == "admin":
            return await self._request_history.async_delete_record(musicbrainz_id)
        return await self._request_history.async_dismiss_record(user_id, musicbrainz_id)

    async def get_active_count(self, user_id: str | None = None) -> int:
        if user_id is not None:
            return await self._request_history.async_get_active_count_for_user(user_id)
        return await self._request_history.async_get_active_count()

    # download_task.status -> request_history.status
    _TASK_TO_REQUEST_STATUS = {
        "downloading": "downloading",
        "processing": "downloading",
        "completed": "imported",
        "partial": "incomplete",
        "failed": "failed",
        "cancelled": "cancelled",
    }

    async def sync_request_statuses(self) -> None:
        """Periodic safety net: reconcile each active request against its native
        download task. The orchestrator already updates a request at each terminal
        transition; this catches tasks that died without a clean transition (a crash
        or a restart mid-download) so a request never sticks on 'Pending' forever.

        Replaces the old Lidarr-queue sync (which referenced an undefined method and
        was a silent no-op)."""
        active_records = await self._request_history.async_get_active_requests()
        if not active_records:
            return
        library_mbids = await self._fetch_library_mbids()
        for record in active_records:
            try:
                await self._reconcile_request(record, library_mbids)
            except Exception:  # noqa: BLE001 - one bad record must not stop the sweep
                logger.warning("Failed to reconcile request %s", record.musicbrainz_id)

    async def _reconcile_request(
        self, record: RequestHistoryRecord, library_mbids: set[str]
    ) -> None:
        task = await self._find_download_task(record)
        if task is not None:
            mapped = self._TASK_TO_REQUEST_STATUS.get(task.status)
            if mapped and mapped != record.status:
                completed_at = (
                    datetime.now(timezone.utc).isoformat()
                    if mapped in ("imported", "failed", "cancelled")
                    else None
                )
                await self._request_history.async_update_status(
                    record.musicbrainz_id, mapped, completed_at=completed_at
                )
                if mapped == "imported":
                    await self._notify_import(record)
            return
        # No native task (older row, or an orphan flow): fall back to library presence.
        await self._check_if_completed(record, library_mbids)

    async def _find_download_task(self, record: RequestHistoryRecord):
        if self._download_store is None:
            return None
        # The link is the reliable path (set right after dispatch). The album fallback
        # only matches ACTIVE tasks, so a request whose link is missing AND whose task
        # already went terminal can't be recovered here - it then falls through to the
        # library-presence check in _reconcile_request, which is the correct backstop.
        if record.download_task_id:
            task = await self._download_store.get_task(record.download_task_id)
            if task is not None:
                return task
        return await self._download_store.get_active_task_for_album_any_user(
            record.musicbrainz_id
        )

    async def _fetch_library_mbids(self) -> set[str]:
        now = _time.monotonic()
        if (
            self._library_mbids_cache is not None
            and (now - self._library_mbids_cache_time) < _LIBRARY_MBIDS_CACHE_TTL
        ):
            return self._library_mbids_cache
        try:
            result = await self._library_mbids_fn()
            self._library_mbids_cache = result
            self._library_mbids_cache_time = now
            return result
        except Exception:  # noqa: BLE001
            if self._library_mbids_cache is not None:
                return self._library_mbids_cache
            return set()

    async def _dispatch_record(
        self,
        record: RequestHistoryRecord,
        *,
        origin: str,
        fallback_user_id: str = "",
        user_id_override: str | None = None,
    ) -> str:
        """Dispatch an approved/retried request without widening exact tracks."""
        user_id = user_id_override or record.user_id or fallback_user_id
        if record.request_kind == "track":
            if not record.track_title:
                raise ValidationError("Exact-track request is missing its track title")
            return await self._acquisition.request_track(
                user_id=user_id,
                recording_mbid=record.musicbrainz_id,
                artist_name=record.artist_name or "Unknown",
                track_title=record.track_title,
                album_title=record.album_title,
                duration_seconds=record.duration_seconds,
                release_group_mbid=record.track_release_group_mbid,
                artist_mbid=record.artist_mbid,
                release_mbid=record.release_mbid,
            )
        return await self._acquisition.request_album(
            user_id=user_id,
            release_group_mbid=record.musicbrainz_id,
            artist_name=record.artist_name or "Unknown",
            album_title=record.album_title or "Unknown",
            year=record.year,
            artist_mbid=record.artist_mbid,
            origin=origin,
            release_mbid=record.release_mbid,
            track_count_priority=RequestPriority.USER_INITIATED,
        )

    @staticmethod
    def _record_title(record: RequestHistoryRecord) -> str:
        if record.request_kind == "track" and record.track_title:
            return record.track_title
        return record.album_title

    @staticmethod
    def _build_pending_item(record: RequestHistoryRecord) -> ActiveRequestItem:
        cover_mbid = (
            record.track_release_group_mbid
            if record.request_kind == "track" and record.track_release_group_mbid
            else record.musicbrainz_id
        )
        return ActiveRequestItem(
            musicbrainz_id=record.musicbrainz_id,
            artist_name=record.artist_name,
            album_title=record.album_title,
            artist_mbid=record.artist_mbid,
            year=record.year,
            cover_url=prefer_release_group_cover_url(
                cover_mbid,
                record.cover_url,
                size=500,
            ),
            requested_at=datetime.fromisoformat(record.requested_at),
            status=record.status,
            progress=None,
            eta=None,
            size=None,
            size_remaining=None,
            download_status=None,
            download_state=None,
            status_messages=None,
            library_queue_id=None,
            user_id=record.user_id,
            requested_by_name=record.requested_by_name,
            request_kind=record.request_kind,
            track_title=record.track_title,
            duration_seconds=record.duration_seconds,
            track_release_group_mbid=record.track_release_group_mbid,
        )

    async def _check_if_completed(
        self,
        record: RequestHistoryRecord,
        library_mbids: set[str],
    ) -> bool:
        now_iso = datetime.now(timezone.utc).isoformat()

        if (
            record.request_kind != "track"
            and record.musicbrainz_id.lower() in library_mbids
        ):
            await self._request_history.async_update_status(
                record.musicbrainz_id, "imported", completed_at=now_iso
            )
            await self._notify_import(record)
            return True

        return False

    async def _notify_import(self, record: RequestHistoryRecord) -> None:
        self._library_mbids_cache = None
        self._library_mbids_cache_time = 0
        if self._on_import_callback:
            try:
                await self._on_import_callback(record)
            except Exception as e:  # noqa: BLE001
                logger.warning(
                    "Import callback failed for %s: %s", record.musicbrainz_id, e
                )
