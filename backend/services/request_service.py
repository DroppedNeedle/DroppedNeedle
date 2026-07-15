import logging
from datetime import datetime, timezone
from typing import TYPE_CHECKING, Literal
from collections.abc import Callable
from infrastructure.persistence.request_history import RequestHistoryStore
from api.v1.schemas.request import (
    BatchCancelResponse,
    BatchRequestResponse,
    RequestAcceptedResponse,
)
from core.exceptions import ExternalServiceError, ValidationError
from services.request_backend_service import ALREADY_IN_LIBRARY, DISPATCH_FAILED

if TYPE_CHECKING:
    from services.request_backend_service import RequestBackendService
    from services.native.download_service import DownloadService
    from services.quota_service import QuotaService

logger = logging.getLogger(__name__)


class RequestService:
    """The approval gate. Both user submission and admin approval route through
    the shared RequestBackendService dispatch contract, ensuring consistent
    behavior across paths and no duplicate acquisition.

    Cancellation operations go directly to download_service for native task
    management.
    """

    def __init__(
        self,
        request_history: RequestHistoryStore,
        get_download_service: "Callable[[], DownloadService]",
        acquisition=None,  # noqa: ANN001 - AcquisitionDispatcher; every dispatch goes through it
        request_backend: "RequestBackendService | None" = None,
        quota_service: "QuotaService | None" = None,
    ):
        self._request_history = request_history
        # cancel_task still goes direct to DownloadService, resolved fresh so a settings
        # save (which rebuilds the singleton) is picked up rather than ignored until restart.
        self._get_download_service = get_download_service
        self._request_backend = request_backend
        self._quota = quota_service
        # every request_album dispatch goes through either the request backend seam
        # or the pre-existing acquisition dispatcher fallback.
        self._acquisition = acquisition

    @staticmethod
    def _dispatch_failed(task_id: str) -> bool:
        return task_id == DISPATCH_FAILED

    async def _dispatch_request(
        self,
        *,
        user_id: str,
        release_group_mbid: str,
        artist_name: str,
        album_title: str,
        year: int | None,
        artist_mbid: str | None,
        origin: Literal["user", "retry"],
    ) -> str:
        if self._request_backend is not None:
            return await self._request_backend.dispatch_request(
                user_id=user_id,
                release_group_mbid=release_group_mbid,
                artist_name=artist_name,
                album_title=album_title,
                year=year,
                artist_mbid=artist_mbid,
                origin=origin,
            )
        if self._acquisition is None:
            raise ExternalServiceError("No acquisition dispatcher configured.")
        return await self._acquisition.request_album(
            user_id=user_id,
            release_group_mbid=release_group_mbid,
            artist_name=artist_name,
            album_title=album_title,
            year=year,
            artist_mbid=artist_mbid,
            origin=origin,
        )

    async def request_album(
        self,
        musicbrainz_id: str,
        artist: str | None = None,
        album: str | None = None,
        year: int | None = None,
        artist_mbid: str | None = None,
        monitor_artist: bool = False,
        auto_download_artist: bool = False,
        user_id: str | None = None,
        user_role: str | None = None,
        requested_by_name: str | None = None,
    ) -> RequestAcceptedResponse:
        if user_role is None:
            raise ExternalServiceError("User role is required to submit a request.")

        needs_approval = user_role == "user"
        initial_status = "awaiting_approval" if needs_approval else "pending"

        try:
            existing = await self._request_history.async_get_record(musicbrainz_id)
            if existing and existing.status in ("pending", "downloading"):
                if monitor_artist and not existing.monitor_artist:
                    await self._request_history.async_update_monitoring_flags(
                        musicbrainz_id, monitor_artist=True, auto_download_artist=auto_download_artist,
                    )
                return RequestAcceptedResponse(
                    success=True,
                    message="Request already in progress",
                    musicbrainz_id=musicbrainz_id,
                    status=existing.status,
                )
            if existing and existing.status == "awaiting_approval":
                return RequestAcceptedResponse(
                    success=True,
                    message="Request is awaiting admin approval",
                    musicbrainz_id=musicbrainz_id,
                    status="awaiting_approval",
                )
            # Request-count quota at SUBMIT (Feature C layer 1, D20): the ask is
            # recorded here long before a download task exists, so this is the only
            # honest place to count it. The byte caps ALSO fail fast here
            # (task-creation keeps the enforcement backstop): without this, a user's
            # ask sits awaiting approval and then dies at approve time. After the
            # dedup early-returns, so re-asking for an in-flight album keeps its
            # friendly answer even while over quota.
            if self._quota is not None:
                await self._quota.check_request_quota(user_id, user_role)
                await self._quota.check_storage_admission(user_id or "", "user")
            await self._request_history.async_record_request(
                musicbrainz_id=musicbrainz_id,
                artist_name=artist or "Unknown",
                album_title=album or "Unknown",
                year=year,
                artist_mbid=artist_mbid,
                monitor_artist=monitor_artist,
                auto_download_artist=auto_download_artist,
                user_id=user_id,
                requested_by_name=requested_by_name,
                initial_status=initial_status,
            )
        except ValidationError:
            raise  # a quota/cap rejection carries its own user-facing message
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to record request history for %s: %s", musicbrainz_id, e)
            raise ExternalServiceError(f"Failed to record request: {e}")

        if needs_approval:
            logger.info("Request queued for approval: %s by user %s", musicbrainz_id, user_id)
            return RequestAcceptedResponse(
                success=True,
                message="Request submitted, awaiting admin approval",
                musicbrainz_id=musicbrainz_id,
                status="awaiting_approval",
            )

        # auto-approve (trusted/admin): dispatch through the shared backend seam
        try:
            task_id = await self._dispatch_request(
                user_id=user_id or "",
                release_group_mbid=musicbrainz_id,
                artist_name=artist or "Unknown",
                album_title=album or "Unknown",
                year=year,
                artist_mbid=artist_mbid,
                origin="user",
            )
            if self._dispatch_failed(task_id):
                logger.error(
                    "request_service.dispatch_failed musicbrainz_id=%s origin=user",
                    musicbrainz_id,
                )
                await self._request_history.async_update_status(
                    musicbrainz_id, "failed",
                    completed_at=datetime.now(timezone.utc).isoformat(),
                )
                raise ExternalServiceError("Failed to start download: request backend dispatch failed")
        except ValidationError as e:
            # cap/quota said no at dispatch (a race past the submit-time check):
            # surface the reason verbatim as a 400 rather than a wrapped 503
            try:
                await self._request_history.async_update_status(
                    musicbrainz_id, "failed",
                    completed_at=datetime.now(timezone.utc).isoformat(),
                )
            except Exception:  # noqa: BLE001
                pass
            raise
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to dispatch download for %s: %s", musicbrainz_id, e)
            try:
                await self._request_history.async_update_status(
                    musicbrainz_id, "failed",
                    completed_at=datetime.now(timezone.utc).isoformat(),
                )
            except Exception:  # noqa: BLE001
                pass
            raise ExternalServiceError(f"Failed to start download: {e}")

        if task_id == ALREADY_IN_LIBRARY:
            return RequestAcceptedResponse(
                success=True,
                message="Album is already in the library",
                musicbrainz_id=musicbrainz_id,
                status="pending",
            )

        await self._request_history.async_update_download_task_id(musicbrainz_id, task_id)
        return RequestAcceptedResponse(
            success=True,
            message="Request accepted",
            musicbrainz_id=musicbrainz_id,
            status="pending",
        )
    
    async def request_batch(
        self,
        items: list[dict],
        monitor_artist: bool = False,
        auto_download_artist: bool = False,
        user_id: str | None = None,
        user_role: str | None = None,
        requested_by_name: str | None = None,
    ) -> BatchRequestResponse:
        if user_role is None:
            raise ExternalServiceError("User role is required to submit a request.")

        needs_approval = user_role == "user"
        initial_status = "awaiting_approval" if needs_approval else "pending"

        try:
            active = await self._request_history.async_get_active_mbids()
            new_items = [
                item for item in items
                if item["musicbrainz_id"].lower() not in active
            ]
            skipped = len(items) - len(new_items)

            if not new_items:
                return BatchRequestResponse(
                    success=True,
                    message="All albums already requested",
                    requested=0,
                    skipped=skipped,
                )

            # A batch of N counts as N asks (A4); over-quota rejects the WHOLE batch
            # (partial acceptance would silently drop albums the user asked for).
            # Byte caps fail fast here too (see request_album).
            if self._quota is not None:
                await self._quota.check_request_quota(user_id, user_role, len(new_items))
                await self._quota.check_storage_admission(user_id or "", "user")

            await self._request_history.async_bulk_record_requests(
                new_items,
                monitor_artist=monitor_artist,
                auto_download_artist=auto_download_artist,
                user_id=user_id,
                requested_by_name=requested_by_name,
                initial_status=initial_status,
            )

            if needs_approval:
                return BatchRequestResponse(
                    success=True,
                    message="Batch request submitted, awaiting admin approval",
                    requested=len(new_items),
                    skipped=skipped,
                )

            # auto-approve: dispatch each item through the shared backend seam
            # (mirrors single request_album). slskd search is serialized
            # client-side, so there's no queue cap (overflow is always 0).
            dispatched = 0
            for item in new_items:
                mbid = item["musicbrainz_id"]
                try:
                    task_id = await self._dispatch_request(
                        user_id=user_id or "",
                        release_group_mbid=mbid,
                        artist_name=item.get("artist_name") or "Unknown",
                        album_title=item.get("album_title") or "Unknown",
                        year=item.get("year"),
                        artist_mbid=item.get("artist_mbid"),
                        origin="user",
                    )
                    if self._dispatch_failed(task_id):
                        raise ExternalServiceError("request backend dispatch failed")
                except Exception as e:  # noqa: BLE001 - one bad item must not sink the batch
                    logger.error("Batch download dispatch failed for %s: %s", mbid, e)
                    try:
                        await self._request_history.async_update_status(
                            mbid, "failed", completed_at=datetime.now(timezone.utc).isoformat()
                        )
                    except Exception:  # noqa: BLE001 - status write must not sink the batch
                        logger.error("Failed to mark batch item %s failed", mbid)
                    continue
                if task_id != ALREADY_IN_LIBRARY:
                    await self._request_history.async_update_download_task_id(mbid, task_id)
                dispatched += 1

            return BatchRequestResponse(
                success=True,
                message=f"Batch request accepted: {dispatched} started",
                requested=dispatched,
                skipped=skipped,
                overflow=0,
            )
        except (ExternalServiceError, ValidationError):
            raise
        except Exception as e:  # noqa: BLE001
            logger.error("Batch request failed: %s", e)
            raise ExternalServiceError(f"Batch request failed: {e}")

    async def cancel_batch(
        self, musicbrainz_ids: list[str], user_id: str | None = None,
        user_role: str | None = None,
    ) -> BatchCancelResponse:
        # non-admin (user_id set): only own requests cancelled, others counted failed
        # without revealing existence; user_id is None is the admin path (cancel any)
        is_admin = user_role == "admin" or user_id is None
        cancelled = 0
        failed = 0
        for mbid in musicbrainz_ids:
            try:
                record = await self._request_history.async_get_record(mbid)
                if not is_admin and (record is None or record.user_id != user_id):
                    failed += 1
                    continue
                # best-effort: a missing/non-cancellable task must not block marking
                # cancelled (only applies to native backend with download_task_id)
                if record is not None and record.download_task_id:
                    try:
                        await self._get_download_service().cancel_task(
                            record.download_task_id,
                            record.user_id or user_id or "",
                            "admin" if is_admin else "user",
                        )
                    except Exception as exc:  # noqa: BLE001
                        logger.warning(
                            "Batch cancel: native task cancel failed for %s: %s", mbid, exc
                        )
                now_iso = datetime.now(timezone.utc).isoformat()
                await self._request_history.async_update_status(
                    mbid, "cancelled", completed_at=now_iso,
                )
                cancelled += 1
            except Exception:  # noqa: BLE001
                failed += 1
        return BatchCancelResponse(
            success=cancelled > 0,
            cancelled=cancelled,
            failed=failed,
            message=f"Cancelled {cancelled} requests" + (f", {failed} failed" if failed else ""),
        )
