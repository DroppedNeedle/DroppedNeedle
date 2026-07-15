"""Request backend service - unified dispatch contract for album acquisition.

Routes both user submission and admin approval paths through a single backend
abstraction. The native backend preserves the existing acquisition flow,
while the configuration allows extensibility to external backends.

This is the shared seam that upstream receives: all acquisition flows go through
dispatch_request() with a consistent contract, ensuring no duplicate acquisition
and consistent behavior across submission/approval paths.
"""

import logging
from typing import TYPE_CHECKING, Literal

from core.request_backend_settings import RequestBackendSettings
from core.exceptions import ValidationError

if TYPE_CHECKING:
    from services.native.download_service import DownloadService

logger = logging.getLogger(__name__)

# Sentinel values for special dispatch outcomes
ALREADY_IN_LIBRARY = "already_in_library"
DISPATCH_FAILED = "dispatch_failed"


class RequestBackendService:
    """Unified request backend dispatcher.

    Routes album acquisition requests through a configurable backend. The native
    backend dispatches to DownloadService (the existing slskd pipeline). Future
    backends can be added here without changing caller code.

    Both user submission and admin approval paths use this service, ensuring:
    - Consistent dispatch contract across paths
    - No duplicate acquisition (deduplication happens before dispatch)
    - Shared error handling and validation
    """

    def __init__(
        self,
        download_service: "DownloadService",
        settings: RequestBackendSettings,
    ):
        self._download_service = download_service
        self._settings = settings

    async def dispatch_request(
        self,
        user_id: str,
        release_group_mbid: str,
        artist_name: str,
        album_title: str,
        year: int | None,
        artist_mbid: str | None,
        origin: Literal["user", "retry"],
    ) -> str:
        """Dispatch an album acquisition request through the configured backend.

        Args:
            user_id: User ID making the request.
            release_group_mbid: MusicBrainz release group ID.
            artist_name: Artist name.
            album_title: Album title.
            year: Release year.
            artist_mbid: Artist MBID (optional).
            origin: Request origin ('user' for submission, 'retry' for retry).

        Returns:
            str: Task ID from the backend, or ALREADY_IN_LIBRARY sentinel,
                 or DISPATCH_FAILED sentinel on error.

        Raises:
            ValidationError: If quota/cap limits are exceeded (surfaced verbatim).
        """
        backend_type = self._settings.request_backend.backend

        if backend_type == "native":
            return await self._dispatch_native(
                user_id=user_id,
                release_group_mbid=release_group_mbid,
                artist_name=artist_name,
                album_title=album_title,
                year=year,
                artist_mbid=artist_mbid,
                origin=origin,
            )
        elif backend_type == "lidarr":
            # Placeholder for future Lidarr backend integration
            # This would include Lidarr-specific dispatch logic
            logger.warning("request_backend.fallback backend=lidarr fallback=native")
            return await self._dispatch_native(
                user_id=user_id,
                release_group_mbid=release_group_mbid,
                artist_name=artist_name,
                album_title=album_title,
                year=year,
                artist_mbid=artist_mbid,
                origin=origin,
            )
        else:
            logger.error("request_backend.invalid backend=%s", backend_type)
            return DISPATCH_FAILED

    async def _dispatch_native(
        self,
        user_id: str,
        release_group_mbid: str,
        artist_name: str,
        album_title: str,
        year: int | None,
        artist_mbid: str | None,
        origin: Literal["user", "retry"],
    ) -> str:
        """Dispatch through the native DownloadService backend.

        This preserves the existing slskd-based acquisition behavior. The call
        wraps DownloadService.request_album() with consistent error handling.

        Args:
            user_id: User ID making the request.
            release_group_mbid: MusicBrainz release group ID.
            artist_name: Artist name.
            album_title: Album title.
            year: Release year.
            artist_mbid: Artist MBID (optional).
            origin: Request origin ('user' for submission, 'retry' for retry).

        Returns:
            str: Task ID from DownloadService, or ALREADY_IN_LIBRARY sentinel.
        """
        try:
            task_id = await self._download_service.request_album(
                user_id=user_id,
                release_group_mbid=release_group_mbid,
                artist_name=artist_name or "Unknown",
                album_title=album_title or "Unknown",
                year=year,
                artist_mbid=artist_mbid,
                origin=origin,
            )
            return task_id
        except ValidationError:
            # Quota/cap validation errors should be re-raised verbatim so
            # callers can surface the exact reason to the user
            raise
        except Exception:
            logger.error(
                "request_backend.dispatch_failed backend=native release_group_mbid=%s",
                release_group_mbid,
                exc_info=True,
            )
            return DISPATCH_FAILED