"""LibraryViewService - the shared neutral read model over the owned library.

Both shims read through this service and receive neutral View DTOs, so the two
protocols return identical data. Active rows only.
"""

from __future__ import annotations

from collections import Counter
from typing import TYPE_CHECKING

from services.compat.view_models import ViewAlbum, ViewArtist, ViewGenre, ViewTrack

if TYPE_CHECKING:
    from infrastructure.persistence.auth_store import UserRecord
    from infrastructure.persistence.library_db import LibraryDB
    from repositories.coverart_repository import CoverArtRepository
    from services.compat.favorites_service import FavoritesService
    from services.native.library_manager import (
        LibraryAlbumSummary,
        LibraryArtistSummary,
        LibraryManager,
        LibraryTrackListItem,
    )


def _dominant_genre(rows: list[dict]) -> str | None:
    """Most frequent non-null genre across an album's tracks (tie -> first in
    disc/track order)."""
    counts: Counter[str] = Counter()
    for row in rows:
        g = row.get("genre")
        if g:
            counts[g] += 1
    if not counts:
        return None
    return counts.most_common(1)[0][0]


class LibraryViewService:
    def __init__(
        self,
        library_manager: "LibraryManager",
        library_db: "LibraryDB",
        coverart_repository: "CoverArtRepository",
        favorites_service: "FavoritesService",
    ) -> None:
        self._lm = library_manager
        self._db = library_db
        self._cover = coverart_repository
        self._fav = favorites_service

    async def get_artists(
        self,
        *,
        limit: int = 50,
        offset: int = 0,
        sort_by: str = "name",
        sort_order: str = "asc",
        q: str | None = None,
        user: "UserRecord | None" = None,
    ) -> tuple[list[ViewArtist], int]:
        # synthetic (Q14) ids stay: compat browse-by-id keys off them for MBID-less artists
        items, total = await self._lm.get_artists(
            limit=limit, offset=offset, sort_by=sort_by, sort_order=sort_order, q=q,
            include_synthetic_mbids=True,
        )
        artists = [self._artist_from_summary(s) for s in items]
        await self._overlay_favorites(artists, "artist", lambda a: a.artist_mbid, user)
        return artists, total

    async def get_albums(
        self,
        *,
        page: int = 1,
        page_size: int = 50,
        sort: str = "recent",
        q: str | None = None,
        file_format: str | None = None,
        decade: int | None = None,
        user: "UserRecord | None" = None,
    ) -> tuple[list[ViewAlbum], int]:
        items, total = await self._lm.get_albums_page(
            page=page, page_size=page_size, sort=sort, q=q,
            file_format=file_format, decade=decade,
        )
        albums = [self._album_from_summary(s) for s in items]
        await self._overlay_favorites(albums, "album", lambda a: a.rg_mbid, user)
        return albums, total

    async def get_albums_for_artist(
        self, artist_mbid: str, *, user: "UserRecord | None" = None
    ) -> list[ViewAlbum]:
        rows = await self._db.get_albums_for_artist(artist_mbid)
        albums = [
            ViewAlbum(
                rg_mbid=r["release_group_mbid"],
                title=r.get("album_title") or "",
                artist_name=r.get("album_artist_name"),
                artist_mbid=r.get("album_artist_mbid"),
                year=r.get("year"),
                track_count=r.get("track_count"),
                cover_available=bool(r.get("cover_url")),
                date_added=int(r["last_imported_at"]) if r.get("last_imported_at") else None,
                is_compilation=bool(r.get("is_compilation")),
            )
            for r in rows
        ]
        await self._overlay_favorites(albums, "album", lambda a: a.rg_mbid, user)
        return albums

    async def get_artist_with_albums(
        self, artist_mbid: str, *, user: "UserRecord | None" = None
    ) -> tuple[ViewArtist, list[ViewAlbum]] | None:
        """None if the artist owns nothing."""
        albums = await self.get_albums_for_artist(artist_mbid, user=user)
        if not albums:
            return None
        name = next((a.artist_name for a in albums if a.artist_name), "Unknown Artist")
        artist = ViewArtist(
            artist_mbid=artist_mbid, name=name, album_count=len(albums)
        )
        await self._overlay_favorites([artist], "artist", lambda a: a.artist_mbid, user)
        return artist, albums

    async def get_album(
        self, rg_mbid: str, *, user: "UserRecord | None" = None
    ) -> ViewAlbum | None:
        rows = await self._db.get_library_files_for_album(rg_mbid)
        if not rows:
            return None
        album = await self._album_from_rows(rg_mbid, rows)
        await self._overlay_favorites([album], "album", lambda a: a.rg_mbid, user)
        return album

    async def get_album_tracks(
        self, rg_mbid: str, *, user: "UserRecord | None" = None
    ) -> list[ViewTrack]:
        rows = await self._db.get_library_files_for_album(rg_mbid)
        tracks = [self._track_from_row(r) for r in rows]
        await self._overlay_favorites(tracks, "track", lambda t: t.file_id, user)
        return tracks

    async def get_track(
        self, file_id: str, *, user: "UserRecord | None" = None
    ) -> ViewTrack | None:
        row = await self._db.get_library_file_by_id(file_id)
        if row is None or row.get("deleted_at") is not None:
            return None
        track = self._track_from_row(row)
        await self._overlay_favorites([track], "track", lambda t: t.file_id, user)
        return track

    async def get_tracks_page(
        self,
        *,
        limit: int = 48,
        offset: int = 0,
        sort: str = "recent",
        q: str | None = None,
        user: "UserRecord | None" = None,
    ) -> tuple[list[ViewTrack], int]:
        items, total = await self._lm.get_tracks_page(
            limit=limit, offset=offset, sort=sort, q=q
        )
        tracks = [self._track_from_list_item(i) for i in items]
        await self._overlay_favorites(tracks, "track", lambda t: t.file_id, user)
        return tracks, total

    async def get_genres(self) -> list[ViewGenre]:
        rows = await self._db.get_genres()
        return [
            ViewGenre(
                name=r["genre"],
                song_count=int(r.get("song_count") or 0),
                album_count=int(r.get("album_count") or 0),
            )
            for r in rows
        ]

    async def get_songs_by_genre(
        self, genre: str, *, limit: int = 50, offset: int = 0,
        user: "UserRecord | None" = None,
    ) -> list[ViewTrack]:
        rows = await self._db.get_files_by_genre(genre, limit=limit, offset=offset)
        return await self.tracks_from_rows(rows, user=user)

    async def get_tracks_by_artist_mbids(
        self, mbids: list[str], *, user: "UserRecord | None" = None, limit: int = 500
    ) -> list[ViewTrack]:
        """Tracks where the track OR album artist matches (Jellyfin ArtistIds union)."""
        rows = await self._db.get_files_by_artist_mbids(mbids, limit=limit)
        return await self.tracks_from_rows(rows, user=user)

    async def get_tracks_by_album_artist_mbids(
        self, mbids: list[str], *, user: "UserRecord | None" = None, limit: int = 500
    ) -> list[ViewTrack]:
        """Tracks where the ALBUM artist matches (Jellyfin AlbumArtistIds strict)."""
        rows = await self._db.get_files_by_album_artist_mbids(mbids, limit=limit)
        return await self.tracks_from_rows(rows, user=user)

    async def tracks_from_rows(
        self, rows: list[dict], *, user: "UserRecord | None" = None
    ) -> list[ViewTrack]:
        """Build complete ViewTracks from raw library_files dicts + favorites overlay."""
        tracks = [self._track_from_row(r) for r in rows]
        await self._overlay_favorites(tracks, "track", lambda t: t.file_id, user)
        return tracks

    @staticmethod
    def _artist_from_summary(s: "LibraryArtistSummary") -> ViewArtist:
        return ViewArtist(
            artist_mbid=s.artist_mbid or "",
            name=s.artist_name,
            album_count=s.album_count,
            date_added=int(s.date_added) if s.date_added is not None else None,
        )

    @staticmethod
    def _album_from_summary(s: "LibraryAlbumSummary") -> ViewAlbum:
        return ViewAlbum(
            rg_mbid=s.release_group_mbid,
            title=s.album_title,
            artist_name=s.album_artist_name,
            year=s.year,
            track_count=s.track_count,
            cover_available=bool(s.cover_url),
            date_added=int(s.last_imported_at) if s.last_imported_at is not None else None,
            is_compilation=s.is_compilation,
        )

    async def _album_from_rows(self, rg_mbid: str, rows: list[dict]) -> ViewAlbum:
        first = rows[0]
        total_duration = sum(float(r.get("duration_seconds") or 0.0) for r in rows)
        date_added = max((r.get("imported_at") or 0) for r in rows)
        etag = await self._cover.get_release_group_cover_etag(rg_mbid)
        return ViewAlbum(
            rg_mbid=rg_mbid,
            title=first.get("album_title") or "",
            artist_name=first.get("album_artist_name"),
            artist_mbid=first.get("album_artist_mbid"),
            year=first.get("year"),
            genre=_dominant_genre(rows),
            track_count=len(rows),
            total_duration_seconds=total_duration,
            cover_available=etag is not None,
            date_added=int(date_added) if date_added else None,
            is_compilation=bool(first.get("is_compilation")),
        )

    @staticmethod
    def _track_from_row(row: dict) -> ViewTrack:
        return ViewTrack(
            file_id=row["id"],
            title=row.get("track_title") or "",
            album_title=row.get("album_title") or "",
            rg_mbid=row.get("release_group_mbid"),
            artist_name=row.get("artist_name") or "",
            artist_mbid=row.get("artist_mbid"),
            album_artist_name=row.get("album_artist_name"),
            album_artist_mbid=row.get("album_artist_mbid"),
            track_number=int(row.get("track_number") or 0),
            disc_number=int(row.get("disc_number") or 1),
            year=row.get("year"),
            genre=row.get("genre"),
            duration_seconds=float(row.get("duration_seconds") or 0.0),
            file_format=(row.get("file_format") or "").lower(),
            bitrate=row.get("bit_rate"),
            sample_rate=row.get("sample_rate"),
            bit_depth=row.get("bit_depth"),
            channels=row.get("channels"),
            file_size_bytes=int(row.get("file_size_bytes") or 0),
            recording_mbid=row.get("recording_mbid"),
            file_path=row.get("file_path") or "",
            created_at=row.get("imported_at"),
        )

    @staticmethod
    def _track_from_list_item(item: "LibraryTrackListItem") -> ViewTrack:
        # list item lacks full technical metadata; the shim's formatter applies
        # fallbacks (channelCount -> 2 etc). Single-track reads are complete.
        return ViewTrack(
            file_id=item.track_file_id,
            title=item.title,
            album_title=item.album_name,
            rg_mbid=item.album_mbid,
            artist_name=item.artist_name,
            track_number=item.track_number,
            disc_number=item.disc_number,
            duration_seconds=item.duration_seconds or 0.0,
            file_format=(item.format or "").lower(),
        )

    async def _overlay_favorites(self, items, kind, key, user) -> None:
        if user is None or not items:
            return
        ids = [key(i) for i in items if key(i)]
        if not ids:
            return
        mapping = await self._fav.map_for_items(user.id, kind, ids)
        for item in items:
            item.starred_at = mapping.get(key(item))
