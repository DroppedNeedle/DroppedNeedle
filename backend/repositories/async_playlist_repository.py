import asyncio
from typing import Optional

from repositories.playlist_repository import (
    PlaylistRecord,
    PlaylistRepository,
    PlaylistSummaryRecord,
    PlaylistTrackRecord,
    _UNSET,
)


class AsyncPlaylistRepository:
    """Async wrapper around PlaylistRepository.

    Delegates all calls to asyncio.to_thread to avoid blocking the event loop.
    """

    def __init__(self, repo: PlaylistRepository):
        self._repo = repo

    async def create_playlist(
        self, name: str, source_ref: str | None = None, user_id: str | None = None,
    ) -> PlaylistRecord:
        return await asyncio.to_thread(self._repo.create_playlist, name, source_ref, user_id)

    async def get_playlist(self, playlist_id: str) -> Optional[PlaylistRecord]:
        return await asyncio.to_thread(self._repo.get_playlist, playlist_id)

    async def get_by_source_ref(
        self, source_ref: str, user_id: str | None = None,
    ) -> Optional[PlaylistRecord]:
        return await asyncio.to_thread(self._repo.get_by_source_ref, source_ref, user_id)

    async def get_imported_source_ids(self, prefix: str, user_id: str | None = None) -> set[str]:
        return await asyncio.to_thread(self._repo.get_imported_source_ids, prefix, user_id)

    async def get_all_playlists(self, user_id: str | None = None) -> list[PlaylistSummaryRecord]:
        return await asyncio.to_thread(self._repo.get_all_playlists, user_id)

    async def get_summary(self, playlist_id: str) -> Optional[PlaylistSummaryRecord]:
        return await asyncio.to_thread(self._repo.get_summary, playlist_id)

    async def set_public(self, playlist_id: str, is_public: bool) -> Optional[PlaylistRecord]:
        return await asyncio.to_thread(self._repo.set_public, playlist_id, is_public)

    async def assign_unowned_to(self, user_id: str) -> int:
        return await asyncio.to_thread(self._repo.assign_unowned_to, user_id)

    async def update_playlist(
        self,
        playlist_id: str,
        name: Optional[str] = None,
        cover_image_path: Optional[str] = _UNSET,
    ) -> Optional[PlaylistRecord]:
        return await asyncio.to_thread(
            self._repo.update_playlist, playlist_id, name, cover_image_path,
        )

    async def delete_playlist(self, playlist_id: str) -> bool:
        return await asyncio.to_thread(self._repo.delete_playlist, playlist_id)

    async def add_tracks(
        self,
        playlist_id: str,
        tracks: list[dict],
        position: Optional[int] = None,
    ) -> list[PlaylistTrackRecord]:
        return await asyncio.to_thread(self._repo.add_tracks, playlist_id, tracks, position)

    async def remove_track(self, playlist_id: str, track_id: str) -> bool:
        return await asyncio.to_thread(self._repo.remove_track, playlist_id, track_id)

    async def remove_tracks(self, playlist_id: str, track_ids: list[str]) -> int:
        return await asyncio.to_thread(self._repo.remove_tracks, playlist_id, track_ids)

    async def reorder_track(
        self, playlist_id: str, track_id: str, new_position: int,
    ) -> Optional[int]:
        return await asyncio.to_thread(
            self._repo.reorder_track, playlist_id, track_id, new_position,
        )

    async def update_track_source(
        self,
        playlist_id: str,
        track_id: str,
        source_type: Optional[str] = None,
        available_sources: Optional[list[str]] = None,
        track_source_id: Optional[str] = None,
        plex_rating_key: Optional[str] = _UNSET,
        library_file_id: Optional[str] = _UNSET,
    ) -> Optional[PlaylistTrackRecord]:
        return await asyncio.to_thread(
            self._repo.update_track_source, playlist_id, track_id,
            source_type, available_sources, track_source_id, plex_rating_key, library_file_id,
        )

    async def batch_update_available_sources(
        self,
        playlist_id: str,
        updates: dict[str, list[str]],
    ) -> int:
        return await asyncio.to_thread(
            self._repo.batch_update_available_sources, playlist_id, updates,
        )

    async def batch_link_library_files(
        self,
        playlist_id: str,
        updates: dict[str, str],
    ) -> int:
        return await asyncio.to_thread(
            self._repo.batch_link_library_files, playlist_id, updates,
        )

    async def get_streamable_counts(self) -> dict[str, tuple[int, int]]:
        return await asyncio.to_thread(self._repo.get_streamable_counts)

    async def get_tracks(self, playlist_id: str) -> list[PlaylistTrackRecord]:
        return await asyncio.to_thread(self._repo.get_tracks, playlist_id)

    async def get_track(self, playlist_id: str, track_id: str) -> Optional[PlaylistTrackRecord]:
        return await asyncio.to_thread(self._repo.get_track, playlist_id, track_id)

    async def check_track_membership(
        self, tracks: list[tuple[str, str, str]], user_id: str | None = None,
    ) -> dict[str, list[int]]:
        return await asyncio.to_thread(self._repo.check_track_membership, tracks, user_id)
