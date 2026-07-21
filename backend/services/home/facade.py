"""Slim HomeService facade that preserves the constructor signature and delegates to sub-services."""

from __future__ import annotations

import asyncio
import logging
import time
from pathlib import Path
from typing import Any, NamedTuple, TYPE_CHECKING

import msgspec.structs

from api.v1.schemas.home import (
    HomeResponse,
    HomeSection,
    HomeGenre,
    HomeArtist,
    HomeAlbum,
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
from infrastructure.persistence.user_listening_prefs_store import (
    UserListeningPrefsStore,
)
from infrastructure.persistence.play_history_store import PlayHistoryStore
from infrastructure.cache.cache_keys import (
    DISCOVER_RESPONSE_PREFIX,
    HOME_RESPONSE_PREFIX,
)
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.http.deduplication import deduplicate
from infrastructure.serialization import clone_with_updates

from .integration_helpers import HomeIntegrationHelpers, resolve_source_value
from .section_builders import HomeSectionBuilders
from services.weekly_exploration_service import WeeklyExplorationService

logger = logging.getLogger(__name__)

if TYPE_CHECKING:
    from services.native.library_ownership_service import LibraryOwnershipService
    from services.home.genre_artwork_service import GenreArtworkService
    from services.native.background_workload_gate import BackgroundWorkloadGate

# full cache entries live long; freshness is governed by the SWR window below so an
# expired-but-present copy is still served instantly while a rebuild runs behind it
HOME_CACHE_TTL = 3600
HOME_STALE_REVALIDATE_SECONDS = 300


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
        ownership_service: "LibraryOwnershipService | None" = None,
        genre_artwork_service: "GenreArtworkService | None" = None,
        workload_gate: "BackgroundWorkloadGate | None" = None,
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
        self._ownership = ownership_service
        self._genre_artwork = genre_artwork_service
        self._workload_gate = workload_gate
        self._transformers = HomeDataTransformers(jellyfin_repo)

        self._helpers = HomeIntegrationHelpers(preferences_service)
        # SWR bookkeeping: per-user in-flight guard + last build-attempt times
        self._building: set[str] = set()
        self._built_at: dict[str, float] = {}
        self._builders = HomeSectionBuilders(self._transformers)
        self._weekly_exploration = WeeklyExplorationService(
            listenbrainz_repo, musicbrainz_repo
        )

    async def _apply_album_ownership(self, response: HomeResponse) -> None:
        if self._ownership is None:
            return
        from services.native.library_ownership_service import AlbumOwnershipCandidate

        albums: list[HomeAlbum] = []
        for field in msgspec.structs.fields(HomeResponse):
            section = getattr(response, field.name)
            if not isinstance(section, HomeSection):
                continue
            albums.extend(item for item in section.items if isinstance(item, HomeAlbum))
        candidates = []
        for album in albums:
            year = None
            if album.release_date and album.release_date[:4].isdigit():
                year = int(album.release_date[:4])
            candidates.append(
                AlbumOwnershipCandidate(
                    release_group_mbid=album.mbid,
                    title=album.name,
                    album_artist=album.artist_name or "",
                    year=year,
                )
            )
        projections = await self._ownership.project_albums(candidates)
        for album, projection in zip(albums, projections):
            album.in_library = projection.owned
            if projection.local_album_id is not None:
                album.local_id = projection.local_album_id

    async def _apply_artist_ownership(self, response: HomeResponse) -> None:
        artists: list[HomeArtist] = []
        for field in msgspec.structs.fields(HomeResponse):
            section = getattr(response, field.name)
            if isinstance(section, HomeSection):
                artists.extend(
                    item for item in section.items if isinstance(item, HomeArtist)
                )
        if response.discover_preview is not None:
            artists.extend(response.discover_preview.items)
        candidate_ids = [artist.mbid for artist in artists if artist.mbid]
        if not candidate_ids:
            return
        try:
            owned = await self._library_repo.existing_artist_mbids(candidate_ids)
        except Exception as exc:  # noqa: BLE001 - ownership flags are best-effort
            logger.warning("native artist mbid lookup failed: %s", exc)
            return
        owned_ids = {identifier.casefold() for identifier in owned}
        for artist in artists:
            if artist.mbid and artist.mbid.casefold() in owned_ids:
                artist.in_library = True

    def _resolve_source(self, source: str | None = None) -> str:
        return self._helpers.resolve_source(source)

    def _build_service_prompts(
        self, lb_enabled, download_client_configured, lfm_enabled
    ):
        return self._builders.build_service_prompts(
            lb_enabled, download_client_configured, lfm_enabled
        )

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

    def _get_home_cache_key(
        self, user_id: str, lb_enabled: bool, lfm_enabled: bool
    ) -> str:
        # lb/lfm enable flags keep a user's connect/disconnect busting their own key.
        # No source dimension: the page is unified, both services build into one response.
        return f"{HOME_RESPONSE_PREFIX}{user_id}:{lb_enabled}:{lfm_enabled}"

    async def _resolve_user_music(
        self, user_id: str, source: str | None
    ) -> _HomeUserMusic:
        lb_client = lfm_client = None
        lb_username = lfm_username = None
        if self._client_factory:
            lb_client = await self._client_factory.resolve_listenbrainz(user_id)
            lfm_client = await self._client_factory.resolve_lastfm(user_id)
            lb_username = await self._client_factory.resolve_listenbrainz_username(
                user_id
            )
            lfm_username = await self._client_factory.resolve_lastfm_username(user_id)
        primary_source = "listenbrainz"
        if self._prefs_store:
            prefs = await self._prefs_store.get(user_id)
            primary_source = prefs.primary_music_source
        lb_enabled = lb_client is not None
        lfm_enabled = lfm_client is not None
        resolved = resolve_source_value(source, primary_source, lb_enabled, lfm_enabled)
        return _HomeUserMusic(
            lb_client,
            lfm_client,
            lb_username,
            lfm_username,
            lb_enabled,
            lfm_enabled,
            resolved,
        )

    def _integration_status_for_user(
        self, lb_enabled: bool, lfm_enabled: bool
    ) -> HomeIntegrationStatus:
        # media-server / library integrations stay global (D2); only LB/Last.fm are per-user
        return msgspec.structs.replace(
            self.get_integration_status(), listenbrainz=lb_enabled, lastfm=lfm_enabled
        )

    async def get_cached_home_data(self, user_id: str) -> HomeResponse | None:
        if not self._memory_cache:
            return None
        music = await self._resolve_user_music(user_id, None)
        cache_key = self._get_home_cache_key(
            user_id, music.lb_enabled, music.lfm_enabled
        )
        cached = await self._memory_cache.get(cache_key)
        if isinstance(cached, HomeResponse):
            await self._apply_genre_artwork(cached)
        return cached

    async def _apply_genre_artwork(self, response: HomeResponse) -> None:
        if self._genre_artwork is None or not response.genre_list:
            return
        genre_names = [
            item.name
            for item in response.genre_list.items[:20]
            if isinstance(item, HomeGenre)
        ]
        response.genre_artwork = await self._genre_artwork.get_artwork_batch(
            genre_names
        )

    def _trigger_warm(self, user_id: str) -> None:
        """Background full rebuild for the user if one isn't already running."""
        from core.task_registry import TaskRegistry

        registry = TaskRegistry.get_instance()
        task_name = f"home-warm-{user_id}"
        if registry.is_running(task_name):
            return
        task = asyncio.create_task(self.warm_cache(user_id))
        try:
            registry.register(task_name, task)
        except RuntimeError:
            pass

    async def warm_cache(self, user_id: str) -> None:
        if self._workload_gate is not None:
            await self._workload_gate.wait_until_available()
        if user_id in self._building:
            return
        self._building.add(user_id)
        cache_key: str | None = None
        try:
            # resolve INSIDE the try: a transient token-decrypt or locked-SQLite read
            # here must still clear the building flag, or the user is stranded in a
            # permanent refreshing state (every later poll sees building=True)
            music = await self._resolve_user_music(user_id, None)
            cache_key = self._get_home_cache_key(
                user_id, music.lb_enabled, music.lfm_enabled
            )
            response = await self._build_full(user_id, music)
            if self._memory_cache:
                await self._memory_cache.set(cache_key, response, HOME_CACHE_TTL)
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to build home data: {e}")
        finally:
            self._building.discard(user_id)
            # record every completed attempt (success or failure) so the miss path
            # backs off instead of re-triggering a doomed build on every poll
            if cache_key is not None:
                self._built_at[cache_key] = time.time()

    @deduplicate(lambda self, user_id: f"{HOME_RESPONSE_PREFIX}dedup:{user_id}")
    async def get_home_data(self, user_id: str) -> HomeResponse:
        """Never blocks on external services: a cached copy is served immediately
        (revalidated in the background when stale); a cache miss gets an instant
        library-only response with ``refreshing=true`` while the full build runs
        behind it (the frontend polls and the sections stream in)."""
        music = await self._resolve_user_music(user_id, None)

        if not self._memory_cache:
            # no cache to hand off through (unit tests): build inline
            return await self._build_full(user_id, music)

        cache_key = self._get_home_cache_key(
            user_id, music.lb_enabled, music.lfm_enabled
        )
        building = user_id in self._building
        cached = await self._memory_cache.get(cache_key)
        if cached is not None:
            age = time.time() - self._built_at.get(cache_key, 0.0)
            if not building and age > HOME_STALE_REVALIDATE_SECONDS:
                self._trigger_warm(user_id)
                building = True
            response = clone_with_updates(cached, {"refreshing": building})
            await self._apply_genre_artwork(response)
            return response

        attempted_recently = (
            time.time() - self._built_at.get(cache_key, 0.0)
            <= HOME_STALE_REVALIDATE_SECONDS
        )
        if not building and not attempted_recently:
            self._trigger_warm(user_id)
            building = True
        return await self._build_fast(user_id, music, refreshing=building)

    async def _build_fast(
        self, user_id: str, music: _HomeUserMusic, refreshing: bool
    ) -> HomeResponse:
        """Local-only first paint: library shelves, play history, and genre tiles
        come from SQLite; everything external streams in via the background build."""
        integration_status = self._integration_status_for_user(
            music.lb_enabled, music.lfm_enabled
        )
        tasks: dict[str, Any] = {}
        if integration_status.library:
            tasks["library_albums"] = self._library_repo.get_home_albums(limit=15)
            tasks["library_artists"] = self._library_repo.get_home_artists(limit=15)
            tasks["recently_imported"] = self._library_repo.get_recently_imported(
                limit=15
            )
        results = await self._helpers.execute_tasks(tasks)

        response = HomeResponse(
            integration_status=integration_status, refreshing=refreshing
        )
        library_albums: list[LibraryAlbum] = results.get("library_albums") or []
        response.recently_added = self._builders.build_recently_added_section(
            results.get("recently_imported") or []
        )
        response.library_artists = self._builders.build_library_artists_section(
            results.get("library_artists") or []
        )
        response.library_albums = self._builders.build_library_albums_section(
            library_albums
        )
        if self._play_history_store:
            recent_records = await self._play_history_store.recent(user_id, limit=15)
            response.recently_played = self._builders.build_play_history_recent_section(
                recent_records
            )
        response.genre_list = self._builders.build_genre_list_section(
            library_albums, None
        )
        await self._apply_genre_artwork(response)
        response.service_prompts = self._builders.build_service_prompts(
            music.lb_enabled,
            integration_status.download_client,
            music.lfm_enabled,
        )
        response.discover_preview = await self._build_discover_preview(
            user_id, music.lb_enabled, music.lfm_enabled
        )
        await self._apply_album_ownership(response)
        await self._apply_artist_ownership(response)
        return response

    async def _build_full(self, user_id: str, music: _HomeUserMusic) -> HomeResponse:
        # primary drives the single-slot sections (trending, popular, favorites,
        # your-top); source-specific sections run whenever their service is linked
        primary = music.resolved_source
        lb_client = music.lb_client
        lfm_client = music.lfm_client
        lb_enabled = music.lb_enabled
        lfm_enabled = music.lfm_enabled
        username = music.lb_username
        lfm_username = music.lfm_username

        integration_status = self._integration_status_for_user(lb_enabled, lfm_enabled)
        download_client_configured = integration_status.download_client

        tasks: dict[str, Any] = {}

        # sitewide trending/popular need no account; keep the global singleton repos
        if primary == "lastfm" and self._lfm_repo:
            tasks["lfm_global_top_artists"] = self._lfm_repo.get_global_top_artists(
                limit=20
            )
            if lfm_client and lfm_username:
                tasks["lfm_top_albums"] = lfm_client.get_user_top_albums(
                    lfm_username, period="1month", limit=20
                )
        else:
            tasks["lb_trending_artists"] = self._lb_repo.get_sitewide_top_artists(
                count=20
            )
            tasks["lb_trending_albums"] = self._lb_repo.get_sitewide_top_release_groups(
                count=20
            )

        # library home sections are driven by the native library (the scanner is
        # always present, D8), not by whether a download client is configured
        if integration_status.library:
            tasks["library_albums"] = self._library_repo.get_home_albums(limit=15)
            tasks["library_artists"] = self._library_repo.get_home_artists(limit=15)
            tasks["recently_imported"] = self._library_repo.get_recently_imported(
                limit=15
            )

        # personalized sections use the requesting user's request-scoped client;
        # omitted entirely for unlinked users (D1). Weekly exploration and genre
        # activity are LB-specific and run whenever LB is linked; the single-slot
        # loved/your-top sections follow the primary source.
        if lb_client and username:
            tasks["lb_genres"] = lb_client.get_user_genre_activity(username)
            tasks["lb_weekly_exploration"] = self._weekly_exploration.build_section(
                username, lb_repo=lb_client
            )
            if primary == "listenbrainz":
                tasks["lb_loved"] = lb_client.get_user_loved_recordings(count=20)
                tasks["lb_user_top_rgs"] = lb_client.get_user_top_release_groups(
                    username=username, range_="this_month", count=20
                )
        if primary == "lastfm" and lfm_client and lfm_username:
            tasks["lfm_loved"] = lfm_client.get_user_loved_tracks(
                lfm_username, limit=20
            )

        results = await self._helpers.execute_tasks(tasks)

        library_albums: list[LibraryAlbum] = results.get("library_albums") or []
        library_artists: list[dict] = results.get("library_artists") or []
        recently_imported: list[LibraryAlbum] = results.get("recently_imported") or []
        library_artist_mbids = {
            a.get("mbid", "").lower() for a in library_artists if a.get("mbid")
        }
        library_album_mbids = {
            album.musicbrainz_id.lower()
            for album in library_albums
            if album.musicbrainz_id
        }

        response = HomeResponse(integration_status=integration_status)

        response.recently_added = self._builders.build_recently_added_section(
            recently_imported
        )
        response.library_artists = self._builders.build_library_artists_section(
            library_artists
        )
        response.library_albums = self._builders.build_library_albums_section(
            library_albums
        )

        if primary == "lastfm":
            response.trending_artists = self._builders.build_lastfm_trending_section(
                results, library_artist_mbids
            )
            if lfm_client:
                response.your_top_albums = (
                    self._builders.build_lastfm_top_albums_section(
                        results, library_album_mbids
                    )
                )
                response.favorite_artists = (
                    self._builders.build_lastfm_favorites_section(results)
                )
        else:
            response.trending_artists = self._builders.build_trending_artists_section(
                results, library_artist_mbids
            )
            response.popular_albums = self._builders.build_popular_albums_section(
                results, library_album_mbids
            )
            if lb_client:
                response.your_top_albums = (
                    self._builders.build_lb_user_top_albums_section(
                        results, library_album_mbids
                    )
                )
                response.favorite_artists = (
                    self._builders.build_listenbrainz_favorites_section(results)
                )
        # LB-specific sections render whenever LB is linked, regardless of primary
        if lb_client:
            response.weekly_exploration = results.get("lb_weekly_exploration")

        # recently_played comes from the local play_history table (D6) for all users,
        # linked or not, not from external listens
        if self._play_history_store:
            recent_records = await self._play_history_store.recent(user_id, limit=15)
            response.recently_played = self._builders.build_play_history_recent_section(
                recent_records
            )

        response.genre_list = self._builders.build_genre_list_section(
            library_albums,
            results.get("lb_genres"),
        )

        await self._apply_genre_artwork(response)

        response.service_prompts = self._builders.build_service_prompts(
            lb_enabled,
            download_client_configured,
            lfm_enabled,
        )

        response.discover_preview = await self._build_discover_preview(
            user_id, lb_enabled, lfm_enabled
        )

        await self._apply_album_ownership(response)
        await self._apply_artist_ownership(response)

        return response

    async def _build_discover_preview(
        self, user_id: str, lb_enabled: bool, lfm_enabled: bool
    ) -> DiscoverPreview | None:
        if not self._memory_cache:
            return None
        try:
            from api.v1.schemas.discover import DiscoverResponse as DR

            # read the user-dimensioned discover key (incl. enable flags) so it tracks
            # the live discover cache entry
            cache_key = (
                f"{DISCOVER_RESPONSE_PREFIX}{user_id}:{lb_enabled}:{lfm_enabled}"
            )
            cached = await self._memory_cache.get(cache_key)
            if not cached or not isinstance(cached, DR):
                return None
            if not cached.because_you_listen_to:
                return None
            first = cached.because_you_listen_to[0]
            preview_items = [
                item
                for item in first.section.items[:15]
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

        tasks = [
            self._mb_repo.get_release_group_id_from_release(rid) for rid in release_ids
        ]
        results = await _asyncio.gather(*tasks, return_exceptions=True)
        rg_map: dict[str, str] = {}
        for rid, rg_id in zip(release_ids, results):
            if isinstance(rg_id, str) and rg_id:
                rg_map[rid] = rg_id
        return rg_map
