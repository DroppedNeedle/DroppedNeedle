from __future__ import annotations

import asyncio
import logging
from typing import Any, TYPE_CHECKING

from api.v1.schemas.home import (
    HomeArtist,
    HomeAlbum,
    GenreDetailResponse,
    GenreLibrarySection,
    GenrePopularSection,
    TrendingArtistsResponse,
    TrendingTimeRange,
    TrendingArtistsRangeResponse,
    PopularAlbumsResponse,
    PopularTimeRange,
    PopularAlbumsRangeResponse,
)
from repositories.protocols import (
    ListenBrainzRepositoryProtocol,
    LibraryRepositoryProtocol,
    MusicBrainzRepositoryProtocol,
    LastFmRepositoryProtocol,
)
from services.home_transformers import HomeDataTransformers
from services.preferences_service import PreferencesService
from services.per_user_client_factory import PerUserClientFactory
from infrastructure.persistence import GenreIndex
from infrastructure.persistence.user_listening_prefs_store import UserListeningPrefsStore

from .integration_helpers import HomeIntegrationHelpers, resolve_source_value

if TYPE_CHECKING:
    from services.genre_cover_prewarm_service import GenreCoverPrewarmService

logger = logging.getLogger(__name__)


class HomeChartsService:
    def __init__(
        self,
        listenbrainz_repo: ListenBrainzRepositoryProtocol,
        library_repo: LibraryRepositoryProtocol,
        musicbrainz_repo: MusicBrainzRepositoryProtocol,
        genre_index: GenreIndex | None = None,
        lastfm_repo: LastFmRepositoryProtocol | None = None,
        preferences_service: PreferencesService | None = None,
        prewarm_service: 'GenreCoverPrewarmService | None' = None,
        client_factory: PerUserClientFactory | None = None,
        listening_prefs_store: UserListeningPrefsStore | None = None,
    ):
        self._lb_repo = listenbrainz_repo
        self._library_repo = library_repo
        self._mb_repo = musicbrainz_repo
        self._genre_index = genre_index
        self._lfm_repo = lastfm_repo
        self._preferences = preferences_service
        self._prewarm_service = prewarm_service
        self._client_factory = client_factory
        self._prefs_store = listening_prefs_store
        self._transformers = HomeDataTransformers()

        self._helpers: HomeIntegrationHelpers | None = None
        if preferences_service:
            self._helpers = HomeIntegrationHelpers(preferences_service)

    def _resolve_source(self, source: str | None) -> str:
        if self._helpers:
            return self._helpers.resolve_source(source)
        if source in ("listenbrainz", "lastfm"):
            return source
        return "listenbrainz"

    async def _execute_tasks(self, tasks: dict[str, Any]) -> dict[str, Any]:
        if self._helpers:
            return await self._helpers.execute_tasks(tasks)
        if not tasks:
            return {}
        keys = list(tasks.keys())
        coros = list(tasks.values())
        raw_results = await asyncio.gather(*coros, return_exceptions=True)
        results = {}
        for key, result in zip(keys, raw_results):
            if isinstance(result, Exception):
                logger.warning(f"Task {key} failed: {result}")
                results[key] = None
            else:
                results[key] = result
        return results

    def _get_lastfm_username(self) -> str | None:
        if self._helpers:
            return self._helpers.get_lastfm_username()
        if not self._preferences:
            return None
        lf_settings = self._preferences.get_lastfm_connection()
        if lf_settings.enabled and lf_settings.username:
            return lf_settings.username
        return None

    def _get_lb_username(self) -> str | None:
        if self._helpers:
            return self._helpers.get_lb_username()
        if not self._preferences:
            return None
        lb_settings = self._preferences.get_listenbrainz_connection()
        if lb_settings.enabled and lb_settings.username:
            return lb_settings.username
        return None

    async def _resolve_user_top(self, user_id: str, source: str | None):
        lb_client = lfm_client = None
        lb_username = lfm_username = None
        if self._client_factory:
            lb_client = await self._client_factory.resolve_listenbrainz(user_id)
            lfm_client = await self._client_factory.resolve_lastfm(user_id)
            lb_username = await self._client_factory.resolve_listenbrainz_username(user_id)
            lfm_username = await self._client_factory.resolve_lastfm_username(user_id)
        primary_source = "listenbrainz"
        if self._prefs_store:
            prefs = await self._prefs_store.get(user_id)
            primary_source = prefs.primary_music_source
        resolved = resolve_source_value(
            source, primary_source, lb_client is not None, lfm_client is not None
        )
        return lb_client, lfm_client, lb_username, lfm_username, resolved

    async def get_genre_artists(
        self, genre: str, limit: int = 100, artist_offset: int = 0, album_offset: int = 0
    ) -> GenreDetailResponse:
        library_results = await asyncio.gather(
            self._library_repo.get_artists_from_library(include_unmonitored=True),
            self._native_album_mbids(),
            return_exceptions=True,
        )
        library_failed = any(isinstance(r, BaseException) for r in library_results[:2])
        if library_failed:
            logger.warning("Lidarr unavailable for genre '%s', proceeding with MusicBrainz data only", genre)
        library_artists = library_results[0] if not isinstance(library_results[0], BaseException) else []
        library_mbids = {a.get("mbid", "").lower() for a in library_artists if a.get("mbid")}
        library_album_mbids = library_results[1] if isinstance(library_results[1], set) else set()
        library_section = None
        if self._genre_index:
            lib_artists_data = await self._genre_index.get_artists_by_genre(genre, limit=50)
            lib_albums_data = await self._genre_index.get_albums_by_genre(genre, limit=50)
            lib_artists = [
                HomeArtist(
                    mbid=a.get("mbid"),
                    name=a.get("name", "Unknown"),
                    image_url=None,
                    listen_count=a.get("album_count"),
                    in_library=True,
                )
                for a in lib_artists_data
            ]
            lib_albums = [
                HomeAlbum(
                    mbid=a.get("mbid"),
                    name=a.get("title", "Unknown"),
                    artist_name=a.get("artist_name"),
                    artist_mbid=a.get("artist_mbid"),
                    image_url=a.get("cover_url"),
                    release_date=str(a.get("year")) if a.get("year") else None,
                    in_library=True,
                )
                for a in lib_albums_data
            ]
            library_section = GenreLibrarySection(
                artists=lib_artists,
                albums=lib_albums,
                artist_count=len(lib_artists_data),
                album_count=len(lib_albums_data),
            )
        mb_artist_results = await self._mb_repo.search_artists_by_tag(
            tag=genre, limit=limit, offset=artist_offset
        )
        mb_album_results = await self._mb_repo.search_release_groups_by_tag(
            tag=genre, limit=limit, offset=album_offset
        )
        popular_artists = [
            HomeArtist(
                mbid=result.musicbrainz_id,
                name=result.title,
                image_url=None,
                listen_count=None,
                in_library=result.musicbrainz_id.lower() in library_mbids,
            )
            for result in mb_artist_results
        ]
        popular_albums = [
            HomeAlbum(
                mbid=result.musicbrainz_id,
                name=result.title,
                artist_name=result.artist,
                artist_mbid=None,
                image_url=None,
                release_date=str(result.year) if result.year else None,
                in_library=result.musicbrainz_id.lower() in library_album_mbids,
            )
            for result in mb_album_results
        ]
        popular_section = GenrePopularSection(
            artists=popular_artists,
            albums=popular_albums,
            has_more_artists=len(mb_artist_results) >= limit,
            has_more_albums=len(mb_album_results) >= limit,
        )
        if self._prewarm_service:
            artist_mbids = [a.mbid for a in popular_artists if a.mbid]
            album_mbids = [a.mbid for a in popular_albums if a.mbid]
            self._prewarm_service.schedule_prewarm(genre, artist_mbids, album_mbids)
        return GenreDetailResponse(
            genre=genre,
            library=library_section,
            popular=popular_section,
            artists=popular_artists,
            total_count=len(popular_artists),
        )

    async def get_trending_artists(self, limit: int = 10, source: str | None = None) -> TrendingArtistsResponse:
        resolved = self._resolve_source(source)
        if resolved == "lastfm" and self._lfm_repo:
            return await self._get_trending_artists_lastfm(limit)

        library_artists = await self._library_repo.get_artists_from_library(include_unmonitored=True)
        library_mbids = {a.get("mbid", "").lower() for a in library_artists if a.get("mbid")}
        ranges = ["this_week", "this_month", "this_year", "all_time"]
        tasks = {r: self._lb_repo.get_sitewide_top_artists(range_=r, count=limit + 1) for r in ranges}
        results = await self._execute_tasks(tasks)
        response_data = {}
        for r in ranges:
            lb_artists = results.get(r) or []
            artists = [
                a for a in (self._transformers.lb_artist_to_home(artist, library_mbids) for artist in lb_artists)
                if a is not None
            ]
            featured = artists[0] if artists else None
            items = artists[1:limit] if len(artists) > 1 else []
            response_data[r] = TrendingTimeRange(
                range_key=r,
                label=HomeDataTransformers.get_range_label(r),
                featured=featured,
                items=items,
                total_count=len(artists),
            )
        return TrendingArtistsResponse(
            this_week=response_data["this_week"],
            this_month=response_data["this_month"],
            this_year=response_data["this_year"],
            all_time=response_data["all_time"],
        )

    async def get_trending_artists_by_range(
        self,
        range_key: str = "this_week",
        limit: int = 25,
        offset: int = 0,
        source: str | None = None,
    ) -> TrendingArtistsRangeResponse:
        allowed_ranges = ["this_week", "this_month", "this_year", "all_time"]
        if range_key not in allowed_ranges:
            range_key = "this_week"
        resolved = self._resolve_source(source)
        if resolved == "lastfm" and self._lfm_repo:
            return await self._get_trending_artists_lastfm_range(
                range_key=range_key,
                limit=limit,
                offset=offset,
            )
        library_artists, lb_artists = await asyncio.gather(
            self._library_repo.get_artists_from_library(include_unmonitored=True),
            self._lb_repo.get_sitewide_top_artists(
                range_=range_key, count=limit + 1, offset=offset
            ),
        )
        library_mbids = {a.get("mbid", "").lower() for a in library_artists if a.get("mbid")}
        artists = [
            a for a in (self._transformers.lb_artist_to_home(artist, library_mbids) for artist in lb_artists)
            if a is not None
        ]
        has_more = len(artists) > limit
        items = artists[:limit]
        return TrendingArtistsRangeResponse(
            range_key=range_key,
            label=HomeDataTransformers.get_range_label(range_key),
            items=items,
            offset=offset,
            limit=limit,
            has_more=has_more,
        )

    async def _native_album_mbids(self) -> set[str]:
        """Release-group MBIDs in the native library (lowercased) for in_library flags.

        get_library() is an empty stub on native installs, so album membership must be
        read from get_library_mbids() - the same authoritative set behind /library/mbids.
        Without this, chart albums already in the library show a stray download button.
        """
        try:
            mbids = await self._library_repo.get_library_mbids(include_release_ids=False)
        except Exception as exc:  # noqa: BLE001 - membership is best-effort
            logger.warning("native album mbid lookup failed: %s", exc)
            return set()
        return {m.lower() for m in mbids}

    async def get_popular_albums(self, limit: int = 10, source: str | None = None) -> PopularAlbumsResponse:
        resolved = self._resolve_source(source)
        if resolved == "lastfm" and self._lfm_repo:
            return await self._get_popular_albums_lastfm(limit)

        library_mbids = await self._native_album_mbids()
        ranges = ["this_week", "this_month", "this_year", "all_time"]
        tasks = {r: self._lb_repo.get_sitewide_top_release_groups(range_=r, count=limit + 1) for r in ranges}
        results = await self._execute_tasks(tasks)
        response_data = {}
        for r in ranges:
            lb_albums = results.get(r) or []
            albums = [self._transformers.lb_release_to_home(a, library_mbids) for a in lb_albums]
            featured = albums[0] if albums else None
            items = albums[1:limit] if len(albums) > 1 else []
            response_data[r] = PopularTimeRange(
                range_key=r,
                label=HomeDataTransformers.get_range_label(r),
                featured=featured,
                items=items,
                total_count=len(albums),
            )
        return PopularAlbumsResponse(
            this_week=response_data["this_week"],
            this_month=response_data["this_month"],
            this_year=response_data["this_year"],
            all_time=response_data["all_time"],
        )

    async def get_popular_albums_by_range(
        self,
        range_key: str = "this_week",
        limit: int = 25,
        offset: int = 0,
        source: str | None = None,
    ) -> PopularAlbumsRangeResponse:
        allowed_ranges = ["this_week", "this_month", "this_year", "all_time"]
        if range_key not in allowed_ranges:
            range_key = "this_week"
        resolved = self._resolve_source(source)
        if resolved == "lastfm" and self._lfm_repo:
            return await self._get_popular_albums_lastfm_range(
                range_key=range_key,
                limit=limit,
                offset=offset,
            )
        library_mbids_raw, lb_albums = await asyncio.gather(
            self._native_album_mbids(),
            self._lb_repo.get_sitewide_top_release_groups(
                range_=range_key, count=limit + 1, offset=offset
            ),
            return_exceptions=True,
        )
        library_mbids = library_mbids_raw if isinstance(library_mbids_raw, set) else set()
        albums = [self._transformers.lb_release_to_home(a, library_mbids) for a in lb_albums]
        has_more = len(albums) > limit
        items = albums[:limit]
        return PopularAlbumsRangeResponse(
            range_key=range_key,
            label=HomeDataTransformers.get_range_label(range_key),
            items=items,
            offset=offset,
            limit=limit,
            has_more=has_more,
        )

    async def _get_trending_artists_lastfm(self, limit: int = 10) -> TrendingArtistsResponse:
        library_artists = await self._library_repo.get_artists_from_library(include_unmonitored=True)
        library_mbids = {a.get("mbid", "").lower() for a in library_artists if a.get("mbid")}
        lfm_artists = await self._lfm_repo.get_global_top_artists(limit=limit + 1)
        artists = [
            a
            for a in (
                self._transformers.lastfm_artist_to_home(artist, library_mbids)
                for artist in lfm_artists
            )
            if a is not None
        ]
        featured = artists[0] if artists else None
        items = artists[1:limit] if len(artists) > 1 else []
        single_range = TrendingTimeRange(
            range_key="all_time",
            label="Global",
            featured=featured,
            items=items,
            total_count=len(artists),
        )
        return TrendingArtistsResponse(
            this_week=single_range,
            this_month=single_range,
            this_year=single_range,
            all_time=single_range,
        )

    async def _get_popular_albums_lastfm(
        self, limit: int = 10, lfm_repo: Any = None, lfm_username: str | None = None
    ) -> PopularAlbumsResponse:
        ranges = ["this_week", "this_month", "this_year", "all_time"]
        library_mbids = await self._native_album_mbids()
        repo = lfm_repo or self._lfm_repo
        # per-user your-top passes its own username; sitewide passes none and falls
        # back to the global account
        username = lfm_username or self._get_lastfm_username()
        if username and repo:
            tasks = {
                range_key: repo.get_user_top_albums(
                    username,
                    period=self._lastfm_period_for_range(range_key),
                    limit=limit + 1,
                )
                for range_key in ranges
            }
            results = await self._execute_tasks(tasks)
        else:
            logger.warning("No Last.fm username configured; returning empty popular albums")
            empty_range = PopularTimeRange(
                range_key="all_time",
                label="Global",
                featured=None,
                items=[],
                total_count=0,
            )
            return PopularAlbumsResponse(
                this_week=empty_range,
                this_month=empty_range,
                this_year=empty_range,
                all_time=empty_range,
            )
        response_data: dict[str, PopularTimeRange] = {}
        for range_key in ranges:
            lfm_albums = results.get(range_key) or []
            albums = [
                HomeAlbum(
                    mbid=None,
                    name=album.name,
                    artist_name=album.artist_name,
                    artist_mbid=None,
                    image_url=album.image_url or None,
                    listen_count=album.playcount,
                    in_library=(album.mbid or "").lower() in library_mbids if album.mbid else False,
                    source="lastfm",
                )
                for album in lfm_albums
            ]
            response_data[range_key] = PopularTimeRange(
                range_key=range_key,
                label=HomeDataTransformers.get_range_label(range_key),
                featured=albums[0] if albums else None,
                items=albums[1:limit] if len(albums) > 1 else [],
                total_count=len(albums),
            )

        return PopularAlbumsResponse(
            this_week=response_data["this_week"],
            this_month=response_data["this_month"],
            this_year=response_data["this_year"],
            all_time=response_data["all_time"],
        )

    async def _get_trending_artists_lastfm_range(
        self, range_key: str = "this_week", limit: int = 25, offset: int = 0
    ) -> TrendingArtistsRangeResponse:
        total_to_fetch = min(limit + offset + 1, 200)
        lfm_artists, library_artists = await asyncio.gather(
            self._lfm_repo.get_global_top_artists(limit=total_to_fetch),
            self._library_repo.get_artists_from_library(include_unmonitored=True),
        )
        library_mbids = {a.get("mbid", "").lower() for a in library_artists if a.get("mbid")}
        artists = [
            a
            for a in (
                self._transformers.lastfm_artist_to_home(artist, library_mbids)
                for artist in lfm_artists
            )
            if a is not None
        ]
        start = min(offset, len(artists))
        end = start + limit
        return TrendingArtistsRangeResponse(
            range_key=range_key,
            label=HomeDataTransformers.get_range_label(range_key),
            items=artists[start:end],
            offset=offset,
            limit=limit,
            has_more=end < len(artists),
        )

    async def _get_popular_albums_lastfm_range(
        self, range_key: str = "this_week", limit: int = 25, offset: int = 0,
        lfm_repo: Any = None, lfm_username: str | None = None,
    ) -> PopularAlbumsRangeResponse:
        repo = lfm_repo or self._lfm_repo
        username = lfm_username or self._get_lastfm_username()
        if not (username and repo):
            return PopularAlbumsRangeResponse(
                range_key=range_key,
                label=HomeDataTransformers.get_range_label(range_key),
                items=[],
                offset=offset,
                limit=limit,
                has_more=False,
            )

        total_to_fetch = min(limit + offset + 1, 200)
        lfm_albums, library_mbids_raw = await asyncio.gather(
            repo.get_user_top_albums(
                username,
                period=self._lastfm_period_for_range(range_key),
                limit=total_to_fetch,
            ),
            self._native_album_mbids(),
            return_exceptions=True,
        )
        library_mbids = library_mbids_raw if isinstance(library_mbids_raw, set) else set()
        albums = [
            HomeAlbum(
                mbid=album.mbid,
                name=album.name,
                artist_name=album.artist_name,
                artist_mbid=None,
                image_url=album.image_url or None,
                listen_count=album.playcount,
                in_library=(album.mbid or "").lower() in library_mbids if album.mbid else False,
                source="lastfm",
            )
            for album in lfm_albums
        ]
        start = min(offset, len(albums))
        end = start + limit
        return PopularAlbumsRangeResponse(
            range_key=range_key,
            label=HomeDataTransformers.get_range_label(range_key),
            items=albums[start:end],
            offset=offset,
            limit=limit,
            has_more=end < len(albums),
        )

    async def get_your_top_albums(
        self, user_id: str, limit: int = 10, source: str | None = None
    ) -> PopularAlbumsResponse:
        lb_client, lfm_client, lb_username, lfm_username, resolved = await self._resolve_user_top(
            user_id, source
        )
        if resolved == "lastfm" and lfm_client and lfm_username:
            return await self._get_popular_albums_lastfm(
                limit, lfm_repo=lfm_client, lfm_username=lfm_username
            )

        if not (lb_client and lb_username):
            empty = PopularTimeRange(
                range_key="all_time", label="All Time", featured=None, items=[], total_count=0
            )
            return PopularAlbumsResponse(
                this_week=empty, this_month=empty, this_year=empty, all_time=empty
            )

        library_mbids = await self._native_album_mbids()
        ranges = ["this_week", "this_month", "this_year", "all_time"]
        tasks = {
            r: lb_client.get_user_top_release_groups(
                username=lb_username, range_=r, count=limit + 1
            )
            for r in ranges
        }
        results = await self._execute_tasks(tasks)
        response_data: dict[str, PopularTimeRange] = {}
        for r in ranges:
            rgs = results.get(r) or []
            albums = [self._transformers.lb_release_to_home(rg, library_mbids) for rg in rgs]
            response_data[r] = PopularTimeRange(
                range_key=r,
                label=HomeDataTransformers.get_range_label(r),
                featured=albums[0] if albums else None,
                items=albums[1:limit] if len(albums) > 1 else [],
                total_count=len(albums),
            )
        return PopularAlbumsResponse(
            this_week=response_data["this_week"],
            this_month=response_data["this_month"],
            this_year=response_data["this_year"],
            all_time=response_data["all_time"],
        )

    async def get_your_top_albums_by_range(
        self,
        user_id: str,
        range_key: str = "this_week",
        limit: int = 25,
        offset: int = 0,
        source: str | None = None,
    ) -> PopularAlbumsRangeResponse:
        allowed_ranges = ["this_week", "this_month", "this_year", "all_time"]
        if range_key not in allowed_ranges:
            range_key = "this_week"
        lb_client, lfm_client, lb_username, lfm_username, resolved = await self._resolve_user_top(
            user_id, source
        )
        if resolved == "lastfm" and lfm_client and lfm_username:
            return await self._get_popular_albums_lastfm_range(
                range_key=range_key, limit=limit, offset=offset,
                lfm_repo=lfm_client, lfm_username=lfm_username,
            )

        if not (lb_client and lb_username):
            return PopularAlbumsRangeResponse(
                range_key=range_key,
                label=HomeDataTransformers.get_range_label(range_key),
                items=[],
                offset=offset,
                limit=limit,
                has_more=False,
            )

        library_mbids_raw, rgs = await asyncio.gather(
            self._native_album_mbids(),
            lb_client.get_user_top_release_groups(
                username=lb_username, range_=range_key, count=limit + 1, offset=offset
            ),
            return_exceptions=True,
        )
        library_mbids = library_mbids_raw if isinstance(library_mbids_raw, set) else set()
        albums = [self._transformers.lb_release_to_home(rg, library_mbids) for rg in rgs]
        has_more = len(albums) > limit
        items = albums[:limit]
        return PopularAlbumsRangeResponse(
            range_key=range_key,
            label=HomeDataTransformers.get_range_label(range_key),
            items=items,
            offset=offset,
            limit=limit,
            has_more=has_more,
        )

    @staticmethod
    def _lastfm_period_for_range(range_key: str) -> str:
        mapping = {
            "this_week": "7day",
            "this_month": "1month",
            "this_year": "12month",
            "all_time": "overall",
        }
        return mapping.get(range_key, "1month")
