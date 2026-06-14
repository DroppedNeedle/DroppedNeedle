import logging
from datetime import datetime, timezone
from repositories.protocols import LidarrRepositoryProtocol
from infrastructure.queue.request_queue import RequestQueue
from infrastructure.persistence.request_history import RequestHistoryStore
from api.v1.schemas.request import (
    BatchCancelResponse,
    BatchRequestResponse,
    QueueStatusResponse,
    RequestAcceptedResponse,
)
from core.exceptions import ExternalServiceError

logger = logging.getLogger(__name__)


class RequestService:
    def __init__(
        self,
        lidarr_repo: LidarrRepositoryProtocol,
        request_queue: RequestQueue,
        request_history: RequestHistoryStore,
    ):
        self._lidarr_repo = lidarr_repo
        self._request_queue = request_queue
        self._request_history = request_history
    
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

        if not self._lidarr_repo.is_configured():
            raise ExternalServiceError("Lidarr isn't configured. Add an API key in Settings before requesting albums.")

        needs_approval = user_role == "user"
        initial_status = "awaiting_approval" if needs_approval else "pending"

        try:
            existing = await self._request_history.async_get_record(musicbrainz_id)
            if existing and existing.status in ("pending", "downloading"):
                if monitor_artist and not existing.monitor_artist:
                    await self._request_history.async_update_monitoring_flags(
                        musicbrainz_id, monitor_artist=True, auto_download_artist=auto_download_artist,
                    )
                await self._request_queue.enqueue(musicbrainz_id)
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

        try:
            enqueued = await self._request_queue.enqueue(musicbrainz_id)
            if not enqueued:
                return RequestAcceptedResponse(
                    success=True,
                    message="Request already in queue",
                    musicbrainz_id=musicbrainz_id,
                    status="pending",
                )
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to enqueue album %s: %s", musicbrainz_id, e)
            try:
                await self._request_history.async_update_status(
                    musicbrainz_id, "failed",
                    completed_at=datetime.now(timezone.utc).isoformat(),
                )
            except Exception:  # noqa: BLE001
                pass
            raise ExternalServiceError(f"Failed to enqueue request: {e}")

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
        """Request multiple albums at once. Returns counts of requested, skipped, and overflow."""
        if user_role is None:
            raise ExternalServiceError("User role is required to submit a request.")

        if not self._lidarr_repo.is_configured():
            raise ExternalServiceError("Lidarr isn't configured. Add an API key in Settings before requesting albums.")

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
                    message=f"Batch request submitted, awaiting admin approval",
                    requested=len(new_items),
                    skipped=skipped,
                )

            mbids = [item["musicbrainz_id"] for item in new_items]
            enqueued, overflow = await self._request_queue.enqueue_many(mbids)

            return BatchRequestResponse(
                success=True,
                message=f"Batch request accepted: {enqueued} enqueued",
                requested=enqueued + overflow,
                skipped=skipped,
                overflow=overflow,
            )
        except ExternalServiceError:
            raise
        except Exception as e:  # noqa: BLE001
            logger.error("Batch request failed: %s", e)
            raise ExternalServiceError(f"Batch request failed: {e}")

    async def cancel_batch(
        self, musicbrainz_ids: list[str], user_id: str | None = None
    ) -> BatchCancelResponse:
        """Cancel multiple requests. Uses RequestQueue.cancel() for each.

        When ``user_id`` is provided (non-admin caller), only requests owned by
        that user are cancelled; others are counted as failed without revealing
        whether they exist, mirroring the single-cancel ownership check.
        """
        cancelled = 0
        failed = 0
        for mbid in musicbrainz_ids:
            try:
                if user_id is not None:
                    record = await self._request_history.async_get_record(mbid)
                    if record is None or record.user_id != user_id:
                        failed += 1
                        continue
                await self._request_queue.cancel(mbid)
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

    def get_queue_status(self) -> QueueStatusResponse:
        status = self._request_queue.get_status()
        return QueueStatusResponse(
            queue_size=status["queue_size"],
            processing=status["processing"],
            active_workers=status.get("active_workers", 0),
            max_workers=status.get("max_workers", 1),
        )
