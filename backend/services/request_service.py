import asyncio
import logging
from datetime import datetime, timezone
from typing import TYPE_CHECKING
from collections.abc import Callable

from api.v1.schemas.download import TrackRequestResponse
from api.v1.schemas.request import (
    BatchCancelResponse,
    BatchRequestResponse,
    RequestAcceptedResponse,
)
from core.exceptions import ExternalServiceError, ResourceNotFoundError, ValidationError
from infrastructure.persistence.request_history import RequestHistoryStore
from infrastructure.queue.priority_queue import RequestPriority
from services.native.download_service import ALREADY_IN_LIBRARY

if TYPE_CHECKING:
    from infrastructure.persistence.mbid_store import MBIDStore
    from services.album_service import AlbumService
    from services.native.download_service import DownloadService
    from services.native.library_ownership_service import LibraryOwnershipService
    from services.quota_service import QuotaService


logger = logging.getLogger(__name__)

_ACTIVE_REQUEST_STATUSES = frozenset(
    {"pending", "downloading", "queued", "awaiting_approval"}
)
_CANCELLING_STATUS = "cancelling"


def _generation_of(value: object | None) -> int | None:
    generation = getattr(value, "generation", None)
    return (
        generation
        if isinstance(generation, int) and not isinstance(generation, bool)
        else None
    )


def _mutation_won(value: object) -> bool:
    return value is not False


def _meaningful_name(value: str | None) -> str | None:
    """None for absent, blank, or literal-"Unknown" names; else the stripped value."""
    if value is None:
        return None
    stripped = value.strip()
    if not stripped or stripped.casefold() == "unknown":
        return None
    return stripped


def _request_begin_won(value: object | None) -> bool:
    return value is not None and value is not False


_RETRYABLE_BEGIN_ATTEMPTS = 2


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

    async def _resolve_request_names(
        self,
        musicbrainz_id: str,
        artist: str | None,
        album: str | None,
    ) -> tuple[str | None, str | None]:
        # Literal placeholders from older callers (e.g. the playlist
        # missing-tracks flow) are missing values, not real names.
        artist = _meaningful_name(artist)
        album = _meaningful_name(album)
        if artist and album:
            return artist, album
        if self._album_service is not None:
            try:
                info = await self._album_service.get_album_basic_info(musicbrainz_id)
            except (ResourceNotFoundError, ValueError) as error:
                # Lookup miss only: unknown MBID or degraded MusicBrainz with no
                # local fallback. Anything else (bugs, outages surfacing as
                # unexpected errors) propagates instead of becoming a 400.
                logger.warning(
                    "Could not resolve names for %s: %s", musicbrainz_id, error
                )
                info = None
            if info is not None:
                if not artist:
                    artist = _meaningful_name(getattr(info, "artist_name", None))
                if not album:
                    album = _meaningful_name(getattr(info, "title", None))
        if not artist or not album:
            raise ValidationError(
                f"Could not resolve artist and album for MBID {musicbrainz_id}: "
                "artist and album are required when MusicBrainz resolution fails"
            )
        return artist, album

    async def _begin_request(
        self,
        *,
        musicbrainz_id: str,
        request_kind: str,
        request_kwargs: dict[str, object],
    ) -> tuple[object | None, object | None]:
        """Claim one request generation without dispatching a race loser.

        The persistence operation returns the exact generation won by this
        caller. ``None`` means an active generation already owns the key; the
        lookup is only used to produce the authoritative response and to retry
        the narrow terminal-transition race.
        """
        winner: object | None = None
        for attempt in range(_RETRYABLE_BEGIN_ATTEMPTS):
            result = await self._request_history.async_record_request(**request_kwargs)
            if _request_begin_won(result):
                return result, None
            winner = await self._request_history.async_get_record(
                musicbrainz_id, request_kind=request_kind
            )
            status = getattr(winner, "status", None)
            if winner is None or status in (
                _ACTIVE_REQUEST_STATUSES | {_CANCELLING_STATUS}
            ):
                return None, winner
            if attempt + 1 < _RETRYABLE_BEGIN_ATTEMPTS:
                continue
        return None, winner

    async def _mark_failed(
        self,
        musicbrainz_id: str,
        request_kind: str,
        *,
        expected_generation: int | None = None,
    ) -> bool:
        try:
            kwargs: dict[str, object] = {
                "completed_at": datetime.now(timezone.utc).isoformat(),
                "request_kind": request_kind,
            }
            if expected_generation is not None:
                kwargs["expected_generation"] = expected_generation
            result = await self._request_history.async_update_status(
                musicbrainz_id,
                "failed",
                **kwargs,
            )
            return _mutation_won(result)
        except Exception:  # noqa: BLE001
            logger.exception("Failed to mark request %s failed", musicbrainz_id)
            return False

    async def _cancel_orphan_task(
        self, task_id: str | None, user_id: str, *, user_role: str = "user"
    ) -> None:
        if not task_id or task_id == ALREADY_IN_LIBRARY:
            return
        try:
            await self._get_download_service().cancel_task(task_id, user_id, user_role)
        except Exception:  # noqa: BLE001 - the request row remains generation-safe
            logger.warning("Failed to cancel orphan download task %s", task_id)

    async def _quality_snapshot_summary(
        self, task_id: str | None, user_id: str | None, user_role: str | None
    ) -> str | None:
        """Read the summary pinned by the acquisition backend, not live policy."""
        if not task_id:
            return None
        method = getattr(type(self._acquisition), "get_quality_snapshot_summary", None)
        if method is None:
            return None
        try:
            summary = await self._acquisition.get_quality_snapshot_summary(
                task_id, user_id or "", user_role or "user"
            )
            return summary if isinstance(summary, str) else None
        except Exception:  # noqa: BLE001 - response feedback cannot undo acceptance
            logger.warning("Unable to read quality summary for task %s", task_id)
            return None

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
        artist_name, album_title = await self._resolve_request_names(
            musicbrainz_id, artist, album
        )
        needs_approval = user_role not in ("trusted", "admin")
        initial_status = "awaiting_approval" if needs_approval else "pending"
        request_kwargs: dict[str, object] = {
            "musicbrainz_id": musicbrainz_id,
            "artist_name": artist_name,
            "album_title": album_title,
            "year": year,
            "artist_mbid": artist_mbid,
            "monitor_artist": monitor_artist,
            "auto_download_artist": auto_download_artist,
            "user_id": user_id,
            "requested_by_name": requested_by_name,
            "release_mbid": release_mbid,
            "initial_status": initial_status,
            "request_kind": "album",
            "dispatch_authorized": not needs_approval,
        }

        try:
            existing = await self._request_history.async_get_record(
                musicbrainz_id, request_kind="album"
            )
            if existing and existing.status in _ACTIVE_REQUEST_STATUSES:
                await self._request_history.async_add_requester(
                    musicbrainz_id,
                    user_id,
                    requested_by_name,
                    request_kind="album",
                )
                if monitor_artist and not getattr(existing, "monitor_artist", False):
                    await self._request_history.async_update_monitoring_flags(
                        musicbrainz_id,
                        monitor_artist=True,
                        auto_download_artist=auto_download_artist,
                        request_kind="album",
                    )
                return RequestAcceptedResponse(
                    success=True,
                    message=(
                        "Request is awaiting admin approval"
                        if existing.status == "awaiting_approval"
                        else "Request already in progress"
                    ),
                    musicbrainz_id=musicbrainz_id,
                    status=existing.status,
                    quality_snapshot_summary=await self._quality_snapshot_summary(
                        getattr(existing, "download_task_id", None),
                        user_id,
                        user_role,
                    ),
                )
            if existing and existing.status == _CANCELLING_STATUS:
                return RequestAcceptedResponse(
                    success=True,
                    message="Request is being cancelled",
                    musicbrainz_id=musicbrainz_id,
                    status=existing.status,
                    quality_snapshot_summary=await self._quality_snapshot_summary(
                        getattr(existing, "download_task_id", None),
                        user_id,
                        user_role,
                    ),
                )

            if self._quota is not None:
                await self._quota.check_request_quota(user_id, user_role)
                await self._quota.check_storage_admission(user_id or "", "user")
            begin_result, winner = await self._begin_request(
                musicbrainz_id=musicbrainz_id,
                request_kind="album",
                request_kwargs=request_kwargs,
            )
            generation = _generation_of(begin_result)
            if begin_result is None:
                status = getattr(winner, "status", None)
                if status in _ACTIVE_REQUEST_STATUSES:
                    await self._request_history.async_add_requester(
                        musicbrainz_id,
                        user_id,
                        requested_by_name,
                        request_kind="album",
                    )
                    return RequestAcceptedResponse(
                        success=True,
                        message=(
                            "Request is awaiting admin approval"
                            if status == "awaiting_approval"
                            else "Request already in progress"
                        ),
                        musicbrainz_id=musicbrainz_id,
                        status=status,
                        quality_snapshot_summary=await self._quality_snapshot_summary(
                            getattr(winner, "download_task_id", None),
                            user_id,
                            user_role,
                        ),
                    )
                if status == _CANCELLING_STATUS:
                    return RequestAcceptedResponse(
                        success=True,
                        message="Request is being cancelled",
                        musicbrainz_id=musicbrainz_id,
                        status=status,
                        quality_snapshot_summary=await self._quality_snapshot_summary(
                            getattr(winner, "download_task_id", None),
                            user_id,
                            user_role,
                        ),
                    )
                return RequestAcceptedResponse(
                    success=False,
                    message="Request could not be recorded",
                    musicbrainz_id=musicbrainz_id,
                    status="failed",
                )
        except ValidationError:
            raise
        except Exception as error:  # noqa: BLE001
            logger.exception("Failed to record request history for %s", musicbrainz_id)
            raise ExternalServiceError("Failed to record request") from error

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
        try:
            task_id = await self._acquisition.request_album(
                user_id=user_id or "",
                release_group_mbid=musicbrainz_id,
                artist_name=artist_name,
                album_title=album_title,
                year=year,
                artist_mbid=artist_mbid,
                origin="user",
                release_mbid=release_mbid,
                track_count_priority=RequestPriority.USER_INITIATED,
            )
        except ValidationError:
            await self._mark_failed(
                musicbrainz_id,
                "album",
                expected_generation=generation,
            )
            raise
        except Exception as error:  # noqa: BLE001
            logger.exception("Failed to dispatch request %s", musicbrainz_id)
            await self._mark_failed(
                musicbrainz_id,
                "album",
                expected_generation=generation,
            )
            raise ExternalServiceError("Failed to start download") from error

        if task_id == ALREADY_IN_LIBRARY:
            try:
                kwargs: dict[str, object] = {
                    "completed_at": datetime.now(timezone.utc).isoformat(),
                    "request_kind": "album",
                }
                if generation is not None:
                    kwargs["expected_generation"] = generation
                await self._request_history.async_update_status(
                    musicbrainz_id,
                    "imported",
                    **kwargs,
                )
            except Exception as error:  # noqa: BLE001
                logger.exception(
                    "Failed to mark album request %s imported", musicbrainz_id
                )
                raise ExternalServiceError("Failed to complete request") from error
            return RequestAcceptedResponse(
                success=True,
                message="Album is already in the library",
                musicbrainz_id=musicbrainz_id,
                status="pending",
            )
        try:
            kwargs = {"request_kind": "album"}
            if generation is not None:
                kwargs["expected_generation"] = generation
            linked = await self._request_history.async_update_download_task_id(
                musicbrainz_id,
                task_id,
                **kwargs,
            )
            if not _mutation_won(linked):
                await self._cancel_orphan_task(task_id, user_id or "")
                raise ExternalServiceError(
                    "Request generation changed while starting download"
                )
        except ExternalServiceError:
            raise
        except Exception as error:  # noqa: BLE001
            logger.exception("Failed to link request %s to task", musicbrainz_id)
            await self._cancel_orphan_task(task_id, user_id or "")
            await self._mark_failed(
                musicbrainz_id,
                "album",
                expected_generation=generation,
            )
            raise ExternalServiceError("Failed to start download") from error
        return RequestAcceptedResponse(
            success=True,
            message="Request accepted",
            musicbrainz_id=musicbrainz_id,
            status="pending",
            quality_snapshot_summary=await self._quality_snapshot_summary(
                task_id, user_id, user_role
            ),
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
        """Record and role-gate one exact recording before acquisition."""
        if user_role is None:
            raise ExternalServiceError("User role is required to submit a request.")
        needs_approval = user_role not in ("trusted", "admin")
        initial_status = "awaiting_approval" if needs_approval else "pending"
        request_kwargs: dict[str, object] = {
            "musicbrainz_id": recording_mbid,
            "artist_name": artist_name or "Unknown",
            "album_title": album_title or "Single track",
            "artist_mbid": artist_mbid,
            "user_id": user_id,
            "requested_by_name": requested_by_name,
            "release_mbid": release_mbid,
            "initial_status": initial_status,
            "request_kind": "track",
            "track_title": track_title,
            "duration_seconds": duration_seconds,
            "track_release_group_mbid": release_group_mbid,
            "dispatch_authorized": not needs_approval,
        }

        try:
            existing = await self._request_history.async_get_record(
                recording_mbid, request_kind="track"
            )
            if existing and existing.status in _ACTIVE_REQUEST_STATUSES:
                await self._request_history.async_add_requester(
                    recording_mbid,
                    user_id,
                    requested_by_name,
                    request_kind="track",
                )
                return TrackRequestResponse(
                    status=(
                        "awaiting_approval"
                        if existing.status == "awaiting_approval"
                        else "queued"
                    ),
                    task_id=getattr(existing, "download_task_id", None),
                )
            if existing and existing.status == _CANCELLING_STATUS:
                return TrackRequestResponse(
                    status="queued",
                    task_id=getattr(existing, "download_task_id", None),
                )

            if self._quota is not None:
                await self._quota.check_request_quota(user_id, user_role)
                await self._quota.check_storage_admission(user_id, "user")
            begin_result, winner = await self._begin_request(
                musicbrainz_id=recording_mbid,
                request_kind="track",
                request_kwargs=request_kwargs,
            )
            generation = _generation_of(begin_result)
            if begin_result is None:
                status = getattr(winner, "status", None)
                if status in _ACTIVE_REQUEST_STATUSES:
                    await self._request_history.async_add_requester(
                        recording_mbid,
                        user_id,
                        requested_by_name,
                        request_kind="track",
                    )
                    return TrackRequestResponse(
                        status=(
                            "awaiting_approval"
                            if status == "awaiting_approval"
                            else "queued"
                        ),
                        task_id=getattr(winner, "download_task_id", None),
                    )
                if status == _CANCELLING_STATUS:
                    return TrackRequestResponse(
                        status="queued",
                        task_id=getattr(winner, "download_task_id", None),
                    )
                return TrackRequestResponse(status="queued")
        except ValidationError:
            raise
        except Exception as error:  # noqa: BLE001
            logger.exception("Failed to record track request %s", recording_mbid)
            raise ExternalServiceError("Failed to record request") from error

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
                origin="user",
                release_mbid=release_mbid,
            )
        except ValidationError:
            await self._mark_failed(
                recording_mbid,
                "track",
                expected_generation=generation,
            )
            raise
        except Exception as error:  # noqa: BLE001
            logger.exception("Failed to dispatch track request %s", recording_mbid)
            await self._mark_failed(
                recording_mbid,
                "track",
                expected_generation=generation,
            )
            raise ExternalServiceError("Failed to start download") from error

        if task_id == ALREADY_IN_LIBRARY:
            try:
                kwargs: dict[str, object] = {
                    "completed_at": datetime.now(timezone.utc).isoformat(),
                    "request_kind": "track",
                }
                if generation is not None:
                    kwargs["expected_generation"] = generation
                await self._request_history.async_update_status(
                    recording_mbid,
                    "imported",
                    **kwargs,
                )
            except Exception as error:  # noqa: BLE001
                logger.exception(
                    "Failed to mark track request %s imported", recording_mbid
                )
                raise ExternalServiceError("Failed to complete request") from error
            return TrackRequestResponse(status="already_in_library")
        try:
            kwargs = {"request_kind": "track"}
            if generation is not None:
                kwargs["expected_generation"] = generation
            linked = await self._request_history.async_update_download_task_id(
                recording_mbid,
                task_id,
                **kwargs,
            )
            if not _mutation_won(linked):
                await self._cancel_orphan_task(task_id, user_id)
                raise ExternalServiceError(
                    "Request generation changed while starting download"
                )
        except ExternalServiceError:
            raise
        except Exception as error:  # noqa: BLE001
            logger.exception("Failed to link track request %s to task", recording_mbid)
            await self._cancel_orphan_task(task_id, user_id)
            await self._mark_failed(
                recording_mbid,
                "track",
                expected_generation=generation,
            )
            raise ExternalServiceError("Failed to start download") from error
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
        normalized: list[dict] = []
        seen_mbids: set[str] = set()
        for item in normalized_items:
            canonical_key = str(item["musicbrainz_id"]).casefold()
            if canonical_key in seen_mbids:
                duplicate_count += 1
                continue
            seen_mbids.add(canonical_key)
            normalized.append(item)

        needs_approval = user_role not in ("trusted", "admin")
        initial_status = "awaiting_approval" if needs_approval else "pending"

        try:
            active = await self._request_history.async_get_requested_mbids(
                request_kind="album"
            )
            new_items = [
                item
                for item in normalized
                if str(item["musicbrainz_id"]).casefold() not in active
            ]
            existing_items = [
                str(item["musicbrainz_id"])
                for item in normalized
                if str(item["musicbrainz_id"]).casefold() in active
            ]
            skipped = duplicate_count + len(existing_items)

            if not new_items:
                if existing_items:
                    await self._request_history.async_add_requesters(
                        existing_items,
                        user_id,
                        requested_by_name,
                        request_kind="album",
                    )
                return BatchRequestResponse(
                    success=True,
                    message="All albums already requested",
                    requested=0,
                    skipped=skipped,
                    status="already_requested",
                )

            if self._quota is not None:
                await self._quota.check_request_quota(
                    user_id, user_role, len(new_items)
                )
                await self._quota.check_storage_admission(user_id or "", "user")

            resolvable: list[dict] = []
            for item in new_items:
                mbid = str(item["musicbrainz_id"])
                try:
                    artist_name, album_title = await self._resolve_request_names(
                        mbid,
                        item.get("artist_name") or None,
                        item.get("album_title") or None,
                    )
                except ValidationError as error:
                    logger.warning("Skipping batch item %s: %s", mbid, error)
                    skipped += 1
                    continue
                item["artist_name"] = artist_name
                item["album_title"] = album_title
                resolvable.append(item)
            new_items = resolvable
            if not new_items:
                return BatchRequestResponse(
                    success=False,
                    message="Batch request could not be recorded",
                    requested=0,
                    skipped=skipped,
                    status="failed",
                )

            bulk_result = await self._request_history.async_bulk_record_requests(
                new_items,
                monitor_artist=monitor_artist,
                auto_download_artist=auto_download_artist,
                user_id=user_id,
                requested_by_name=requested_by_name,
                initial_status=initial_status,
                request_kind="album",
                dispatch_authorized=not needs_approval,
            )

            # The transaction returns the exact generations won by this batch.
            # Never reconstruct winners from mutable owner/status fields: another
            # overlapping batch may have won one of the same keys.
            generation_by_key: dict[str, int | None] = {}
            if isinstance(bulk_result, list):
                winner_by_key: dict[str, object] = {}
                for result in bulk_result:
                    result_id = getattr(result, "musicbrainz_id", None)
                    result_kind = getattr(result, "request_kind", "album")
                    if result_id is None:
                        continue
                    key = f"{result_kind}:{result_id}".casefold()
                    winner_by_key[key] = result
                    generation_by_key[key] = _generation_of(result)
                created_items = [
                    item
                    for item in new_items
                    if f"album:{item['musicbrainz_id']}".casefold() in winner_by_key
                ]
                skipped += len(new_items) - len(created_items)
            elif type(bulk_result) is int:
                # Legacy test doubles returned only a count. Keep this narrow
                # fallback for callers that have not adopted the result API; the
                # production store always returns exact winner objects.
                created_count = max(0, min(len(new_items), bulk_result))
                created_items = new_items[:created_count]
                skipped += len(new_items) - len(created_items)
            else:
                # A lightweight legacy mock with no configured return value.
                created_items = new_items

            # Any rows won by another request between the initial read and the
            # bulk begin are listener attachments, never dispatch candidates.
            raced_items: list[str] = []
            created_keys = {
                str(item["musicbrainz_id"]).casefold() for item in created_items
            }
            for item in new_items:
                mbid = str(item["musicbrainz_id"])
                if mbid.casefold() in created_keys:
                    continue
                record = await self._request_history.async_get_record(
                    mbid, request_kind="album"
                )
                if getattr(record, "status", None) in _ACTIVE_REQUEST_STATUSES:
                    raced_items.append(mbid)
            if raced_items:
                await self._request_history.async_add_requesters(
                    raced_items,
                    user_id,
                    requested_by_name,
                    request_kind="album",
                )

            if existing_items:
                await self._request_history.async_add_requesters(
                    existing_items,
                    user_id,
                    requested_by_name,
                    request_kind="album",
                )

            if not created_items:
                if existing_items or skipped:
                    return BatchRequestResponse(
                        success=True,
                        message="All albums already requested",
                        requested=0,
                        skipped=skipped,
                        status="already_requested",
                    )
                return BatchRequestResponse(
                    success=False,
                    message="Batch request could not be recorded",
                    requested=0,
                    skipped=skipped,
                    status="failed",
                )

            if needs_approval:
                return BatchRequestResponse(
                    success=True,
                    message="Batch request submitted, awaiting admin approval",
                    requested=len(created_items),
                    skipped=skipped,
                    status="awaiting_approval",
                )

            dispatched = 0
            for item in created_items:
                mbid = str(item["musicbrainz_id"])
                generation = generation_by_key.get(f"album:{mbid}".casefold())
                task_id: str | None = None
                try:
                    task_id = await self._acquisition.request_album(
                        user_id=user_id or "",
                        release_group_mbid=mbid,
                        artist_name=item["artist_name"],
                        album_title=item["album_title"],
                        year=item.get("year"),
                        artist_mbid=item.get("artist_mbid"),
                        origin="user",
                        release_mbid=item.get("release_mbid"),
                        track_count_priority=RequestPriority.USER_INITIATED,
                    )
                    if task_id == ALREADY_IN_LIBRARY:
                        kwargs: dict[str, object] = {
                            "completed_at": datetime.now(timezone.utc).isoformat(),
                            "request_kind": "album",
                        }
                        if generation is not None:
                            kwargs["expected_generation"] = generation
                        await self._request_history.async_update_status(
                            mbid,
                            "imported",
                            **kwargs,
                        )
                    else:
                        kwargs = {"request_kind": "album"}
                        if generation is not None:
                            kwargs["expected_generation"] = generation
                        linked = (
                            await self._request_history.async_update_download_task_id(
                                mbid,
                                task_id,
                                **kwargs,
                            )
                        )
                        if not _mutation_won(linked):
                            await self._cancel_orphan_task(task_id, user_id or "")
                            raise ExternalServiceError(
                                "Request generation changed while starting download"
                            )
                    dispatched += 1
                except ValidationError:
                    await self._mark_failed(
                        mbid,
                        "album",
                        expected_generation=generation,
                    )
                except ExternalServiceError:
                    logger.warning("Batch request generation changed for %s", mbid)
                except Exception:  # noqa: BLE001
                    logger.exception("Batch download dispatch failed for %s", mbid)
                    await self._cancel_orphan_task(task_id, user_id or "")
                    await self._mark_failed(
                        mbid,
                        "album",
                        expected_generation=generation,
                    )

            return BatchRequestResponse(
                success=True,
                message=f"Batch request accepted: {dispatched} started",
                requested=dispatched,
                skipped=skipped,
                overflow=0,
                status="pending" if dispatched else "failed",
            )
        except (ExternalServiceError, ValidationError):
            raise
        except Exception as error:  # noqa: BLE001
            logger.exception("Batch request failed")
            raise ExternalServiceError("Batch request failed") from error

    async def cancel_batch(
        self,
        musicbrainz_ids: list[str],
        user_id: str | None = None,
        user_role: str | None = None,
        request_kind: str = "album",
    ) -> BatchCancelResponse:
        # Only an explicitly authenticated admin is authoritative. A missing
        # user_id must never turn an ordinary or unknown role into an admin.
        is_admin = user_role == "admin"
        cancelled = 0
        failed = 0
        for mbid in dict.fromkeys(musicbrainz_ids):
            try:
                record = await self._request_history.async_get_record(
                    mbid, request_kind=request_kind
                )
                if record is None:
                    failed += 1
                    continue

                if not is_admin:
                    decision = (
                        await self._request_history.async_prepare_requester_cancel(
                            user_id or "", mbid, request_kind=request_kind
                        )
                    )
                    if decision.action == "denied":
                        failed += 1
                        continue
                    if decision.action in {"detached", "cancelled"}:
                        cancelled += 1
                        continue
                    if decision.action != "cancel_task":
                        failed += 1
                        continue

                    # ``record`` is the immutable primary attribution captured
                    # before the atomic decision. A co-requester can never replace
                    # it with their own user id while cancelling.
                    current = await self._request_history.async_get_record(
                        mbid, request_kind=request_kind
                    )
                    task_id = (
                        getattr(current, "download_task_id", None)
                        if current is not None
                        else None
                    ) or getattr(record, "download_task_id", None)
                    generation = _generation_of(decision) or _generation_of(record)
                    try:
                        if task_id:
                            await self._get_download_service().cancel_task(
                                task_id,
                                getattr(record, "user_id", None) or "",
                                "user",
                            )
                        kwargs: dict[str, object] = {
                            "completed_at": datetime.now(timezone.utc).isoformat(),
                            "request_kind": request_kind,
                        }
                        if generation is not None:
                            kwargs["expected_generation"] = generation
                        changed = await self._request_history.async_update_status(
                            mbid,
                            "cancelled",
                            **kwargs,
                        )
                        if not _mutation_won(changed):
                            failed += 1
                            continue
                    except Exception:  # noqa: BLE001
                        logger.exception("Batch cancel failed for %s", mbid)
                        try:
                            if decision.prior_status is not None:
                                await (
                                    self._request_history.async_restore_request_status(
                                        mbid,
                                        decision.prior_status,
                                        expected_status=_CANCELLING_STATUS,
                                        expected_generation=generation,
                                        request_kind=request_kind,
                                    )
                                )
                        except Exception:  # noqa: BLE001
                            logger.exception("Failed to restore batch request %s", mbid)
                        failed += 1
                        continue
                    cancelled += 1
                    continue

                status = getattr(record, "status", None)
                if status not in (
                    None,
                    "awaiting_approval",
                    "pending",
                    "queued",
                    "downloading",
                ):
                    failed += 1
                    continue
                generation = _generation_of(record)
                prior_authorized = getattr(record, "dispatch_authorized", None)
                try:
                    task_id = getattr(record, "download_task_id", None)
                    if task_id:
                        await self._get_download_service().cancel_task(
                            task_id,
                            getattr(record, "user_id", None) or "",
                            "admin",
                        )
                    kwargs: dict[str, object] = {
                        "completed_at": datetime.now(timezone.utc).isoformat(),
                        "request_kind": request_kind,
                    }
                    if generation is not None:
                        kwargs["expected_generation"] = generation
                    changed = await self._request_history.async_update_status(
                        mbid,
                        "cancelled",
                        **kwargs,
                    )
                    if not _mutation_won(changed):
                        failed += 1
                        continue
                    # Approval cancellation must revoke the persisted capability
                    # after winning the generation CAS.
                    if status == "awaiting_approval":
                        await self._request_history.async_update_dispatch_authorized(
                            mbid, False, request_kind=request_kind
                        )
                except Exception:  # noqa: BLE001
                    logger.exception("Batch admin cancel failed for %s", mbid)
                    try:
                        if status is not None:
                            restore_kwargs: dict[str, object] = {
                                "request_kind": request_kind
                            }
                            if generation is not None:
                                restore_kwargs["expected_generation"] = generation
                            await self._request_history.async_update_status(
                                mbid, status, **restore_kwargs
                            )
                        if (
                            status == "awaiting_approval"
                            and prior_authorized is not None
                        ):
                            await (
                                self._request_history.async_update_dispatch_authorized(
                                    mbid,
                                    bool(prior_authorized),
                                    request_kind=request_kind,
                                )
                            )
                    except Exception:  # noqa: BLE001
                        logger.exception("Failed to restore batch request %s", mbid)
                    failed += 1
                    continue
                cancelled += 1
            except Exception:  # noqa: BLE001
                logger.exception("Batch cancel failed for %s", mbid)
                failed += 1
        return BatchCancelResponse(
            success=cancelled > 0,
            cancelled=cancelled,
            failed=failed,
            message=f"Cancelled {cancelled} requests"
            + (f", {failed} failed" if failed else ""),
        )
