"""Target catalog adapter for services that need the library repository protocol."""

from __future__ import annotations

from typing import Any

from infrastructure.persistence.native_library_store import NativeLibraryStore
from infrastructure.persistence.request_history import RequestHistoryStore
from models.common import ServiceStatus
from models.library import LibraryAlbum
from services.native.library_manager import (
    LibraryAlbumSummary,
    LibraryStats,
    LibraryTrack,
)


class TargetLibraryRepository:
    def __init__(
        self,
        store: NativeLibraryStore,
        request_history: RequestHistoryStore | None = None,
    ) -> None:
        self._store = store
        self._request_history = request_history
        self._related_artist_ids: dict[str, str] = {}

    def is_configured(self) -> bool:
        return True

    def is_library_empty(self) -> bool:
        return False

    async def get_status(self) -> ServiceStatus:
        return ServiceStatus(status="ok")

    async def get_library_albums(self) -> list[LibraryAlbum]:
        return await self.get_library()

    async def get_library_album_mbids(self) -> set[str]:
        return await self.get_library_mbids(include_release_ids=False)

    async def get_library_artist_mbids(self) -> set[str]:
        return await self.get_artist_mbids()

    async def get_file_row_by_id(self, file_id: str) -> dict[str, Any] | None:
        row = await self._store.get_target_track(file_id)
        if row is None:
            return None
        row["deleted_at"] = None if row["availability"] == "indexed" else 1
        return row

    async def get_library_file_by_id(self, file_id: str) -> dict[str, Any] | None:
        return await self.get_file_row_by_id(file_id)

    async def has_album_files(self, album_id: str) -> bool:
        return bool(await self._store.get_target_album_tracks(album_id))

    async def resolve_library_album_identifier(self, album_id: str) -> str | None:
        rows = await self._store.get_target_album_tracks(album_id)
        if not rows:
            return None
        provider_ids = {
            str(row["provider_release_group_mbid"])
            for row in rows
            if row.get("provider_release_group_mbid")
        }
        if len(provider_ids) == 1:
            return next(iter(provider_ids))
        local_ids = {str(row["release_group_mbid"]) for row in rows}
        return next(iter(local_ids)) if len(local_ids) == 1 else album_id

    async def get_album_release_mbid(self, album_id: str) -> str | None:
        rows = await self._store.get_target_album_tracks(album_id)
        counts: dict[str, int] = {}
        for row in rows:
            value = row.get("provider_release_mbid")
            if value:
                release_mbid = str(value)
                counts[release_mbid] = counts.get(release_mbid, 0) + 1
        if not counts:
            return None
        return min(counts, key=lambda value: (-counts[value], value))

    async def has_any_files(self) -> bool:
        return await self._store.target_has_any_tracks()

    async def has_track(self, track_id: str) -> bool:
        return await self.has_recording(track_id)

    async def has_album(self, album_id: str) -> bool:
        return await self.has_album_files(album_id)

    async def album_quality_tier(self, album_id: str) -> str | None:
        from services.native.quality_tiers import tier_for, tier_rank

        rows = await self._store.get_target_album_tracks(album_id)
        tiers = [
            tier_for(row.get("file_format") or "", row.get("bit_rate")) for row in rows
        ]
        return min(tiers, key=tier_rank) if tiers else None

    async def recording_quality_tier(self, track_id: str) -> str | None:
        from services.native.quality_tiers import tier_for, tier_rank

        rows = await self._store.get_target_recording_tracks(track_id)
        tiers = [
            tier_for(row.get("file_format") or "", row.get("bit_rate")) for row in rows
        ]
        return max(tiers, key=tier_rank) if tiers else None

    async def list_cutoff_unmet(self, cutoff: str) -> list[dict[str, Any]]:
        from services.native.quality_tiers import tier_rank

        rows, _ = await self._store.list_target_albums(
            limit=100_000, offset=0, sort="name"
        )
        result: list[dict[str, Any]] = []
        for row in rows:
            tier = await self.album_quality_tier(str(row["release_group_mbid"]))
            if tier is not None and tier_rank(tier) < tier_rank(cutoff):
                result.append({**row, "current_tier": tier})
        return result

    async def get_file_rows_for_album(self, album_id: str) -> list[dict[str, Any]]:
        return await self._store.get_target_album_tracks(album_id)

    async def get_library_mbids(self, include_release_ids: bool = True) -> set[str]:
        _revision, albums = await self._store.target_provider_album_snapshot()
        if not include_release_ids:
            return albums
        return albums | await self._store.target_provider_release_ids()

    async def get_artist_mbids(self) -> set[str]:
        return await self._store.target_provider_artist_ids()

    async def existing_album_mbids(self, identifiers: list[str]) -> set[str]:
        normalized = {
            value.strip().casefold() for value in identifiers if value.strip()
        }
        rows = await self._store.target_album_ownership_rows(provider_ids=normalized)
        return {
            str(row["release_group_mbid"]).casefold()
            for row in rows
            if row.get("release_group_mbid")
        }

    async def existing_artist_mbids(self, identifiers: list[str]) -> set[str]:
        return await self._store.target_existing_provider_artist_ids(identifiers)

    async def get_all_album_mbids(self) -> set[str]:
        return await self.get_library_mbids()

    async def get_all_artist_mbids(self) -> set[str]:
        return await self.get_artist_mbids()

    async def get_total_library_bytes(self) -> int:
        return await self._store.get_target_total_library_bytes()

    async def get_user_library_bytes(self, user_id: str) -> int:
        return await self._store.get_target_user_library_bytes(user_id)

    async def get_stats(self) -> LibraryStats:
        stats = await self._store.get_target_library_stats()
        recently_added, _ = await self.get_albums_page(
            page=1, page_size=10, sort="recent"
        )
        return LibraryStats(
            total_albums=int(stats["total_albums"]),
            total_artists=int(stats["total_artists"]),
            total_tracks=int(stats["total_tracks"]),
            total_size_bytes=int(stats["total_size_bytes"]),
            format_breakdown={
                str(key): int(value) for key, value in stats["format_breakdown"].items()
            },
            unmatched_count=int(stats["unmatched_count"]),
            last_scan_at=stats.get("last_scan_at"),
            recently_added=recently_added,
        )

    async def get_cache_stats(self) -> dict[str, Any]:
        stats = await self._store.get_target_library_stats()
        return {
            "artist_count": int(stats["total_artists"]),
            "album_count": int(stats["total_albums"]),
            "db_size_bytes": self._store.db_path.stat().st_size,
            "last_sync": (
                int(stats["last_scan_at"])
                if stats["last_scan_at"] is not None
                else None
            ),
        }

    async def get_artists(self) -> list[dict[str, Any]]:
        rows, _ = await self._store.list_target_artists(
            limit=100_000, offset=0, sort_order="asc"
        )
        return [
            {"mbid": row["provider_artist_mbid"], "name": row["artist_name"]}
            for row in rows
            if row.get("provider_artist_mbid")
        ]

    async def get_albums(self) -> list[dict[str, Any]]:
        rows, _ = await self._store.list_target_albums(
            limit=100_000, offset=0, sort="name"
        )
        return [
            {
                "mbid": row.get("provider_release_group_mbid"),
                "local_id": row["release_group_mbid"],
                "title": row["album_title"],
                "artist_name": row["album_artist_name"],
                "artist_mbid": row.get("provider_artist_mbid"),
                "year": row.get("year"),
            }
            for row in rows
        ]

    async def get_all_albums_for_matching(
        self,
    ) -> list[tuple[str, str, str, str]]:
        rows, _ = await self._store.list_target_albums(
            limit=100_000, offset=0, sort="name"
        )
        return [
            (
                str(row["album_title"]),
                str(row.get("album_artist_name") or ""),
                str(row["provider_release_group_mbid"]),
                str(row.get("provider_artist_mbid") or ""),
            )
            for row in rows
            if row.get("provider_release_group_mbid")
        ]

    async def get_artists_from_library(
        self, include_unmonitored: bool = False
    ) -> list[dict[str, Any]]:
        del include_unmonitored
        rows, _ = await self._store.list_target_artists(
            limit=100_000, offset=0, sort_order="asc"
        )
        return [
            {
                "mbid": row.get("provider_artist_mbid"),
                "local_id": row["artist_mbid"],
                "name": row["artist_name"],
                "album_count": int(row.get("album_count") or 0),
                "date_added": row.get("date_added"),
            }
            for row in rows
        ]

    async def get_library(
        self, include_unmonitored: bool = False
    ) -> list[LibraryAlbum]:
        del include_unmonitored
        rows, _ = await self._store.list_target_albums(
            limit=100_000, offset=0, sort="name"
        )
        return [self._to_library_album(row) for row in rows]

    async def get_recently_imported(self, limit: int = 20) -> list[LibraryAlbum]:
        rows, _ = await self._store.list_target_albums(
            limit=limit, offset=0, sort="recent"
        )
        return [self._to_library_album(row) for row in rows]

    async def get_home_albums(self, limit: int = 15) -> list[LibraryAlbum]:
        rows, _ = await self._store.list_target_albums(
            limit=min(max(limit, 1), 500), offset=0, sort="recent"
        )
        return [self._to_library_album(row) for row in rows]

    async def get_home_artists(self, limit: int = 15) -> list[dict[str, Any]]:
        rows, _ = await self._store.list_target_artists(
            limit=min(max(limit, 1), 500), offset=0, sort_order="asc"
        )
        return [
            {
                "mbid": row.get("provider_artist_mbid"),
                "local_id": row["artist_mbid"],
                "name": row["artist_name"],
                "album_count": int(row.get("album_count") or 0),
                "date_added": row.get("date_added"),
            }
            for row in rows
        ]

    async def get_albums_page(
        self,
        page: int = 1,
        page_size: int = 50,
        sort: str = "recent",
        q: str | None = None,
        file_format: str | None = None,
        decade: int | None = None,
    ) -> tuple[list[LibraryAlbumSummary], int]:
        rows, total = await self._store.list_target_albums(
            limit=page_size,
            offset=(max(page, 1) - 1) * max(page_size, 1),
            sort=sort,
            search=q,
            from_year=decade,
            to_year=decade + 9 if decade is not None else None,
            file_format=file_format,
        )
        return [self._to_album_summary(row) for row in rows], total

    async def get_tracks(self, album_id: str) -> list[LibraryTrack]:
        rows = await self._store.get_target_album_tracks(album_id)
        return [self._to_track(row) for row in rows]

    async def get_album_by_id(self, album_id: str | int) -> dict[str, Any] | None:
        rows, _ = await self._store.list_target_albums(
            limit=1, offset=0, sort="name", album_ids=[str(album_id)]
        )
        return self._to_legacy_album(rows[0]) if rows else None

    async def get_album_by_mbid(self, mbid: str) -> dict[str, Any] | None:
        return await self.get_album_by_id(mbid)

    async def get_album_tracks(self, album_id: str | int) -> list[dict[str, Any]]:
        rows = await self._store.get_target_album_tracks(str(album_id))
        return [
            {
                "id": row["id"],
                "track_file_id": row["id"],
                "has_file": row["availability"] == "indexed",
                "title": row.get("track_title") or "",
                "track_number": row.get("track_number"),
                "disc_number": row.get("disc_number"),
                "duration_ms": int(float(row.get("duration_seconds") or 0) * 1000),
            }
            for row in rows
        ]

    async def get_track_files_by_album(
        self, album_id: str | int
    ) -> list[dict[str, Any]]:
        rows = await self._store.get_target_album_tracks(str(album_id))
        return [
            {
                "id": row["id"],
                "path": row.get("file_path") or "",
                "size": int(row.get("file_size_bytes") or 0),
                "dateAdded": row.get("imported_at"),
                "quality": {
                    "quality": {"bitrate": row.get("bit_rate")},
                },
            }
            for row in rows
        ]

    async def get_requested_mbids(self, ids: list[str] | None = None) -> set[str]:
        if self._request_history is None:
            return set()
        if ids is not None:
            return await self._request_history.async_existing_requested_mbids(ids)
        return await self._request_history.async_get_requested_mbids()

    async def has_recording(self, track_id: str) -> bool:
        return bool(await self._store.get_target_recording_tracks(track_id))

    async def get_library_files_for_recording(
        self, track_id: str
    ) -> list[dict[str, Any]]:
        return await self._store.get_target_recording_tracks(track_id)

    async def get_library_files_for_album(self, album_id: str) -> list[dict[str, Any]]:
        return await self._store.get_target_album_tracks(album_id)

    async def get_library_files_by_ids(
        self, file_ids: list[str]
    ) -> dict[str, dict[str, Any]]:
        return await self._store.get_target_tracks_by_ids(file_ids)

    async def get_albums_aggregated(
        self,
        *,
        limit: int,
        offset: int,
        sort: str = "recent",
        q: str | None = None,
        file_format: str | None = None,
        decade: int | None = None,
        from_year: int | None = None,
        to_year: int | None = None,
        genre: str | None = None,
        release_group_mbids: list[str] | None = None,
    ) -> tuple[list[dict[str, Any]], int]:
        return await self._store.list_target_albums(
            limit=limit,
            offset=offset,
            sort=sort,
            search=q,
            from_year=decade if decade is not None else from_year,
            to_year=decade + 9 if decade is not None else to_year,
            genre=genre,
            album_ids=release_group_mbids,
            file_format=file_format,
        )

    async def get_artists_aggregated(
        self,
        *,
        limit: int,
        offset: int,
        sort_by: str,
        sort_order: str,
        q: str | None,
    ) -> tuple[list[dict[str, Any]], int]:
        del sort_by
        rows, total = await self._store.list_target_artists(
            limit=limit,
            offset=offset,
            search=q,
            sort_order=sort_order,
        )
        return rows, total

    async def get_tracks_paginated(
        self,
        *,
        limit: int,
        offset: int,
        sort: str,
        q: str | None,
    ) -> tuple[list[dict[str, Any]], int]:
        return await self._store.list_target_tracks(
            limit=limit, offset=offset, sort=sort, search=q
        )

    async def get_crate_tracks(
        self, *, order: str, limit: int, decade: int | None = None
    ) -> list[dict[str, Any]]:
        rows, _ = await self._store.list_target_tracks(
            limit=limit,
            offset=0,
            sort="random" if order == "random" else "recent",
            from_year=decade,
            to_year=decade + 9 if decade is not None else None,
        )
        return rows

    async def search_tracks(self, q: str, *, limit: int) -> list[dict[str, Any]]:
        rows, _ = await self._store.list_target_tracks(
            limit=limit, offset=0, sort="title", search=q
        )
        return rows

    async def get_decades(self) -> list[dict[str, int]]:
        return await self._store.target_decades()

    async def get_library_stats(self) -> dict[str, Any]:
        return await self._store.get_target_library_stats()

    async def existing_compat_ids(
        self,
        *,
        artist_ids: list[str],
        album_ids: list[str],
        track_ids: list[str],
    ) -> dict[str, set[str]]:
        return await self._store.target_existing_ids(
            artist_ids=artist_ids, album_ids=album_ids, track_ids=track_ids
        )

    async def get_library_revision(self) -> int:
        return await self._store.get_catalog_revision()

    async def get_genres(self) -> list[dict[str, Any]]:
        return await self._store.list_target_genres()

    async def get_files_by_genre(
        self, genre: str, *, limit: int, offset: int
    ) -> list[dict[str, Any]]:
        rows, _ = await self._store.list_target_tracks(
            limit=limit, offset=offset, genre=genre
        )
        return rows

    async def get_files_by_artist_name(
        self, artist_name: str, *, limit: int
    ) -> list[dict[str, Any]]:
        rows, _ = await self._store.list_target_tracks(
            limit=limit, offset=0, artist_name=artist_name
        )
        return rows

    async def get_random_files(
        self,
        *,
        limit: int,
        genre: str | None = None,
        from_year: int | None = None,
        to_year: int | None = None,
    ) -> list[dict[str, Any]]:
        rows, _ = await self._store.list_target_tracks(
            limit=limit,
            offset=0,
            sort="random",
            genre=genre,
            from_year=from_year,
            to_year=to_year,
        )
        return rows

    async def get_related_artist_mbids(self, artist_id: str) -> str | None:
        return self._related_artist_ids.get(artist_id)

    async def set_related_artist_mbids(self, artist_id: str, value: str) -> None:
        self._related_artist_ids[artist_id] = value

    async def get_files_by_artist_mbids(
        self, artist_ids: list[str], *, limit: int
    ) -> list[dict[str, Any]]:
        rows, _ = await self._store.list_target_tracks(
            limit=limit, offset=0, artist_ids=artist_ids
        )
        return rows

    async def get_files_by_release_group_mbids(
        self, album_ids: list[str], *, limit: int
    ) -> list[dict[str, Any]]:
        rows: list[dict[str, Any]] = []
        for album_id in album_ids:
            rows.extend(await self._store.get_target_album_tracks(album_id))
            if len(rows) >= limit:
                break
        return rows[:limit]

    async def get_files_by_album_artist_mbids(
        self, artist_ids: list[str], *, limit: int
    ) -> list[dict[str, Any]]:
        rows, _ = await self._store.list_target_tracks(
            limit=limit,
            offset=0,
            artist_ids=artist_ids,
            album_artist_only=True,
        )
        return rows

    async def get_albums_for_artist(self, artist_id: str) -> list[dict[str, Any]]:
        rows, _ = await self._store.list_target_albums(
            limit=10_000, offset=0, sort="name", artist_id=artist_id
        )
        return rows

    @staticmethod
    def _to_library_album(row: dict[str, Any]) -> LibraryAlbum:
        imported_at = row.get("last_imported_at")
        return LibraryAlbum(
            artist=str(row.get("album_artist_name") or "Unknown Artist"),
            album=str(row.get("album_title") or "Unknown Album"),
            local_id=str(row["release_group_mbid"]),
            year=row.get("year"),
            quality=row.get("file_format"),
            cover_url=row.get("cover_url"),
            musicbrainz_id=row.get("provider_release_group_mbid"),
            artist_mbid=row.get("provider_artist_mbid"),
            date_added=int(imported_at) if imported_at is not None else None,
        )

    @staticmethod
    def _to_album_summary(row: dict[str, Any]) -> LibraryAlbumSummary:
        return LibraryAlbumSummary(
            release_group_mbid=str(row["release_group_mbid"]),
            album_title=str(row.get("album_title") or ""),
            album_artist_name=row.get("album_artist_name"),
            track_count=int(row.get("track_count") or 0),
            total_size_bytes=int(row.get("total_size_bytes") or 0),
            quality_format=row.get("file_format"),
            year=row.get("year"),
            is_compilation=bool(row.get("is_compilation")),
            cover_url=row.get("cover_url"),
            last_imported_at=row.get("last_imported_at"),
            album_artist_mbid=str(row["album_artist_mbid"]),
            album_sort_name=row.get("album_sort_name"),
            original_release_date=row.get("original_release_date"),
        )

    @staticmethod
    def _to_track(row: dict[str, Any]) -> LibraryTrack:
        return LibraryTrack(
            id=str(row["id"]),
            recording_mbid=row.get("recording_mbid"),
            disc_number=int(row.get("disc_number") or 1),
            track_number=int(row.get("track_number") or 0),
            track_title=str(row.get("track_title") or ""),
            artist_name=row.get("artist_name"),
            file_path=str(row.get("file_path") or ""),
            file_format=row.get("file_format"),
            bit_rate=row.get("bit_rate"),
            sample_rate=row.get("sample_rate"),
            bit_depth=row.get("bit_depth"),
            duration_seconds=row.get("duration_seconds"),
            file_size_bytes=int(row.get("file_size_bytes") or 0),
        )

    @staticmethod
    def _to_legacy_album(row: dict[str, Any]) -> dict[str, Any]:
        return {
            "id": str(row["release_group_mbid"]),
            "title": str(row.get("album_title") or ""),
            "foreignAlbumId": row.get("provider_release_group_mbid"),
            "artist": {
                "id": str(row["album_artist_mbid"]),
                "artistName": str(row.get("album_artist_name") or ""),
                "foreignArtistId": row.get("provider_artist_mbid"),
            },
        }
