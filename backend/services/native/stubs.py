"""Brownout stubs used while Lidarr is wired out (Phase 1) but the native
library engine (Phase 3) and download client (Phase 6) are not yet built.

Every method returns a safe default (empty collection / ``False`` / ``None``)
so the surrounding services keep serving empty data without raising. Each
method logs once per instance on first call so the dead path is discoverable.
"""

import logging
from typing import TYPE_CHECKING, Any

from models.common import ServiceStatus
from models.library import LibraryAlbum, LibraryGroupedArtist

if TYPE_CHECKING:
    from services.native.library_manager import LibraryStats

logger = logging.getLogger(__name__)


class LibraryStub:
    """Brownout stand-in for ``LidarrRepository``. Replaced by ``LibraryManager`` in Phase 3."""

    def __init__(self) -> None:
        self._logged: set[str] = set()

    def _log_once(self, method: str) -> None:
        if method not in self._logged:
            self._logged.add(method)
            logger.debug("LibraryStub.%s called - replace in Phase 3", method)

    def is_configured(self) -> bool:
        self._log_once("is_configured")
        return True  # scanner is always present; only the data is empty

    def is_library_empty(self) -> bool:
        self._log_once("is_library_empty")
        return True  # no library_files rows yet

    async def has_album(self, mbid: str) -> bool:
        self._log_once("has_album")
        return False

    async def get_library_albums(self) -> list[LibraryAlbum]:
        self._log_once("get_library_albums")
        return []

    async def get_library_album_mbids(self) -> set[str]:
        self._log_once("get_library_album_mbids")
        return set()

    async def get_library_artist_mbids(self) -> set[str]:
        self._log_once("get_library_artist_mbids")
        return set()

    async def get_status(self) -> ServiceStatus:
        self._log_once("get_status")
        return ServiceStatus(status="ok")

    async def get_library(self, include_unmonitored: bool = False) -> list[LibraryAlbum]:
        self._log_once("get_library")
        return []

    async def get_library_grouped(self) -> list[LibraryGroupedArtist]:
        self._log_once("get_library_grouped")
        return []

    async def get_library_mbids(self, include_release_ids: bool = True) -> set[str]:
        self._log_once("get_library_mbids")
        return set()

    async def existing_album_mbids(self, identifiers: list[str]) -> set[str]:
        self._log_once("existing_album_mbids")
        return set()

    async def existing_artist_mbids(self, identifiers: list[str]) -> set[str]:
        self._log_once("existing_artist_mbids")
        return set()

    async def get_artist_mbids(self) -> set[str]:
        self._log_once("get_artist_mbids")
        return set()

    async def get_requested_mbids(self) -> set[str]:
        self._log_once("get_requested_mbids")
        return set()

    async def get_artists_from_library(self, include_unmonitored: bool = False) -> list[dict[str, Any]]:
        self._log_once("get_artists_from_library")
        return []

    async def get_recently_imported(self, limit: int = 20) -> list[LibraryAlbum]:
        self._log_once("get_recently_imported")
        return []

    async def get_home_albums(self, limit: int = 15) -> list[LibraryAlbum]:
        del limit
        self._log_once("get_home_albums")
        return []

    async def get_home_artists(self, limit: int = 15) -> list[dict[str, Any]]:
        del limit
        self._log_once("get_home_artists")
        return []

    async def get_album_image_url(self, album_mbid: str, size: int | None = 500) -> str | None:
        self._log_once("get_album_image_url")
        return None

    async def get_artist_image_url(self, artist_mbid: str, size: int | None = 250) -> str | None:
        self._log_once("get_artist_image_url")
        return None

    async def get_album_tracks(self, album_id: int) -> list[dict[str, Any]]:
        self._log_once("get_album_tracks")
        return []

    async def get_album_by_id(self, album_id: int) -> dict[str, Any] | None:
        self._log_once("get_album_by_id")
        return None

    async def get_album_by_mbid(self, mbid: str) -> dict[str, Any] | None:
        self._log_once("get_album_by_mbid")
        return None

    async def get_track_files_by_album(self, album_id: int) -> list[dict[str, Any]]:
        self._log_once("get_track_files_by_album")
        return []

    async def get_albums_page(
        self,
        page: int = 1,
        page_size: int = 50,
        sort: str = "recent",
        q: str | None = None,
        file_format: str | None = None,
        decade: int | None = None,
    ) -> tuple[list[dict[str, Any]], int]:
        self._log_once("get_albums_page")
        return [], 0

    async def get_crate_tracks(
        self, *, order: str = "random", limit: int = 8, decade: int | None = None
    ) -> list[dict[str, Any]]:
        self._log_once("get_crate_tracks")
        return []

    async def get_decades(self) -> list[dict[str, Any]]:
        self._log_once("get_decades")
        return []

    async def get_tracks(self, release_group_mbid: str) -> list[dict[str, Any]]:
        self._log_once("get_tracks")
        return []

    async def get_file_row_by_id(self, file_id: str) -> dict[str, Any] | None:
        self._log_once("get_file_row_by_id")
        return None

    async def search_tracks(self, q: str, *, limit: int = 30) -> list[dict[str, Any]]:
        self._log_once("search_tracks")
        return []

    async def delete_album(self, album_id: int, delete_files: bool = False) -> bool:
        self._log_once("delete_album")
        return False

    async def delete_artist(self, artist_id: int, delete_files: bool = False) -> bool:
        self._log_once("delete_artist")
        return False

    async def get_stats(self) -> "LibraryStats":
        # LibraryStats lives in library_manager, which imports this module - lazy
        # import to avoid the cycle. Safe zero-stats default for the brownout.
        self._log_once("get_stats")
        from services.native.library_manager import LibraryStats

        return LibraryStats()
