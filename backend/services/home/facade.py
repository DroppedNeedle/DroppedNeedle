"""Slim HomeService facade that preserves the constructor signature and delegates to sub-services."""

from __future__ import annotations

import asyncio
import logging
from pathlib import Path
from typing import Any, NamedTuple

import msgspec.structs

from api.v1.schemas.home import (
    HomeResponse,
    HomeGenre,
    HomeArtist,
    DiscoverPreview,
    HomeIntegrationStatus,
)
from api.v1.schemas.library import LibraryAlbum
from repositories.protocols import (
    ListenBrainzRepositoryProtocol,
    JellyfinRepositoryProtocol,
    LibraryRepositoryProtocol,
    MusicBrainzRepositoryProtocol,
    LastFmRepositoryProtocol,
)
from services.preferences_service import PreferencesService
from services.home_transformers import HomeDataTransformers
from services.per_user_client_factory import PerUserClientFactory
from infrastructure.persistence.user_listening_prefs_store import UserListeningPrefsStore
from infrastructure.persistence.play_history_store import PlayHistoryStore
from infrastructure.cache.cache_keys import DISCOVER_RESPONSE_PREFIX, HOME_RESPONSE_PREFIX
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.http.deduplication import deduplicate

from .integration_helpers import HomeIntegrationHelpers, resolve_source_value
from .section_builders import HomeSectionBuilders
from .genre_service import GenreService
from services.weekly_exploration_service import WeeklyExplorationService

logger = logging.getLogger(__name__)


class _HomeUserMusic(NamedTuple):
    # per-request music identity: request-scoped LB/Last.fm clients, never a mutated singleton
    lb_client: Any
    lfm_client: Any
    lb_username: str | None
    lfm_username: str | None
    lb_enabled: bool
    lfm_enabled: bool
    resolved_source: str


class HomeService:
    def __init__(
        self,
        listenbrainz_repo: ListenBrainzRepositoryProtocol,
        jellyfin_repo: JellyfinRepositoryProtocol,
        library_repo: LibraryRepositoryProtocol,
        musicbrainz_repo: MusicBrainzRepositoryProtocol,
        preferences_service: PreferencesService,
        memory_cache: CacheInterface | None = None,
        lastfm_repo: LastFmRepositoryProtocol | None = None,
        audiodb_image_service: Any = None,
        cache_dir: Path | None = None,
        client_factory: PerUserClientFactory | None = None,
        listening_prefs_store: UserListeningPrefsStore | None = None,
        play_history_store: PlayHistoryStore | None = None,
    ):
        self._lb_repo = listenbrainz_repo
        self._jf_repo = jellyfin_repo
        self._library_repo = library_repo
        self._mb_repo = musicbrainz_repo
        self._preferences = preferences_service
        self._memory_cache = memory_cache
        self._lfm_repo = lastfm_repo
        self._audiodb_image_service = audiodb_image_service
        self._client_factory = client_factory
        self._prefs_store = listening_prefs_store
        self._play_history_store = play_history_store
        self._transformers = HomeDataTransformers(jellyfin_repo)

        self._helpers = HomeIntegrationHelpers(preferences_service)
        self._builders = HomeSectionBuilders(self._transformers)
        self._genre = GenreService(
            musicbrainz_repo, memory_cache, audiodb_image_service,
            cache_dir=cache_dir, preferences_service=preferences_service,
        )
        self._weekly_exploration = WeeklyExplorationService(listenbrainz_repo, musicbrainz_repo)

    def clear_genre_disk_cache(self) -> int:
        return self._genre.clear_disk_cache()

    def _resolve_source(self, source: str | None = None) -> str:
        return self._helpers.resolve_source(source)

    def _build_service_prompts(self, lb_enabled, download_client_configured, lfm_enabled):
        return self._builders.build_service_prompts(lb_enabled, download_client_configured, lfm_enabled)

    def get_integration_status(self) -> HomeIntegrationStatus:
        return HomeIntegrationStatus(
            listenbrainz=self._helpers.is_listenbrainz_enabled(),
            jellyfin=self._helpers.is_jellyfin_enabled(),
            download_client=self._helpers.is_download_client_configured(),
            library=self._helpers.is_library_configured(),
            youtube=self._helpers.is_youtube_enabled(),
            youtube_api=self._helpers.is_youtube_api_enabled(),
            localfiles=self._helpers.is_local_files_enabled(),
            lastfm=self._helpers.is_lastfm_enabled(),
            navidrome=self._helpers.is_navidrome_enabled(),
            plex=self._helpers.is_plex_enabled(),
        )

    async def has_local_files(self) -> bool:
        # the engine is always present; affordances only light up once music exists
        return await self._library_repo.has_any_files()

    async def get_genre_artist(
        self, genre_name: str, exclude_mbids: set[str] | None = None
    ) -> str | None:
        return await self._genre.get_genre_artist(genre_name, exclude_mbids)

    async def get_genre_artists_batch(self, genres: list[str]) -> dict[str, str | None]:
        return await self._genre.get_genre_artists_batch(genres)

    async def get_library_genre_names(self, limit: int = 20) -> list[str]:
        # account-less top genres from the shared library, for the genre-cover warmer
        library_albums = await self._library_repo.get_library(include_unmonitored=True)
        genre_list = self._builders.build_genre_list_section(library_albums, None)
        if not genre_list or not genre_list.items:
            return []
        return [g.name for g in genre_list.items[:limit] if isinstance(g, HomeGenre)]

    def _get_home_cache_key(
        self, user_id: str, resolved_source: str, lb_enabled: bool, lfm_enabled: bool
    ) -> str:
        # lb/lfm enable flags keep a user's connect/disconnect busting their own key
        return f"{HOME_RESPONSE_PREFIX}{user_id}:{resolved_source}:{lb_enabled}:{lfm_enabled}"

    async def _resolve_user_music(self, user_id: str, source: str | None) -> _HomeUserMusic:
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
        lb_enabled = lb_client is not None
        lfm_enabled = lfm_client is not None
        resolved = resolve_source_value(source, primary_source, lb_enabled, lfm_enabled)
        return _HomeUserMusic(
            lb_client, lfm_client, lb_username, lfm_username, lb_enabled, lfm_enabled, resolved
        )

    def _integration_status_for_user(
        self, lb_enabled: bool, lfm_enabled: bool
    ) -> HomeIntegrationStatus:
        # media-server / library integrations stay global (D2); only LB/Last.fm are per-user
        return msgspec.structs.replace(
            self.get_integration_status(), listenbrainz=lb_enabled, lastfm=lfm_enabled
        )

    async def get_cached_home_data(self, user_id: str, source: str | None = None) -> HomeResponse | None:
        if not self._memory_cache:
            return None
        music = await self._resolve_user_music(user_id, source)
        cache_key = self._get_home_cache_key(
            user_id, music.resolved_source, music.lb_enabled, music.lfm_enabled
        )
        return await self._memory_cache.get(cache_key)

    @deduplicate(lambda self, user_id, source=None: f"{HOME_RESPONSE_PREFIX}dedup:{user_id}:{source or ''}")
    async def get_home_data(self, user_id: str, source: str | None = None) -> HomeResponse:
        HOME_CACHE_TTL = 300
        music = await self._resolve_user_music(user_id, source)
        resolved_source = music.resolved_source
        lb_client = music.lb_client
        lfm_client = music.lfm_client
        lb_enabled = music.lb_enabled
        lfm_enabled = music.lfm_enabled
        username = music.lb_username
        lfm_username = music.lfm_username
        home_settings = self._preferences.get_home_settings()

        if self._memory_cache:
            cache_key = self._get_home_cache_key(user_id, resolved_source, lb_enabled, lfm_enabled)
            cached = await self._memory_cache.get(cache_key)
            if cached is not None:
                if not cached.genre_artists:
                    genre_section = await self._genre.get_cached_genre_section(resolved_source)
                    if genre_section:
                        from infrastructure.serialization import clone_with_updates
                        cached = clone_with_updates(cached, {
                            "genre_artists": genre_section[0],
                            "genre_artist_images": genre_section[1],
                        })
                        if cached.genre_list and cached.genre_list.items:
                            cur_names = [g.name for g in cached.genre_list.items[:20] if isinstance(g, HomeGenre)]
                            missing = [n for n in cur_names if n not in genre_section[0]]
                            if missing:
                                asyncio.create_task(
                                    self._genre.build_and_cache_genre_section(resolved_source, cur_names)
                                )
                if not home_settings.show_whats_hot:
                    from infrastructure.serialization import clone_with_updates
                    cached = clone_with_updates(cached, {
                        "trending_artists": None,
                        "popular_albums": None,
                    })
                return cached

        integration_status = self._integration_status_for_user(lb_enabled, lfm_enabled)
        download_client_configured = integration_status.download_client

        tasks: dict[str, Any] = {}

        # sitewide trending/popular need no account; keep the global singleton repos
        if resolved_source == "listenbrainz":
            if home_settings.show_whats_hot:
                tasks["lb_trending_artists"] = self._lb_repo.get_sitewide_top_artists(count=20)
                tasks["lb_trending_albums"] = self._lb_repo.get_sitewide_top_release_groups(count=20)
        elif resolved_source == "lastfm" and self._lfm_repo:
            if home_settings.show_whats_hot:
                tasks["lfm_global_top_artists"] = self._lfm_repo.get_global_top_artists(limit=20)
            if lfm_client and lfm_username:
                tasks["lfm_top_albums"] = lfm_client.get_user_top_albums(
                    lfm_username, period="1month", limit=20
                )

        # library home sections are driven by the native library (the scanner is
        # always present, D8), not by whether a download client is configured
        if integration_status.library:
            tasks["library_albums"] = self._library_repo.get_library(include_unmonitored=True)
            # get_library() is an empty stub on native installs, so the album-membership
            # set must come from get_library_mbids() (the authoritative native set the
            # frontend's /library/mbids also uses) - otherwise chart albums already in the
            # library are flagged in_library=False and wrongly show a download button.
            tasks["library_album_mbids"] = self._library_repo.get_library_mbids(
                include_release_ids=False
            )
            tasks["library_artists"] = self._library_repo.get_artists_from_library(include_unmonitored=True)
            tasks["recently_imported"] = self._library_repo.get_recently_imported(limit=15)

        # personalized sections use the requesting user's request-scoped client;
        # omitted entirely for unlinked users (D1)
        if resolved_source == "listenbrainz" and lb_client and username:
            tasks["lb_loved"] = lb_client.get_user_loved_recordings(count=20)
            tasks["lb_genres"] = lb_client.get_user_genre_activity(username)
            tasks["lb_user_top_rgs"] = lb_client.get_user_top_release_groups(
                username=username, range_="this_month", count=20
            )
            tasks["lb_weekly_exploration"] = self._weekly_exploration.build_section(
                username, lb_repo=lb_client
            )
        elif resolved_source == "lastfm" and lfm_client and lfm_username:
            tasks["lfm_loved"] = lfm_client.get_user_loved_tracks(lfm_username, limit=20)

        results = await self._helpers.execute_tasks(tasks)

        library_albums: list[LibraryAlbum] = results.get("library_albums") or []
        library_artists: list[dict] = results.get("library_artists") or []
        recently_imported: list[LibraryAlbum] = results.get("recently_imported") or []
        library_artist_mbids = {
            a.get("mbid", "").lower() for a in library_artists if a.get("mbid")
        }
        library_album_mbids = {
            m.lower() for m in (results.get("library_album_mbids") or set())
        }

        response = HomeResponse(integration_status=integration_status)

        response.recently_added = self._builders.build_recently_added_section(recently_imported)
        response.library_artists = self._builders.build_library_artists_section(library_artists)
        response.library_albums = self._builders.build_library_albums_section(library_albums)

        if resolved_source == "listenbrainz":
            if home_settings.show_whats_hot:
                response.trending_artists = self._builders.build_trending_artists_section(
                    results, library_artist_mbids
                )
                response.popular_albums = self._builders.build_popular_albums_section(
                    results, library_album_mbids
                )
            if lb_client:
                response.your_top_albums = self._builders.build_lb_user_top_albums_section(
                    results, library_album_mbids
                )
                response.favorite_artists = self._builders.build_listenbrainz_favorites_section(results)
                response.weekly_exploration = results.get("lb_weekly_exploration")
        elif resolved_source == "lastfm":
            if home_settings.show_whats_hot:
                response.trending_artists = self._builders.build_lastfm_trending_section(
                    results, library_artist_mbids
                )
            if lfm_client:
                response.your_top_albums = self._builders.build_lastfm_top_albums_section(
                    results, library_album_mbids
                )
                response.favorite_artists = self._builders.build_lastfm_favorites_section(results)

        # recently_played comes from the local play_history table (D6) for all users,
        # linked or not, not from external listens
        if self._play_history_store:
            recent_records = await self._play_history_store.recent(user_id, limit=15)
            response.recently_played = self._builders.build_play_history_recent_section(recent_records)

        response.genre_list = self._builders.build_genre_list_section(
            library_albums,
            results.get("lb_genres") if resolved_source == "listenbrainz" else None,
        )

        if response.genre_list and response.genre_list.items:
            genre_names = [
                g.name for g in response.genre_list.items[:20]
                if isinstance(g, HomeGenre)
            ]
            cached_section = await self._genre.get_cached_genre_section(resolved_source)
            if cached_section:
                response.genre_artists, response.genre_artist_images = cached_section
                missing = [n for n in genre_names if n not in cached_section[0]]
                if missing:
                    asyncio.create_task(
                        self._genre.build_and_cache_genre_section(resolved_source, genre_names)
                    )
            elif genre_names:
                asyncio.create_task(
                    self._genre.build_and_cache_genre_section(resolved_source, genre_names)
                )

        response.service_prompts = self._builders.build_service_prompts(
            lb_enabled,
            download_client_configured,
            lfm_enabled,
        )

        response.discover_preview = await self._build_discover_preview(
            user_id, resolved_source, lb_enabled, lfm_enabled
        )

        if self._memory_cache:
            cache_key = self._get_home_cache_key(user_id, resolved_source, lb_enabled, lfm_enabled)
            await self._memory_cache.set(cache_key, response, HOME_CACHE_TTL)

        return response

    async def _build_discover_preview(
        self, user_id: str, resolved_source: str, lb_enabled: bool, lfm_enabled: bool
    ) -> DiscoverPreview | None:
        if not self._memory_cache:
            return None
        try:
            from api.v1.schemas.discover import DiscoverResponse as DR
            # read the user-dimensioned discover key (incl. enable flags) so it tracks
            # the live discover cache entry
            cache_key = f"{DISCOVER_RESPONSE_PREFIX}{user_id}:{resolved_source}:{lb_enabled}:{lfm_enabled}"
            cached = await self._memory_cache.get(cache_key)
            if not cached or not isinstance(cached, DR):
                return None
            if not cached.because_you_listen_to:
                return None
            first = cached.because_you_listen_to[0]
            preview_items = [
                item for item in first.section.items[:15]
                if isinstance(item, HomeArtist)
            ]
            return DiscoverPreview(
                seed_artist=first.seed_artist,
                seed_artist_mbid=first.seed_artist_mbid,
                items=preview_items,
            )
        except Exception:  # noqa: BLE001
            return None

    async def _resolve_release_mbids(self, release_ids: list[str]) -> dict[str, str]:
        if not release_ids:
            return {}
        import asyncio as _asyncio
        tasks = [self._mb_repo.get_release_group_id_from_release(rid) for rid in release_ids]
        results = await _asyncio.gather(*tasks, return_exceptions=True)
        rg_map: dict[str, str] = {}
        for rid, rg_id in zip(release_ids, results):
            if isinstance(rg_id, str) and rg_id:
                rg_map[rid] = rg_id
        return rg_map
