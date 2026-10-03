"""Target-only adapters over reference transactions owned by NativeLibraryStore."""

from __future__ import annotations

import json
import time
from datetime import datetime, timezone
from typing import Any
from uuid import uuid4

from infrastructure.persistence.compat_bookmark_store import CompatBookmark
from infrastructure.persistence.compat_play_queue_store import CompatPlayQueue
from infrastructure.persistence.native_library_store import NativeLibraryStore
from infrastructure.persistence.play_history_store import PlayHistoryRecord
from repositories.playlist_repository import (
    PlaylistRecord,
    PlaylistSummaryRecord,
    PlaylistTrackRecord,
    _UNSET,
)
from services.native.target_catalog_writer_service import TargetCatalogWriterService


class TargetDiscoveryBatchLibraryService:
    def __init__(self, writer: TargetCatalogWriterService) -> None:
        self._writer = writer

    async def remove_album(
        self, album_id: str, delete_files: bool = False, *, to_recycle: bool = False
    ) -> None:
        await self._writer.remove_album(
            album_id,
            actor_user_id=None,
            delete_files=delete_files or to_recycle,
            recycle_files=to_recycle,
        )


class TargetAlbumReleasePinStore:
    """Edition pins keyed through target local album identity."""

    def __init__(self, store: NativeLibraryStore) -> None:
        self._store = store

    async def get(self, album_identifier: str) -> str | None:
        return await self._store.get_target_album_release_pin(album_identifier)

    async def set(
        self,
        album_identifier: str,
        release_mbid: str,
        set_by_user_id: str | None = None,
    ) -> None:
        await self._store.set_target_album_release_pin(
            album_identifier,
            release_mbid,
            set_by_user_id,
            datetime.now(timezone.utc).isoformat(),
        )

    async def clear(self, album_identifier: str) -> bool:
        return await self._store.clear_target_album_release_pin(album_identifier)


class TargetGenreIndex:
    """Shared genre metadata projected through target catalog membership."""

    def __init__(self, store: NativeLibraryStore) -> None:
        self._store = store

    async def get_artists_by_genre(
        self, genre: str, limit: int = 50
    ) -> list[dict[str, Any]]:
        return await self._store.get_target_artists_by_genre(genre, limit=limit)

    async def get_albums_by_genre(
        self, genre: str, limit: int = 50
    ) -> list[dict[str, Any]]:
        return await self._store.get_target_albums_by_genre(genre, limit=limit)

    async def get_top_genres(self, limit: int = 20) -> list[tuple[str, int]]:
        return await self._store.get_target_top_genres(limit=limit)

    async def get_genre_artist_counts(self, genres: list[str]) -> dict[str, int]:
        return await self._store.get_target_genre_artist_counts(genres)

    async def get_artists_for_genres(self, genres: list[str]) -> dict[str, list[str]]:
        return await self._store.get_target_artists_for_genres(genres)

    async def get_genres_for_artists(
        self, artist_mbids: list[str]
    ) -> dict[str, list[str]]:
        return await self._store.get_target_genres_for_artists(artist_mbids)

    async def get_underrepresented_genres(
        self, known_genres: list[str], threshold: int = 2
    ) -> list[str]:
        return await self._store.get_target_underrepresented_genres(
            known_genres, threshold=threshold
        )


def _summary_record(row: dict[str, Any]) -> PlaylistSummaryRecord:
    """F-TARGETCATALOG-07: shared row conversion so the full-list and the
    ID-filtered summary paths produce identical records."""
    return PlaylistSummaryRecord(
        id=str(row["id"]),
        name=str(row["name"]),
        cover_image_path=row.get("cover_image_path"),
        created_at=str(row["created_at"]),
        updated_at=str(row["updated_at"]),
        track_count=int(row["track_count"]),
        total_duration=int(row["total_duration"]),
        cover_urls=(
            [value for value in str(row["cover_urls"]).split(",") if value]
            if row.get("cover_urls")
            else []
        ),
        source_ref=row.get("source_ref"),
        user_id=row.get("user_id"),
        is_public=bool(row.get("is_public")),
    )


def _playlist_record(row: dict[str, Any]) -> PlaylistRecord:
    return PlaylistRecord(
        id=str(row["id"]),
        name=str(row["name"]),
        cover_image_path=row.get("cover_image_path"),
        created_at=str(row["created_at"]),
        updated_at=str(row["updated_at"]),
        source_ref=row.get("source_ref"),
        user_id=row.get("user_id"),
        is_public=bool(row.get("is_public")),
    )


def _playlist_track_record(row: dict[str, Any]) -> PlaylistTrackRecord:
    raw_sources = row.get("available_sources")
    available_sources: list[str] | None = None
    if isinstance(raw_sources, str):
        try:
            decoded = json.loads(raw_sources)
        except json.JSONDecodeError:
            decoded = None
        if isinstance(decoded, list) and all(isinstance(item, str) for item in decoded):
            available_sources = decoded
    elif isinstance(raw_sources, list) and all(
        isinstance(item, str) for item in raw_sources
    ):
        available_sources = raw_sources
    local_track_id = row.get("local_track_id")
    return PlaylistTrackRecord(
        id=str(row["id"]),
        playlist_id=str(row["playlist_id"]),
        position=int(row["position"]),
        track_name=str(row["track_name"]),
        artist_name=str(row["artist_name"]),
        album_name=str(row["album_name"]),
        album_id=row.get("local_album_id") or row.get("album_id"),
        artist_id=row.get("local_artist_id") or row.get("artist_id"),
        track_source_id=local_track_id or row.get("track_source_id"),
        cover_url=row.get("cover_url"),
        source_type=str(row["source_type"]),
        available_sources=available_sources,
        format=row.get("format"),
        track_number=row.get("track_number"),
        disc_number=row.get("disc_number"),
        duration=row.get("duration"),
        created_at=str(row["created_at"]),
        plex_rating_key=row.get("plex_rating_key"),
        library_file_id=local_track_id or row.get("library_file_id"),
        origin_recording_mbid=row.get("origin_recording_mbid")
        or (
            row.get("track_source_id")
            if not row.get("source_type")
            else None
        ),
    )


class TargetPlaylistRepository:
    """Async playlist repository over target tables owned by NativeLibraryStore."""

    def __init__(self, store: NativeLibraryStore) -> None:
        self._store = store

    @staticmethod
    def _now() -> str:
        return datetime.now(timezone.utc).isoformat()

    async def create_playlist(
        self, name: str, source_ref: str | None = None, user_id: str | None = None
    ) -> PlaylistRecord:
        row = await self._store.create_target_playlist(
            playlist_id=str(uuid4()),
            name=name,
            source_ref=source_ref,
            user_id=user_id or "",
            created_at=self._now(),
        )
        return _playlist_record(row)

    async def get_playlist(self, playlist_id: str) -> PlaylistRecord | None:
        row = await self._store.get_target_playlist(playlist_id)
        return _playlist_record(row) if row is not None else None

    async def get_by_source_ref(
        self, source_ref: str, user_id: str | None = None
    ) -> PlaylistRecord | None:
        row = await self._store.get_target_playlist_by_source(source_ref, user_id)
        return _playlist_record(row) if row is not None else None

    async def get_imported_source_ids(
        self, prefix: str, user_id: str | None = None
    ) -> set[str]:
        return await self._store.get_target_imported_playlist_source_ids(
            prefix, user_id
        )

    async def get_all_playlists(
        self, user_id: str | None = None
    ) -> list[PlaylistSummaryRecord]:
        rows = await self._store.list_target_playlists(user_id)
        return [_summary_record(row) for row in rows]

    async def get_summary(self, playlist_id: str) -> PlaylistSummaryRecord | None:
        # F-TARGETCATALOG-07: one ID-filtered aggregate instead of loading
        # and filtering the full playlist list.
        row = await self._store.get_target_playlist_summary(playlist_id)
        return _summary_record(row) if row is not None else None

    async def set_public(
        self, playlist_id: str, is_public: bool
    ) -> PlaylistRecord | None:
        row = await self._store.set_target_playlist_public(
            playlist_id, is_public, self._now()
        )
        return _playlist_record(row) if row is not None else None

    async def update_playlist(
        self,
        playlist_id: str,
        name: str | None = None,
        cover_image_path: str | None = _UNSET,
    ) -> PlaylistRecord | None:
        row = await self._store.update_target_playlist(
            playlist_id,
            name=name,
            cover_image_path=cover_image_path,
            changed_at=self._now(),
            cover_unchanged=_UNSET,
        )
        return _playlist_record(row) if row is not None else None

    async def delete_playlist(self, playlist_id: str) -> bool:
        return await self._store.delete_target_playlist(playlist_id)

    async def add_tracks(
        self,
        playlist_id: str,
        tracks: list[dict[str, Any]],
        position: int | None = None,
    ) -> list[PlaylistTrackRecord]:
        created_at = self._now()
        prepared = [
            {**track, "id": str(uuid4()), "created_at": created_at} for track in tracks
        ]
        rows = await self._store.add_target_playlist_tracks(
            playlist_id, prepared, position=position, changed_at=created_at
        )
        return [_playlist_track_record(row) for row in rows]

    async def remove_track(self, playlist_id: str, track_id: str) -> bool:
        return bool(
            await self._store.remove_target_playlist_tracks(
                playlist_id, [track_id], self._now()
            )
        )

    async def remove_tracks(self, playlist_id: str, track_ids: list[str]) -> int:
        return await self._store.remove_target_playlist_tracks(
            playlist_id, track_ids, self._now()
        )

    async def reorder_track(
        self, playlist_id: str, track_id: str, new_position: int
    ) -> int | None:
        return await self._store.reorder_target_playlist_track(
            playlist_id, track_id, new_position, self._now()
        )

    async def update_track_source(
        self,
        playlist_id: str,
        track_id: str,
        source_type: str | None = None,
        available_sources: list[str] | None = None,
        track_source_id: str | None = None,
        plex_rating_key: str | None = _UNSET,
        library_file_id: str | None = _UNSET,
    ) -> PlaylistTrackRecord | None:
        row = await self._store.update_target_playlist_track_source(
            playlist_id,
            track_id,
            source_type=source_type,
            available_sources=available_sources,
            track_source_id=track_source_id,
            plex_rating_key=plex_rating_key,
            library_file_id=library_file_id,
            unchanged=_UNSET,
            changed_at=self._now(),
        )
        return _playlist_track_record(row) if row is not None else None

    async def batch_update_available_sources(
        self, playlist_id: str, updates: dict[str, list[str]]
    ) -> int:
        return await self._store.update_target_playlist_sources(
            playlist_id, updates, self._now()
        )

    async def batch_link_library_files(
        self, playlist_id: str, updates: dict[str, str]
    ) -> int:
        return await self._store.link_target_playlist_tracks(
            playlist_id, updates, self._now()
        )

    async def get_streamable_counts(self) -> dict[str, tuple[int, int]]:
        return await self._store.target_playlist_streamable_counts()

    async def get_tracks(self, playlist_id: str) -> list[PlaylistTrackRecord]:
        return [
            _playlist_track_record(row)
            for row in await self._store.list_target_playlist_tracks(playlist_id)
        ]

    async def get_track(
        self, playlist_id: str, track_id: str
    ) -> PlaylistTrackRecord | None:
        row = await self._store.get_target_playlist_track(playlist_id, track_id)
        return _playlist_track_record(row) if row is not None else None

    async def check_track_membership(
        self, tracks: list[tuple[str, str, str]], user_id: str | None = None
    ) -> dict[str, list[int]]:
        return await self._store.target_playlist_membership(tracks, user_id)


class TargetFavoritesStore:
    def __init__(self, store: NativeLibraryStore) -> None:
        self._store = store

    async def add(
        self, user_id: str, item_kind: str, item_id: str, created_at: float
    ) -> None:
        await self._store.add_target_favorite(user_id, item_kind, item_id, created_at)

    async def apply_many(
        self,
        user_id: str,
        targets: list[tuple[str, str]],
        *,
        add: bool,
        created_at: float,
    ) -> None:
        await self._store.apply_target_favorites(
            user_id, targets, add=add, created_at=created_at
        )

    async def remove(self, user_id: str, item_kind: str, item_id: str) -> None:
        await self._store.remove_target_favorite(user_id, item_kind, item_id)

    async def is_favorite(self, user_id: str, item_kind: str, item_id: str) -> bool:
        return item_id in await self._store.target_favorite_map(
            user_id, item_kind, [item_id]
        )

    async def list(self, user_id: str, item_kind: str) -> list[tuple[str, float]]:
        return await self._store.list_target_favorites(user_id, item_kind)

    async def map_for_items(
        self, user_id: str, item_kind: str, item_ids: list[str]
    ) -> dict[str, float]:
        return await self._store.target_favorite_map(user_id, item_kind, item_ids)


class TargetPlayHistoryStore:
    def __init__(self, store: NativeLibraryStore) -> None:
        self._store = store

    async def insert(
        self,
        user_id: str,
        *,
        track_name: str,
        artist_name: str,
        played_at: str,
        album_name: str | None = None,
        recording_mbid: str | None = None,
        release_group_mbid: str | None = None,
        duration_ms: int | None = None,
        source: str | None = None,
    ) -> str:
        track_id = await self._store.resolve_target_track_for_history(
            recording_mbid=recording_mbid,
            album_identifier=release_group_mbid,
            track_name=track_name,
            artist_name=artist_name,
        )
        history_id = uuid4().hex
        await self._store.insert_target_play_history(
            history_id=history_id,
            user_id=user_id,
            track_id=track_id,
            track_name=track_name,
            artist_name=artist_name,
            played_at=played_at,
            album_name=album_name,
            duration_ms=duration_ms,
            source=source,
        )
        return history_id

    async def compat_stats(
        self,
        user_id: str,
        *,
        recording_mbids: list[str],
        release_group_mbids: list[str],
        artist_names: list[str],
    ) -> dict[str, dict[str, tuple[int, str]]]:
        return await self._store.target_play_history_stats(
            user_id,
            track_ids=recording_mbids,
            album_ids=release_group_mbids,
            artist_ids=artist_names,
        )

    async def recent(self, user_id: str, limit: int = 50) -> list[PlayHistoryRecord]:
        rows = await self._store.list_target_play_history(user_id, limit=limit)
        return [PlayHistoryRecord(**row) for row in rows]

    async def play_counts_by_artist(
        self, user_id: str, artist_name: str
    ) -> dict[str, int]:
        return await self._store.target_play_counts_by_artist(user_id, artist_name)

    async def album_ids(
        self,
        user_id: str,
        *,
        frequent: bool,
        limit: int,
        offset: int,
    ) -> list[str]:
        return await self._store.target_history_album_ids(
            user_id,
            frequent=frequent,
            limit=limit,
            offset=offset,
        )


class TargetBookmarkStore:
    def __init__(self, store: NativeLibraryStore) -> None:
        self._store = store

    async def list(self, user_id: str) -> list[CompatBookmark]:
        rows = await self._store.list_target_bookmarks(user_id)
        return [CompatBookmark(**row) for row in rows]

    async def upsert(
        self, user_id: str, file_id: str, position_ms: int, comment: str
    ) -> None:
        await self._store.upsert_target_bookmark(
            user_id, file_id, position_ms, comment, time.time()
        )

    async def delete(self, user_id: str, file_id: str) -> None:
        await self._store.delete_target_bookmark(user_id, file_id)


class TargetPlayQueueStore:
    def __init__(self, store: NativeLibraryStore) -> None:
        self._store = store

    async def get(self, user_id: str) -> CompatPlayQueue:
        return CompatPlayQueue(**await self._store.get_target_play_queue(user_id))

    async def replace(
        self,
        user_id: str,
        file_ids: tuple[str, ...],
        *,
        current_index: int | None,
        position_ms: int,
        changed_by_client: str,
    ) -> CompatPlayQueue:
        result = await self._store.replace_target_play_queue(
            user_id,
            file_ids,
            current_index=current_index,
            position_ms=position_ms,
            changed_by_client=changed_by_client,
            updated_at=time.time(),
        )
        return CompatPlayQueue(**result)


class TargetCompatIdMapStore:
    def __init__(self, store: NativeLibraryStore) -> None:
        self._store = store

    async def get_jf_id(self, kind: str, internal_id: str) -> str | None:
        return await self._store.get_target_compat_id(kind, internal_id)

    async def get_mapping(self, jf_id: str) -> tuple[str, str] | None:
        return await self._store.get_target_compat_mapping(jf_id)

    async def insert(self, jf_id: str, kind: str, internal_id: str) -> None:
        await self._store.insert_target_compat_mapping(jf_id, kind, internal_id)
