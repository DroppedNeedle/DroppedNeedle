import asyncio
import logging

from api.v1.schemas.jellyfin import (
    JellyfinAlbumDetail,
    JellyfinAlbumMatch,
    JellyfinAlbumSummary,
    JellyfinArtistIndexEntry,
    JellyfinArtistIndexResponse,
    JellyfinArtistSummary,
    JellyfinFavoritesExpanded,
    JellyfinFilterFacets,
    JellyfinHubResponse,
    JellyfinImportResult,
    JellyfinLibraryStats,
    JellyfinLyricsLineSchema,
    JellyfinLyricsResponse,
    JellyfinPlaylistDetail,
    JellyfinPlaylistCollection,
    JellyfinPlaylistSummary,
    JellyfinPlaylistTrack,
    JellyfinSearchResponse,
    JellyfinSessionInfo,
    JellyfinSessionsResponse,
    JellyfinTrackInfo,
)
from infrastructure.cover_urls import prefer_artist_cover_url, prefer_release_group_cover_url
from core.exceptions import (
    ExternalServiceError,
    JellyfinAuthError,
    MediaAccountRelinkRequiredError,
    ResourceNotFoundError,
)
from repositories.protocols import JellyfinRepositoryProtocol
from repositories.jellyfin_models import JellyfinItem
from services.preferences_service import PreferencesService
from services.per_user_client_factory import MediaClientResolution, PerUserClientFactory

logger = logging.getLogger(__name__)


class JellyfinLibraryService:
    _DEFAULT_RECENTLY_PLAYED_TTL = 300
    _DEFAULT_FAVORITES_TTL = 300
    _DEFAULT_GENRES_TTL = 3600
    _DEFAULT_STATS_TTL = 600

    def __init__(
        self,
        jellyfin_repo: JellyfinRepositoryProtocol,
        preferences_service: PreferencesService,
        client_factory: PerUserClientFactory | None = None,
    ):
        self._jellyfin = jellyfin_repo
        self._preferences = preferences_service
        self._client_factory = client_factory

    def is_configured(self) -> bool:
        try:
            return bool(self._jellyfin.is_configured())
        except Exception:  # noqa: BLE001
            return True

    def _get_recently_played_ttl(self) -> int:
        try:
            return self._preferences.get_advanced_settings().cache_ttl_jellyfin_recently_played
        except Exception:  # noqa: BLE001
            return self._DEFAULT_RECENTLY_PLAYED_TTL

    def _get_favorites_ttl(self) -> int:
        try:
            return self._preferences.get_advanced_settings().cache_ttl_jellyfin_favorites
        except Exception:  # noqa: BLE001
            return self._DEFAULT_FAVORITES_TTL

    def _get_genres_ttl(self) -> int:
        try:
            return self._preferences.get_advanced_settings().cache_ttl_jellyfin_genres
        except Exception:  # noqa: BLE001
            return self._DEFAULT_GENRES_TTL

    def _get_stats_ttl(self) -> int:
        try:
            return self._preferences.get_advanced_settings().cache_ttl_jellyfin_library_stats
        except Exception:  # noqa: BLE001
            return self._DEFAULT_STATS_TTL

    def _item_to_album_summary(self, item: JellyfinItem) -> JellyfinAlbumSummary:
        pids = item.provider_ids or {}
        mbid = pids.get("MusicBrainzReleaseGroup") or pids.get("MusicBrainzAlbum")
        artist_mbid = pids.get("MusicBrainzAlbumArtist") or pids.get("MusicBrainzArtist")
        proxy_url = f"/api/v1/jellyfin/image/{item.id}" if item.id else None
        image_url = prefer_release_group_cover_url(
            mbid,
            proxy_url,
            size=500,
        )
        return JellyfinAlbumSummary(
            jellyfin_id=item.id,
            name=item.name,
            artist_name=item.artist_name or "",
            year=item.year,
            track_count=item.child_count or 0,
            image_url=image_url,
            musicbrainz_id=mbid,
            artist_musicbrainz_id=artist_mbid,
            play_count=item.play_count,
        )

    def _item_to_artist_summary(self, item: JellyfinItem) -> JellyfinArtistSummary:
        mbid = item.provider_ids.get("MusicBrainzArtist") if item.provider_ids else None
        proxy_url = f"/api/v1/jellyfin/image/{item.id}" if item.id else None
        image_url = prefer_artist_cover_url(
            mbid,
            proxy_url,
            size=500,
        )
        return JellyfinArtistSummary(
            jellyfin_id=item.id,
            name=item.name,
            image_url=image_url,
            album_count=item.album_count or 0,
            musicbrainz_id=mbid,
            play_count=item.play_count,
        )

    def _item_to_track_info(self, item: JellyfinItem) -> JellyfinTrackInfo:
        duration_seconds = (item.duration_ticks / 10_000_000.0) if item.duration_ticks else 0.0
        album_id = item.album_id or ""
        return JellyfinTrackInfo(
            jellyfin_id=item.id,
            title=item.name,
            track_number=item.index_number or 0,
            disc_number=item.parent_index_number or 1,
            duration_seconds=duration_seconds,
            album_name=item.album_name or "",
            artist_name=item.artist_name or "",
            album_id=album_id,
            codec=item.codec,
            bitrate=item.bitrate,
            image_url=f"/api/v1/jellyfin/image/{album_id}" if album_id else None,
        )

    @staticmethod
    def _fix_missing_track_numbers(tracks: list[JellyfinTrackInfo]) -> list[JellyfinTrackInfo]:
        """When all tracks share the same track_number (e.g. Jellyfin returns 0
        for every track), assign 1-based indices so downstream Map lookups work."""
        if len(tracks) <= 1:
            return tracks
        tracks_by_disc: dict[int, list[JellyfinTrackInfo]] = {}
        for track in tracks:
            tracks_by_disc.setdefault(track.disc_number, []).append(track)

        renumbered_ids: dict[str, int] = {}
        for disc_tracks in tracks_by_disc.values():
            numbers = {t.track_number for t in disc_tracks}
            if len(numbers) > 1:
                continue
            for i, track in enumerate(disc_tracks, start=1):
                renumbered_ids[track.jellyfin_id] = i

        fixed: list[JellyfinTrackInfo] = []
        for track in tracks:
            track_number = renumbered_ids.get(track.jellyfin_id, track.track_number)
            fixed.append(JellyfinTrackInfo(
                jellyfin_id=track.jellyfin_id,
                title=track.title,
                track_number=track_number,
                disc_number=track.disc_number,
                duration_seconds=track.duration_seconds,
                album_name=track.album_name,
                artist_name=track.artist_name,
                codec=track.codec,
                bitrate=track.bitrate,
            ))
        return fixed

    async def get_albums(
        self,
        limit: int = 50,
        offset: int = 0,
        sort_by: str = "SortName",
        sort_order: str = "Ascending",
        genre: str | None = None,
        year: int | None = None,
        tags: str | None = None,
        studios: str | None = None,
    ) -> tuple[list[JellyfinAlbumSummary], int]:
        items, total = await self._jellyfin.get_albums(
            limit=limit, offset=offset, sort_by=sort_by, sort_order=sort_order,
            genre=genre, year=year, tags=tags, studios=studios,
        )
        return [self._item_to_album_summary(i) for i in items], total

    async def get_album_detail(self, album_id: str) -> JellyfinAlbumDetail | None:
        item = await self._jellyfin.get_album_detail(album_id)
        if not item:
            return None

        tracks_items = await self._jellyfin.get_album_tracks(album_id)
        tracks = self._fix_missing_track_numbers(
            [self._item_to_track_info(t) for t in tracks_items]
        )
        pids = item.provider_ids or {}
        mbid = pids.get("MusicBrainzReleaseGroup") or pids.get("MusicBrainzAlbum")
        artist_mbid = pids.get("MusicBrainzAlbumArtist") or pids.get("MusicBrainzArtist")
        proxy_url = f"/api/v1/jellyfin/image/{item.id}" if item.id else None
        image_url = prefer_release_group_cover_url(
            mbid,
            proxy_url,
            size=500,
        )

        return JellyfinAlbumDetail(
            jellyfin_id=item.id,
            name=item.name,
            artist_name=item.artist_name or "",
            year=item.year,
            track_count=len(tracks),
            image_url=image_url,
            musicbrainz_id=mbid,
            artist_musicbrainz_id=artist_mbid,
            tracks=tracks,
        )

    async def get_album_tracks(self, album_id: str) -> list[JellyfinTrackInfo]:
        items = await self._jellyfin.get_album_tracks(album_id)
        return self._fix_missing_track_numbers(
            [self._item_to_track_info(i) for i in items]
        )

    async def match_album_by_mbid(self, musicbrainz_id: str) -> JellyfinAlbumMatch:
        item = await self._jellyfin.get_album_by_mbid(musicbrainz_id)
        if not item:
            return JellyfinAlbumMatch(found=False)

        tracks_items = await self._jellyfin.get_album_tracks(item.id)
        tracks = self._fix_missing_track_numbers(
            [self._item_to_track_info(t) for t in tracks_items]
        )

        return JellyfinAlbumMatch(
            found=True,
            jellyfin_album_id=item.id,
            tracks=tracks,
        )

    async def resolve_album_mbid(self, album_id: str) -> str | None:
        """Map a Jellyfin album GUID to its MusicBrainz release-group MBID.

        Reads ProviderIds.MusicBrainzReleaseGroup, falling back to
        MusicBrainzAlbum - the same precedence JellyfinAlbumDetail uses.
        Returns None when the album or its provider ids are unavailable
        (get_album_detail degrades to None on upstream failure).
        """
        item = await self._jellyfin.get_album_detail(album_id)
        if not item:
            return None
        pids = item.provider_ids or {}
        return pids.get("MusicBrainzReleaseGroup") or pids.get("MusicBrainzAlbum")

    async def get_artists(
        self, limit: int = 50, offset: int = 0
    ) -> list[JellyfinArtistSummary]:
        items, _total = await self._jellyfin.get_artists(limit=limit, offset=offset)
        return [self._item_to_artist_summary(i) for i in items]

    async def browse_artists(
        self,
        limit: int = 48,
        offset: int = 0,
        sort_by: str = "SortName",
        sort_order: str = "Ascending",
        search: str = "",
    ) -> tuple[list[JellyfinArtistSummary], int]:
        items, total = await self._jellyfin.get_artists(
            limit=limit, offset=offset, sort_by=sort_by, sort_order=sort_order, search=search
        )
        return [self._item_to_artist_summary(i) for i in items], total

    async def get_artists_index(self) -> JellyfinArtistIndexResponse:
        all_artists: list[JellyfinArtistSummary] = []
        offset = 0
        batch_size = 500
        while True:
            items, _total = await self._jellyfin.get_artists(
                limit=batch_size, offset=offset, sort_by="SortName", sort_order="Ascending"
            )
            all_artists.extend(self._item_to_artist_summary(i) for i in items)
            if len(items) < batch_size:
                break
            offset += batch_size

        groups: dict[str, list[JellyfinArtistSummary]] = {}
        for artist in all_artists:
            letter = artist.name[0].upper() if artist.name else "#"
            if not letter.isalpha():
                letter = "#"
            groups.setdefault(letter, []).append(artist)

        entries = [
            JellyfinArtistIndexEntry(name=letter, artists=artists)
            for letter, artists in sorted(groups.items())
        ]
        return JellyfinArtistIndexResponse(index=entries)

    async def browse_tracks(
        self,
        limit: int = 48,
        offset: int = 0,
        sort_by: str = "SortName",
        sort_order: str = "Ascending",
        search: str = "",
    ) -> tuple[list[JellyfinTrackInfo], int]:
        items, total = await self._jellyfin.get_tracks(
            limit=limit, offset=offset, sort_by=sort_by, sort_order=sort_order, search=search
        )
        return [self._item_to_track_info(i) for i in items], total

    async def search(
        self, query: str
    ) -> JellyfinSearchResponse:
        items = await self._jellyfin.search_items(query)
        albums = []
        artists = []
        tracks = []
        for item in items:
            if item.type == "MusicAlbum":
                albums.append(self._item_to_album_summary(item))
            elif item.type in ("MusicArtist", "Artist"):
                artists.append(self._item_to_artist_summary(item))
            elif item.type == "Audio":
                tracks.append(self._item_to_track_info(item))
        return JellyfinSearchResponse(albums=albums, artists=artists, tracks=tracks)

    async def get_recently_played(self, limit: int = 20) -> list[JellyfinAlbumSummary]:
        ttl_seconds = self._get_recently_played_ttl()
        items = await self._jellyfin.get_recently_played(
            limit=limit,
            ttl_seconds=ttl_seconds,
        )
        seen_album_ids: set[str] = set()
        unique_album_ids: list[str] = []
        for item in items:
            aid = item.album_id or item.parent_id
            if not aid or aid in seen_album_ids:
                continue
            seen_album_ids.add(aid)
            unique_album_ids.append(aid)
            if len(unique_album_ids) >= limit:
                break

        _CONCURRENCY_LIMIT = 5
        sem = asyncio.Semaphore(_CONCURRENCY_LIMIT)

        async def _fetch(aid: str) -> JellyfinItem | None:
            async with sem:
                return await self._jellyfin.get_album_detail(aid)

        details = await asyncio.gather(
            *(_fetch(aid) for aid in unique_album_ids)
        )
        return [
            self._item_to_album_summary(detail)
            for detail in details
            if detail is not None
        ]

    async def get_favorites(self, limit: int = 20) -> list[JellyfinAlbumSummary]:
        ttl_seconds = self._get_favorites_ttl()
        items = await self._jellyfin.get_favorite_albums(
            limit=limit,
            ttl_seconds=ttl_seconds,
        )
        return [self._item_to_album_summary(i) for i in items]

    async def get_favorites_expanded(self, limit: int = 50) -> JellyfinFavoritesExpanded:
        ttl_seconds = self._get_favorites_ttl()
        albums_items, artists_items = await asyncio.gather(
            self._jellyfin.get_favorite_albums(limit=limit, ttl_seconds=ttl_seconds),
            self._jellyfin.get_favorite_artists(limit=limit),
        )
        return JellyfinFavoritesExpanded(
            albums=[self._item_to_album_summary(i) for i in albums_items],
            artists=[self._item_to_artist_summary(i) for i in artists_items],
        )

    async def get_filter_facets(self) -> JellyfinFilterFacets:
        facets = await self._jellyfin.get_filter_facets()
        return JellyfinFilterFacets(
            years=facets.get("years", []),
            tags=facets.get("tags", []),
            studios=facets.get("studios", []),
        )

    async def get_genres(self) -> list[str]:
        ttl_seconds = self._get_genres_ttl()
        return await self._jellyfin.get_genres(ttl_seconds=ttl_seconds)

    async def get_songs_by_genre(
        self, genre: str, limit: int = 50, offset: int = 0
    ) -> tuple[list[JellyfinTrackInfo], int]:
        items, total = await self._jellyfin.get_tracks(
            limit=limit, offset=offset, sort_by="Random", genre=genre
        )
        return [self._item_to_track_info(i) for i in items], total

    async def get_stats(self) -> JellyfinLibraryStats:
        ttl_seconds = self._get_stats_ttl()
        raw = await self._jellyfin.get_library_stats(ttl_seconds=ttl_seconds)
        return JellyfinLibraryStats(
            total_tracks=raw.get("total_tracks", 0),
            total_albums=raw.get("total_albums", 0),
            total_artists=raw.get("total_artists", 0),
        )

    async def get_recently_added(self, limit: int = 20) -> list[JellyfinAlbumSummary]:
        items = await self._jellyfin.get_recently_added(limit=limit)
        return [self._item_to_album_summary(i) for i in items]

    async def get_most_played_artists(self, limit: int = 20) -> list[JellyfinArtistSummary]:
        items = await self._jellyfin.get_most_played_artists(limit=limit)
        return [self._item_to_artist_summary(i) for i in items]

    async def get_most_played_albums(self, limit: int = 20) -> list[JellyfinAlbumSummary]:
        items = await self._jellyfin.get_most_played_albums(limit=limit)
        return [self._item_to_album_summary(i) for i in items]

    async def _playlist_resolution(
        self, user_id: str
    ) -> MediaClientResolution[JellyfinRepositoryProtocol]:
        if self._client_factory is not None:
            linked = await self._client_factory.resolve_jellyfin_playlist(user_id)
            if linked is not None:
                return linked
        return MediaClientResolution(
            repository=self._jellyfin,
            account_mode="shared",
            account_label="Shared Jellyfin account",
            cache_scope="shared",
        )

    async def _list_playlists(
        self, repo: JellyfinRepositoryProtocol, limit: int
    ) -> list[JellyfinPlaylistSummary]:
        items = await repo.get_playlists(limit=limit)
        summaries = []
        for i in items:
            cover = f"/api/v1/jellyfin/playlist-image/{i.id}/{i.id}"
            summaries.append(JellyfinPlaylistSummary(
                id=i.id,
                name=i.name,
                track_count=i.child_count or 0,
                duration_seconds=int(i.duration_ticks / 10_000_000) if i.duration_ticks else 0,
                cover_url=cover,
                created_at=i.date_created or "",
            ))
        return summaries

    async def list_playlists(self, limit: int = 50) -> list[JellyfinPlaylistSummary]:
        return await self._list_playlists(self._jellyfin, limit)

    async def list_user_playlists(
        self,
        requesting: 'UserRecord',
        playlist_service: 'PlaylistService',
        limit: int = 50,
    ) -> JellyfinPlaylistCollection:
        resolution = await self._playlist_resolution(requesting.id)
        try:
            playlists = await self._list_playlists(resolution.repository, limit)
        except JellyfinAuthError as exc:
            if resolution.account_mode == "linked":
                raise MediaAccountRelinkRequiredError(
                    "Reconnect Jellyfin to check your playlists"
                ) from exc
            raise
        imported_ids = await playlist_service.get_imported_source_ids(
            "jellyfin:", user_id=requesting.id
        )
        for playlist in playlists:
            playlist.is_imported = playlist.id in imported_ids
        return JellyfinPlaylistCollection(
            account_mode=resolution.account_mode,
            account_label=resolution.account_label,
            playlists=playlists,
        )

    async def _get_playlist_detail(
        self, repo: JellyfinRepositoryProtocol, playlist_id: str
    ) -> JellyfinPlaylistDetail:
        visible = await repo.get_playlists(limit=10_000)
        if not any(item.id == playlist_id for item in visible):
            raise ResourceNotFoundError("Jellyfin playlist not found")
        playlist = await repo.get_playlist(playlist_id)
        if playlist is None:
            raise ResourceNotFoundError("Jellyfin playlist not found")

        items = await repo.get_playlist_items(playlist_id)
        tracks = []
        for t in items:
            tracks.append(JellyfinPlaylistTrack(
                id=t.id,
                track_name=t.name,
                artist_name=t.artist_name or "",
                album_name=t.album_name or "",
                album_id=t.album_id or "",
                artist_id=t.artist_id or "",
                duration_seconds=int(t.duration_ticks / 10_000_000) if t.duration_ticks else 0,
                track_number=t.index_number or 0,
                disc_number=t.parent_index_number or 1,
                cover_url=(
                    f"/api/v1/jellyfin/playlist-image/{playlist_id}/{t.album_id}"
                    if t.album_id
                    else ""
                ),
            ))

        cover = f"/api/v1/jellyfin/playlist-image/{playlist.id}/{playlist.id}"
        if not playlist.image_tag and tracks:
            first_with_album = next((t for t in tracks if t.album_id), None)
            if first_with_album:
                cover = (
                    f"/api/v1/jellyfin/playlist-image/{playlist.id}/"
                    f"{first_with_album.album_id}"
                )
        return JellyfinPlaylistDetail(
            id=playlist.id,
            name=playlist.name,
            track_count=playlist.child_count or 0,
            duration_seconds=int(playlist.duration_ticks / 10_000_000) if playlist.duration_ticks else 0,
            cover_url=cover,
            created_at=playlist.date_created or "",
            tracks=tracks,
        )

    async def get_playlist_detail(self, playlist_id: str) -> JellyfinPlaylistDetail:
        return await self._get_playlist_detail(self._jellyfin, playlist_id)

    async def get_user_playlist_detail(
        self, playlist_id: str, requesting: 'UserRecord'
    ) -> JellyfinPlaylistDetail:
        resolution = await self._playlist_resolution(requesting.id)
        try:
            return await self._get_playlist_detail(resolution.repository, playlist_id)
        except JellyfinAuthError as exc:
            if resolution.account_mode == "linked":
                raise MediaAccountRelinkRequiredError(
                    "Reconnect Jellyfin to check your playlists"
                ) from exc
            raise

    async def get_playlist_image(
        self,
        playlist_id: str,
        item_id: str,
        requesting: 'UserRecord',
        size: int,
    ) -> tuple[bytes, str]:
        resolution = await self._playlist_resolution(requesting.id)
        repo = resolution.repository
        try:
            visible = await repo.get_playlists(limit=10_000)
            if not any(item.id == playlist_id for item in visible):
                raise ResourceNotFoundError("Jellyfin playlist image not found")
            items = await repo.get_playlist_items(playlist_id)
            allowed = {playlist_id}
            for item in items:
                allowed.add(item.id)
                if item.album_id:
                    allowed.add(item.album_id)
            if item_id not in allowed:
                raise ResourceNotFoundError("Jellyfin playlist image not found")
            return await repo.proxy_image(item_id, size)
        except JellyfinAuthError as exc:
            if resolution.account_mode == "linked":
                raise MediaAccountRelinkRequiredError(
                    "Reconnect Jellyfin to check your playlists"
                ) from exc
            raise

    async def import_playlist(
        self,
        playlist_id: str,
        playlist_service: 'PlaylistService',
        requesting: 'UserRecord',
    ) -> JellyfinImportResult:
        source_ref = f"jellyfin:{playlist_id}"
        existing = await playlist_service.get_by_source_ref(source_ref, user_id=requesting.id)
        if existing:
            return JellyfinImportResult(
                droppedneedle_playlist_id=existing.id,
                already_imported=True,
            )

        detail = await self.get_user_playlist_detail(playlist_id, requesting)
        try:
            created = await playlist_service.create_playlist(
                detail.name, source_ref=source_ref, user_id=requesting.id,
            )
        except Exception:  # noqa: BLE001
            re_check = await playlist_service.get_by_source_ref(source_ref, user_id=requesting.id)
            if re_check:
                return JellyfinImportResult(droppedneedle_playlist_id=re_check.id, already_imported=True)
            raise

        # Map each distinct Jellyfin album GUID to its MusicBrainz MBID so the
        # stored album_id can match the MBID-keyed local catalog (#150). One
        # deduped fetch per album; unresolvable GUIDs store None (absence) so
        # the frontend renders plain text instead of a dead /album/<guid> link.
        album_mbids: dict[str, str] = {}
        distinct_album_ids = sorted({t.album_id for t in detail.tracks if t.album_id})
        if distinct_album_ids:
            resolved = await asyncio.gather(
                *(self.resolve_album_mbid(guid) for guid in distinct_album_ids)
            )
            album_mbids = {
                guid: mbid
                for guid, mbid in zip(distinct_album_ids, resolved)
                if mbid
            }

        track_dicts = []
        failed = 0
        for t in detail.tracks:
            try:
                track_dicts.append({
                    "track_name": t.track_name,
                    "artist_name": t.artist_name,
                    "album_name": t.album_name,
                    "duration": t.duration_seconds,
                    "track_source_id": t.id,
                    "source_type": "jellyfin",
                    "album_id": album_mbids.get(t.album_id) or None,
                    "artist_id": t.artist_id,
                    "track_number": t.track_number,
                    "disc_number": t.disc_number,
                    "cover_url": t.cover_url,
                })
            except Exception:  # noqa: BLE001
                failed += 1

        if track_dicts:
            try:
                await playlist_service.add_tracks(created.id, requesting, track_dicts)
            except Exception:  # noqa: BLE001
                logger.error("Failed to add tracks during Jellyfin playlist import %s", playlist_id, exc_info=True)
                await playlist_service.delete_playlist(created.id, requesting)
                raise ExternalServiceError(f"Failed to import Jellyfin playlist {playlist_id}")

        return JellyfinImportResult(
            droppedneedle_playlist_id=created.id,
            tracks_imported=len(track_dicts),
            tracks_failed=failed,
        )

    async def get_instant_mix(
        self, item_id: str, limit: int = 50
    ) -> list[JellyfinTrackInfo]:
        try:
            items = await self._jellyfin.get_instant_mix(item_id, limit=limit)
            return [self._item_to_track_info(i) for i in items]
        except Exception:  # noqa: BLE001
            logger.warning("get_instant_mix failed for %s", item_id, exc_info=True)
            return []

    async def get_instant_mix_by_artist(
        self, artist_id: str, limit: int = 50
    ) -> list[JellyfinTrackInfo]:
        try:
            items = await self._jellyfin.get_instant_mix_by_artist(artist_id, limit=limit)
            return [self._item_to_track_info(i) for i in items]
        except Exception:  # noqa: BLE001
            logger.warning("get_instant_mix_by_artist failed for %s", artist_id, exc_info=True)
            return []

    async def get_instant_mix_by_genre(
        self, genre_name: str, limit: int = 50
    ) -> list[JellyfinTrackInfo]:
        try:
            items = await self._jellyfin.get_instant_mix_by_genre(genre_name, limit=limit)
            return [self._item_to_track_info(i) for i in items]
        except Exception:  # noqa: BLE001
            logger.warning("get_instant_mix_by_genre failed for %s", genre_name, exc_info=True)
            return []

    async def get_sessions(self) -> JellyfinSessionsResponse:
        _TICKS_TO_SECONDS = 10_000_000
        try:
            raw_sessions = await self._jellyfin.get_sessions()
            sessions = [
                JellyfinSessionInfo(
                    session_id=s.session_id,
                    user_name=s.user_name,
                    device_name=s.device_name,
                    client_name=s.client_name,
                    track_name=s.now_playing_name,
                    artist_name=s.now_playing_artist,
                    album_name=s.now_playing_album,
                    album_id=s.now_playing_album_id,
                    cover_url=f"/api/v1/jellyfin/image/{s.now_playing_item_id}" if s.now_playing_item_id else "",
                    position_seconds=s.position_ticks / _TICKS_TO_SECONDS if s.position_ticks else 0.0,
                    duration_seconds=s.runtime_ticks / _TICKS_TO_SECONDS if s.runtime_ticks else 0.0,
                    is_paused=s.is_paused,
                    play_method=s.play_method,
                    audio_codec=s.audio_codec,
                    bitrate=s.bitrate,
                )
                for s in raw_sessions
            ]
            return JellyfinSessionsResponse(sessions=sessions)
        except Exception:  # noqa: BLE001
            logger.warning("get_sessions failed", exc_info=True)
            return JellyfinSessionsResponse(sessions=[])

    async def get_hub_data(self) -> JellyfinHubResponse:
        _HUB_TIMEOUT = 10

        results = await asyncio.gather(
            asyncio.wait_for(self.get_recently_played(limit=20), timeout=_HUB_TIMEOUT),
            asyncio.wait_for(self.get_favorites(limit=20), timeout=_HUB_TIMEOUT),
            asyncio.wait_for(self.get_albums(limit=12), timeout=_HUB_TIMEOUT),
            asyncio.wait_for(self.get_stats(), timeout=_HUB_TIMEOUT),
            asyncio.wait_for(self.get_recently_added(limit=20), timeout=_HUB_TIMEOUT),
            asyncio.wait_for(self.get_most_played_artists(limit=10), timeout=_HUB_TIMEOUT),
            asyncio.wait_for(self.get_most_played_albums(limit=10), timeout=_HUB_TIMEOUT),
            asyncio.wait_for(self.get_genres(), timeout=_HUB_TIMEOUT),
            return_exceptions=True,
        )

        all_failed = all(isinstance(r, BaseException) for r in results)
        if all_failed:
            raise ExternalServiceError("All Jellyfin hub data requests failed")

        recently_played = results[0] if not isinstance(results[0], BaseException) else []
        if isinstance(results[0], BaseException):
            logger.warning("Hub: get_recently_played failed: %s", results[0])

        favorites = results[1] if not isinstance(results[1], BaseException) else []
        if isinstance(results[1], BaseException):
            logger.warning("Hub: get_favorites failed: %s", results[1])

        albums_result = results[2]
        if isinstance(albums_result, BaseException):
            logger.warning("Hub: get_albums failed: %s", albums_result)
            all_albums_preview: list[JellyfinAlbumSummary] = []
        else:
            all_albums_preview = albums_result[0]

        stats = results[3] if not isinstance(results[3], BaseException) else None
        if isinstance(results[3], BaseException):
            logger.warning("Hub: get_stats failed: %s", results[3])

        recently_added = results[4] if not isinstance(results[4], BaseException) else []
        if isinstance(results[4], BaseException):
            logger.warning("Hub: get_recently_added failed: %s", results[4])

        most_played_artists = results[5] if not isinstance(results[5], BaseException) else []
        if isinstance(results[5], BaseException):
            logger.warning("Hub: get_most_played_artists failed: %s", results[5])

        most_played_albums = results[6] if not isinstance(results[6], BaseException) else []
        if isinstance(results[6], BaseException):
            logger.warning("Hub: get_most_played_albums failed: %s", results[6])

        genres = results[7] if not isinstance(results[7], BaseException) else []
        if isinstance(results[7], BaseException):
            logger.warning("Hub: get_genres failed: %s", results[7])

        return JellyfinHubResponse(
            stats=stats,
            recently_played=recently_played,
            recently_added=recently_added,
            favorites=favorites,
            most_played_artists=most_played_artists,
            most_played_albums=most_played_albums,
            all_albums_preview=all_albums_preview,
            genres=genres,
        )

    async def get_similar_items(
        self, item_id: str, limit: int = 10
    ) -> list[JellyfinAlbumSummary]:
        try:
            items = await self._jellyfin.get_similar_items(item_id, limit=limit)
            return [self._item_to_album_summary(i) for i in items if i.type in ("MusicAlbum", "Audio")]
        except Exception:  # noqa: BLE001
            logger.warning("Similar items unavailable for %s", item_id)
            return []

    async def get_lyrics(self, item_id: str) -> JellyfinLyricsResponse | None:
        try:
            lyrics = await self._jellyfin.get_lyrics(item_id)
            if lyrics is None:
                return None
            is_synced = any(line.start is not None for line in lyrics.lines)
            lines = [
                JellyfinLyricsLineSchema(
                    text=line.text,
                    start_seconds=line.start / 10_000_000 if line.start is not None else None,
                )
                for line in lyrics.lines
            ]
            lyrics_text = "\n".join(line.text for line in lyrics.lines)
            return JellyfinLyricsResponse(
                lines=lines,
                is_synced=is_synced,
                lyrics_text=lyrics_text,
            )
        except ExternalServiceError:
            logger.warning("Jellyfin API error fetching lyrics for item %s", item_id)
            return None
        except Exception:  # noqa: BLE001
            logger.warning("Unexpected error fetching lyrics for item %s", item_id, exc_info=True)
            return None
