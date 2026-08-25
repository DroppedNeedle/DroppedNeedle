import asyncio
import logging
from datetime import datetime, timezone
from typing import TYPE_CHECKING
from collections.abc import Callable
from infrastructure.persistence.request_history import RequestHistoryStore
from api.v1.schemas.request import (
    BatchCancelResponse,
    BatchRequestResponse,
    RequestAcceptedResponse,
)
from api.v1.schemas.download import TrackRequestResponse
from core.exceptions import ExternalServiceError, ValidationError
from infrastructure.queue.priority_queue import RequestPriority
from services.native.download_service import ALREADY_IN_LIBRARY

if TYPE_CHECKING:
    from infrastructure.persistence.mbid_store import MBIDStore
    from services.album_service import AlbumService
    from services.native.download_service import DownloadService
    from services.native.library_ownership_service import LibraryOwnershipService
    from services.quota_service import QuotaService

logger = logging.getLogger(__name__)


class RequestService:
    """The approval gate. The actual download runs through ``DownloadService``: a
    'user'-role request waits for admin approval; 'trusted'/'admin' auto-approve and
    dispatch the native pipeline immediately, linking the new ``download_task_id``."""

    def __init__(
        self,
        request_history: RequestHistoryStore,
        get_download_service: "Callable[[], DownloadService]",
        acquisition,  # noqa: ANN001 - AcquisitionDispatcher; every dispatch goes through it
        quota_service: "QuotaService | None" = None,
        ownership_service: "LibraryOwnershipService | None" = None,
        album_service: "AlbumService | None" = None,
        mbid_store: "MBIDStore | None" = None,
    ):
        self._request_history = request_history
        # cancel_task still goes direct to DownloadService, resolved fresh so a settings
        # save (which rebuilds the singleton) is picked up rather than ignored until restart.
        self._get_download_service = get_download_service
        self._quota = quota_service
        # every request_album dispatch goes through here; it picks the download client
        # or Free Music, so RequestService cannot function without it
        self._acquisition = acquisition
        self._ownership = ownership_service
        self._album_service = album_service
        self._mbid_store = mbid_store

    async def _resolve_album_identity(
        self, musicbrainz_id: str
    ) -> tuple[str, str | None]:
        if self._ownership is not None:
            musicbrainz_id = await self._ownership.provider_album_id(musicbrainz_id)
        if self._album_service is None:
            return musicbrainz_id, None
        canonical_id, release_mbid = await self._album_service.resolve_album_identity(
            musicbrainz_id
        )
        if self._mbid_store is not None:
            await self._mbid_store.save_mbid_resolution_map(
                {musicbrainz_id: canonical_id}
            )
            await self._request_history.async_canonicalize_known_release_aliases(
                [musicbrainz_id]
            )
        return canonical_id, release_mbid

    async def _resolve_batch_identities(self, items: list[dict]) -> list[dict]:
        provider_ids = [str(item["musicbrainz_id"]) for item in items]
        if self._ownership is not None:
            provider_ids = list(
                await asyncio.gather(
                    *(
                        self._ownership.provider_album_id(value)
                        for value in provider_ids
                    )
                )
            )
        if self._mbid_store is None:
            resolved = [
                await self._resolve_album_identity(provider_id)
                for provider_id in provider_ids
            ]
        else:
            durable = await self._mbid_store.get_mbid_resolution_map(provider_ids)
            resolved = []
            for provider_id in provider_ids:
                canonical = durable.get(provider_id.casefold()) or provider_id
                release_mbid = (
                    provider_id
                    if canonical.casefold() != provider_id.casefold()
                    else None
                )
                resolved.append((canonical, release_mbid))
        normalized: list[dict] = []
        for item, (canonical_mbid, release_mbid) in zip(items, resolved):
            value = dict(item)
            value["musicbrainz_id"] = canonical_mbid
            value["release_mbid"] = release_mbid
            normalized.append(value)
        return normalized

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
        musicbrainz_id, release_mbid = await self._resolve_album_identity(
            musicbrainz_id
        )

        needs_approval = user_role == "user"
        initial_status = "awaiting_approval" if needs_approval else "pending"

        try:
            existing = await self._request_history.async_get_record(musicbrainz_id)
            if existing and existing.status in ("pending", "downloading"):
                if monitor_artist and not existing.monitor_artist:
                    await self._request_history.async_update_monitoring_flags(
                        musicbrainz_id,
                        monitor_artist=True,
                        auto_download_artist=auto_download_artist,
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
                release_mbid=release_mbid,
                initial_status=initial_status,
            )
        except ValidationError:
            raise  # a quota/cap rejection carries its own user-facing message
        except Exception as e:  # noqa: BLE001
            logger.error(
                "Failed to record request history for %s: %s", musicbrainz_id, e
            )
            raise ExternalServiceError(f"Failed to record request: {e}")

        if needs_approval:
            logger.info(
                "Request queued for approval: %s by user %s", musicbrainz_id, user_id
            )
            return RequestAcceptedResponse(
                success=True,
                message="Request submitted, awaiting admin approval",
                musicbrainz_id=musicbrainz_id,
                status="awaiting_approval",
            )

        # auto-approve (trusted/admin): dispatch the native pipeline and link the
        # request to its task; the 'already_in_library' sentinel is guarded
        try:
            dispatch_kwargs = {
                "user_id": user_id or "",
                "release_group_mbid": musicbrainz_id,
                "artist_name": artist or "Unknown",
                "album_title": album or "Unknown",
                "year": year,
                "artist_mbid": artist_mbid,
                "origin": "user",
                "track_count_priority": RequestPriority.USER_INITIATED,
            }
            if release_mbid is not None:
                dispatch_kwargs["release_mbid"] = release_mbid
            task_id = await self._acquisition.request_album(
                **dispatch_kwargs,
            )
        except ValidationError:
            # cap/quota said no at dispatch (a race past the submit-time check):
            # surface the reason verbatim as a 400 rather than a wrapped 503
            try:
                await self._request_history.async_update_status(
                    musicbrainz_id,
                    "failed",
                    completed_at=datetime.now(timezone.utc).isoformat(),
                )
            except Exception:  # noqa: BLE001
                pass
            raise
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to dispatch download for %s: %s", musicbrainz_id, e)
            try:
                await self._request_history.async_update_status(
                    musicbrainz_id,
                    "failed",
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

        await self._request_history.async_update_download_task_id(
            musicbrainz_id, task_id
        )
        return RequestAcceptedResponse(
            success=True,
            message="Request accepted",
            musicbrainz_id=musicbrainz_id,
            status="pending",
        )

    async def request_track(
        self,
        recording_mbid: str,
        *,
        artist_name: str,
        track_title: str,
        album_title: str | None = None,
        duration_seconds: int | None = None,
        release_group_mbid: str | None = None,
        artist_mbid: str | None = None,
        release_mbid: str | None = None,
        user_id: str,
        user_role: str,
        requested_by_name: str | None = None,
    ) -> TrackRequestResponse:
        """Record an exact-track ask before dispatching it.

        This deliberately shares the album approval gate. A normal user can
        request one recording, but only a trusted user or an owner can start
        acquisition without review. The previous route bypassed approval and
        made exact-track requests less safe than whole-album requests.
        """
        needs_approval = user_role == "user"
        existing = await self._request_history.async_get_record(recording_mbid)
        if existing and existing.status in (
            "awaiting_approval",
            "pending",
            "queued",
            "downloading",
        ):
            return TrackRequestResponse(
                status=(
                    "awaiting_approval"
                    if existing.status == "awaiting_approval"
                    else "queued"
                ),
                task_id=existing.download_task_id,
            )

        if self._quota is not None:
            await self._quota.check_request_quota(user_id, user_role)
            await self._quota.check_storage_admission(user_id, "user")

        await self._request_history.async_record_request(
            musicbrainz_id=recording_mbid,
            artist_name=artist_name or "Unknown",
            album_title=album_title or "Single track",
            artist_mbid=artist_mbid,
            user_id=user_id,
            requested_by_name=requested_by_name,
            release_mbid=release_mbid,
            initial_status="awaiting_approval" if needs_approval else "pending",
            request_kind="track",
            track_title=track_title,
            duration_seconds=duration_seconds,
            track_release_group_mbid=release_group_mbid,
        )

        if needs_approval:
            logger.info(
                "Exact-track request queued for approval: %s by user %s",
                recording_mbid,
                user_id,
            )
            return TrackRequestResponse(status="awaiting_approval")

        try:
            task_id = await self._acquisition.request_track(
                user_id=user_id,
                recording_mbid=recording_mbid,
                artist_name=artist_name,
                track_title=track_title,
                album_title=album_title,
                duration_seconds=duration_seconds,
                release_group_mbid=release_group_mbid,
                artist_mbid=artist_mbid,
                release_mbid=release_mbid,
            )
        except Exception:
            await self._request_history.async_update_status(
                recording_mbid,
                "failed",
                completed_at=datetime.now(timezone.utc).isoformat(),
            )
            raise

        if task_id == ALREADY_IN_LIBRARY:
            await self._request_history.async_update_status(
                recording_mbid,
                "imported",
                completed_at=datetime.now(timezone.utc).isoformat(),
            )
            return TrackRequestResponse(status="already_in_library")

        await self._request_history.async_update_download_task_id(
            recording_mbid, task_id
        )
        return TrackRequestResponse(status="queued", task_id=task_id)

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
        raw_items: list[dict] = []
        seen_raw_mbids: set[str] = set()
        duplicate_count = 0
        for item in items:
            raw_key = str(item["musicbrainz_id"]).casefold()
            if raw_key in seen_raw_mbids:
                duplicate_count += 1
                continue
            seen_raw_mbids.add(raw_key)
            raw_items.append(item)
        normalized_items = await self._resolve_batch_identities(raw_items)
        items = []
        seen_mbids: set[str] = set()
        for item in normalized_items:
            canonical_key = str(item["musicbrainz_id"]).casefold()
            if canonical_key in seen_mbids:
                duplicate_count += 1
                continue
            seen_mbids.add(canonical_key)
            items.append(item)

        needs_approval = user_role == "user"
        initial_status = "awaiting_approval" if needs_approval else "pending"

        try:
            active = await self._request_history.async_get_requested_mbids()
            new_items = [
                item for item in items if item["musicbrainz_id"].lower() not in active
            ]
            skipped = duplicate_count + len(items) - len(new_items)

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
                await self._quota.check_request_quota(
                    user_id, user_role, len(new_items)
                )
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

            # auto-approve: dispatch each item through the native pipeline (mirrors
            # single request_album). slskd search is serialized client-side, so there's
            # no queue cap (overflow is always 0).
            dispatched = 0
            for item in new_items:
                mbid = item["musicbrainz_id"]
                try:
                    task_id = await self._acquisition.request_album(
                        user_id=user_id or "",
                        release_group_mbid=mbid,
                        artist_name=item.get("artist_name") or "Unknown",
                        album_title=item.get("album_title") or "Unknown",
                        year=item.get("year"),
                        artist_mbid=item.get("artist_mbid"),
                        origin="user",
                        release_mbid=item.get("release_mbid"),
                        track_count_priority=RequestPriority.USER_INITIATED,
                    )
                except Exception as e:  # noqa: BLE001 - one bad item must not sink the batch
                    logger.error("Batch download dispatch failed for %s: %s", mbid, e)
                    try:
                        await self._request_history.async_update_status(
                            mbid,
                            "failed",
                            completed_at=datetime.now(timezone.utc).isoformat(),
                        )
                    except Exception:  # noqa: BLE001 - status write must not sink the batch
                        logger.error("Failed to mark batch item %s failed", mbid)
                    continue
                if task_id != ALREADY_IN_LIBRARY:
                    await self._request_history.async_update_download_task_id(
                        mbid, task_id
                    )
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
        self,
        musicbrainz_ids: list[str],
        user_id: str | None = None,
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
                if record is not None and record.download_task_id:
                    try:
                        await self._get_download_service().cancel_task(
                            record.download_task_id,
                            record.user_id or user_id or "",
                            "admin" if is_admin else "user",
                        )
                    except Exception as exc:  # noqa: BLE001
                        logger.warning(
                            "Batch cancel: native task cancel failed for %s: %s",
                            mbid,
                            exc,
                        )
                now_iso = datetime.now(timezone.utc).isoformat()
                await self._request_history.async_update_status(
                    mbid,
                    "cancelled",
                    completed_at=now_iso,
                )
                cancelled += 1
            except Exception:  # noqa: BLE001
                failed += 1
        return BatchCancelResponse(
            success=cancelled > 0,
            cancelled=cancelled,
            failed=failed,
            message=f"Cancelled {cancelled} requests"
            + (f", {failed} failed" if failed else ""),
        )
