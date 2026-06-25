import asyncio
import logging
import random
import time
from datetime import datetime, timezone
from typing import Any

from api.v1.schemas.discover import (
    DiscoverResponse,
    BecauseYouListenTo,
    PlaylistProfile,
)
from api.v1.schemas.home import (
    HomeSection,
    HomeArtist,
    HomeAlbum,
    HomeGenre,
    ServicePrompt,
    DiscoverPreview,
)
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.cover_urls import prefer_artist_cover_url
from infrastructure.persistence import MBIDStore
from infrastructure.serialization import clone_with_updates
from repositories.protocols import (
    ListenBrainzRepositoryProtocol,
    JellyfinRepositoryProtocol,
    LibraryRepositoryProtocol,
    MusicBrainzRepositoryProtocol,
    LastFmRepositoryProtocol,
)
from repositories.listenbrainz_models import ListenBrainzArtist
from services.home_transformers import HomeDataTransformers
from services.home.integration_helpers import resolve_source_value
from services.per_user_client_factory import PerUserClientFactory
from infrastructure.persistence.user_listening_prefs_store import UserListeningPrefsStore
from services.discover.integration_helpers import IntegrationHelpers
from services.discover.mbid_resolution_service import MbidResolutionService
from services.discover.queue_strategies import build_similar_artist_pools, build_similar_artist_pools_lastfm, discover_by_genres, queue_item_to_home_album, round_robin_dedup_select
from services.weekly_exploration_service import WeeklyExplorationService

logger = logging.getLogger(__name__)

DISCOVER_CACHE_TTL = 43200  # 12 hours
# stale-while-revalidate: serve a cached response immediately but rebuild it in the
# background once it's older than this. Kept comfortably above a build's duration so
# active use can't trigger back-to-back rebuilds (external API rate limits). Tunable.
STALE_REVALIDATE_SECONDS = 300  # 5 minutes
# cap any single upstream call within a build so one slow service can't hang the update
DISCOVER_TASK_TIMEOUT_SECONDS = 25
REDISCOVER_PLAY_THRESHOLD = 5
REDISCOVER_MONTHS_AGO = 3
MISSING_ESSENTIALS_MIN_ALBUMS = 3
MISSING_ESSENTIALS_MAX_PER_ARTIST = 3
VARIOUS_ARTISTS_MBID = "89ad4ac3-39f7-470e-963a-56509c546377"
DAILY_MIX_CACHE_TTL = 86400  # 24 hours
DISCOVER_PICKS_CACHE_TTL = 14400  # 4 hours
UNEXPLORED_GENRES_THRESHOLD = 2
UNEXPLORED_GENRES_MAX = 8


class DiscoverHomepageService:
    def __init__(
        self,
        listenbrainz_repo: ListenBrainzRepositoryProtocol,
        jellyfin_repo: JellyfinRepositoryProtocol,
        library_repo: LibraryRepositoryProtocol,
        musicbrainz_repo: MusicBrainzRepositoryProtocol,
        integration: IntegrationHelpers,
        mbid_resolution: MbidResolutionService,
        memory_cache: CacheInterface | None = None,
        lastfm_repo: LastFmRepositoryProtocol | None = None,
        audiodb_image_service: Any = None,
        genre_index: Any = None,
        mbid_store: MBIDStore | None = None,
        client_factory: PerUserClientFactory | None = None,
        listening_prefs_store: UserListeningPrefsStore | None = None,
    ) -> None:
        self._lb_repo = listenbrainz_repo
        self._jf_repo = jellyfin_repo
        self._library_repo = library_repo
        self._mb_repo = musicbrainz_repo
        self._integration = integration
        self._mbid = mbid_resolution
        self._memory_cache = memory_cache
        self._lfm_repo = lastfm_repo
        self._audiodb_image_service = audiodb_image_service
        self._genre_index = genre_index
        self._mbid_store = mbid_store
        self._client_factory = client_factory
        self._prefs_store = listening_prefs_store
        self._transformers = HomeDataTransformers(jellyfin_repo)
        self._weekly_exploration = WeeklyExplorationService(listenbrainz_repo, musicbrainz_repo)
        # per-(user, source) in-flight warm guard so one user never blocks another
        self._building_keys: set[tuple[str, str]] = set()
        # cache_key -> unix time of the last successful build, for stale-while-revalidate
        self._built_at: dict[str, float] = {}

    def _daily_mix_cache_key(self, user_id: str, source: str) -> str:
        today = datetime.now(timezone.utc).date().isoformat()
        return f"daily_mix:{user_id}:{source}:{today}"

    def _discover_picks_cache_key(self, user_id: str, source: str) -> str:
        return f"discover_picks:{user_id}:{source}"

    async def _resolve_user_music(self, user_id: str, source: str | None):
        # request-scoped LB/Last.fm clients; never mutates a shared singleton's creds
        lb_client = lfm_client = None
        lb_username = lfm_username = None
        if self._client_factory:
            lb_client = await self._client_factory.resolve_listenbrainz(user_id)
            lfm_client = await self._client_factory.resolve_lastfm(user_id)
            lb_username = await self._client_factory.resolve_listenbrainz_username(user_id)
            lfm_username = await self._client_factory.resolve_lastfm_username(user_id)
        primary_source = "listenbrainz"
        if self._prefs_store:
            primary_source = (await self._prefs_store.get(user_id)).primary_music_source
        lb_enabled = lb_client is not None
        lfm_enabled = lfm_client is not None
        resolved = resolve_source_value(source, primary_source, lb_enabled, lfm_enabled)
        return lb_client, lfm_client, lb_username, lfm_username, lb_enabled, lfm_enabled, resolved

    def _trigger_warm(self, user_id: str, resolved_source: str) -> None:
        """Kick off a background rebuild for (user, source) if one isn't already running."""
        from core.task_registry import TaskRegistry

        registry = TaskRegistry.get_instance()
        task_name = f"discover-homepage-warm-{user_id}-{resolved_source}"
        if registry.is_running(task_name):
            return
        task = asyncio.create_task(self.warm_cache(user_id, source=resolved_source))
        try:
            registry.register(task_name, task)
        except RuntimeError:
            pass

    async def get_discover_data(self, user_id: str, source: str | None = None) -> DiscoverResponse:
        _, _, _, _, lb_enabled, lfm_enabled, resolved_source = await self._resolve_user_music(user_id, source)
        building = (user_id, resolved_source) in self._building_keys
        if self._memory_cache:
            cache_key = self._integration.get_discover_cache_key(
                user_id, resolved_source, lb_enabled, lfm_enabled
            )
            cached = await self._memory_cache.get(cache_key)
            if cached is not None and isinstance(cached, DiscoverResponse):
                # stale-while-revalidate: serve the cached copy immediately, but rebuild
                # in the background once it's older than the freshness window so the data
                # always converges to fresh without ever showing the build screen again
                age = time.time() - self._built_at.get(cache_key, 0.0)
                if not building and age > STALE_REVALIDATE_SECONDS:
                    self._trigger_warm(user_id, resolved_source)
                    building = True
                updates = {"refreshing": building}
                home_settings = self._integration.get_home_settings()
                if not home_settings.show_globally_trending:
                    updates["globally_trending"] = None
                return clone_with_updates(cached, updates)
        # cache miss (first build, restart-wiped cache, or a user whose build is
        # legitimately empty). Back off if a build was attempted within the freshness
        # window so an empty/failed build doesn't rebuild on every 3s poll and hammer
        # upstream APIs; if backed off we report refreshing=false so the UI settles on
        # the empty state instead of polling forever.
        cache_key = self._integration.get_discover_cache_key(
            user_id, resolved_source, lb_enabled, lfm_enabled
        )
        attempted_recently = (
            time.time() - self._built_at.get(cache_key, 0.0) <= STALE_REVALIDATE_SECONDS
        )
        if not building and not attempted_recently:
            self._trigger_warm(user_id, resolved_source)
            building = True
        return DiscoverResponse(
            integration_status=self._integration.get_integration_status(),
            service_prompts=self._build_service_prompts(lb_enabled, lfm_enabled),
            refreshing=building,
        )

    async def get_discover_preview(self, user_id: str) -> DiscoverPreview | None:
        if not self._memory_cache:
            return None
        _, _, _, _, lb_enabled, lfm_enabled, resolved = await self._resolve_user_music(user_id, None)
        cache_key = self._integration.get_discover_cache_key(user_id, resolved, lb_enabled, lfm_enabled)
        cached = await self._memory_cache.get(cache_key)
        if not cached or not isinstance(cached, DiscoverResponse):
            return None
        if not cached.because_you_listen_to:
            return None
        first = cached.because_you_listen_to[0]
        preview_items = [
            item for item in first.section.items[:5]
            if isinstance(item, HomeArtist)
        ]
        return DiscoverPreview(
            seed_artist=first.seed_artist,
            seed_artist_mbid=first.seed_artist_mbid,
            items=preview_items,
        )

    async def refresh_discover_data(self, user_id: str, source: str | None = None) -> None:
        _, _, _, _, lb_enabled, lfm_enabled, resolved = await self._resolve_user_music(user_id, source)
        if (user_id, resolved) in self._building_keys:
            return
        # mark the current cache stale so the next GET's stale-while-revalidate reliably
        # reports refreshing=true (the UI then shows the updating indicator and polls),
        # even when the manual refresh hits already-fresh data
        cache_key = self._integration.get_discover_cache_key(
            user_id, resolved, lb_enabled, lfm_enabled
        )
        self._built_at.pop(cache_key, None)
        self._trigger_warm(user_id, resolved)

    async def warm_cache(self, user_id: str, source: str | None = None) -> None:
        _, _, _, _, lb_enabled, lfm_enabled, resolved = await self._resolve_user_music(user_id, source)
        key = (user_id, resolved)
        if key in self._building_keys:
            return
        self._building_keys.add(key)
        cache_key = self._integration.get_discover_cache_key(
            user_id, resolved, lb_enabled, lfm_enabled
        )
        try:
            response = await self.build_discover_data(user_id, source=resolved)
            if self._memory_cache and self._has_meaningful_content(response):
                await self._memory_cache.set(cache_key, response, DISCOVER_CACHE_TTL)
            else:
                logger.warning("Discover build produced no meaningful content, keeping existing cache")
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to build discover data: {e}")
        finally:
            self._building_keys.discard(key)
            # record the attempt time on every build (success, empty, or failure) so both
            # the stale-while-revalidate window and the cache-miss path back off instead of
            # rebuilding on every poll - including users whose build is legitimately empty
            self._built_at[cache_key] = time.time()

    def _has_meaningful_content(self, response: DiscoverResponse) -> bool:
        return bool(
            response.because_you_listen_to
            or response.fresh_releases
            or response.globally_trending
            or response.artists_you_might_like
            or response.popular_in_your_genres
            or response.missing_essentials
            or response.rediscover
            or response.lastfm_weekly_artist_chart
            or response.lastfm_weekly_album_chart
            or response.lastfm_recent_scrobbles
            or response.weekly_exploration
            or response.daily_mixes
            or response.discover_picks
            or response.radio_sections
            or response.unexplored_genres
        )

    async def build_discover_data(self, user_id: str, source: str | None = None) -> DiscoverResponse:
        (lb_client, lfm_client, username, lfm_username,
         lb_enabled, lfm_enabled, resolved_source) = await self._resolve_user_music(user_id, source)
        home_settings = self._integration.get_home_settings()
        jf_enabled = self._integration.is_jellyfin_enabled()
        library_configured = self._integration.is_library_configured()

        library_mbids = await self._mbid.get_library_artist_mbids(library_configured)

        seed_artists = await self._get_seed_artists(
            lb_enabled, username, jf_enabled,
            resolved_source=resolved_source,
            lfm_enabled=lfm_enabled,
            lfm_username=lfm_username,
            lb_client=lb_client,
        )

        tasks: dict[str, Any] = {}

        for i, seed in enumerate(seed_artists[:3]):
            mbid = seed.artist_mbids[0] if hasattr(seed, 'artist_mbids') and seed.artist_mbids else getattr(seed, 'artist_mbid', None)
            if mbid:
                if resolved_source == "lastfm" and self._lfm_repo and lfm_enabled:
                    tasks[f"similar_{i}"] = self._lfm_repo.get_similar_artists(
                        seed.artist_name, mbid=mbid, limit=20
                    )
                else:
                    tasks[f"similar_{i}"] = self._lb_repo.get_similar_artists(mbid, max_similar=20)

        if home_settings.show_globally_trending:
            if resolved_source == "listenbrainz":
                tasks["lb_trending"] = self._lb_repo.get_sitewide_top_artists(count=20)
            elif resolved_source == "lastfm" and self._lfm_repo and lfm_enabled:
                tasks["lfm_global_top"] = self._lfm_repo.get_global_top_artists(limit=20)

        if self._lfm_repo and lfm_enabled and lfm_username:
            tasks["lfm_weekly_artists"] = self._lfm_repo.get_user_weekly_artist_chart(
                lfm_username
            )
            tasks["lfm_weekly_albums"] = self._lfm_repo.get_user_weekly_album_chart(
                lfm_username
            )
            tasks["lfm_recent"] = self._lfm_repo.get_user_recent_tracks(
                lfm_username, limit=20
            )

        if resolved_source == "listenbrainz" and lb_client and username:
            tasks["lb_fresh"] = lb_client.get_user_fresh_releases()
            tasks["lb_genres"] = lb_client.get_user_genre_activity(username)
        elif resolved_source == "lastfm" and self._lfm_repo and lfm_enabled and lfm_username:
            tasks["lfm_user_top_artists_for_genres"] = self._lfm_repo.get_user_top_artists(
                lfm_username, period="3month", limit=5
            )

        if jf_enabled:
            tasks["jf_most_played"] = self._jf_repo.get_most_played_artists(limit=50)

        if library_configured:
            tasks["library_artists"] = self._library_repo.get_artists_from_library(include_unmonitored=True)
            tasks["library_albums"] = self._library_repo.get_library(include_unmonitored=True)

        results = await self._execute_tasks(tasks)

        response = DiscoverResponse(
            integration_status=self._integration.get_integration_status(),
        )

        seen_artist_mbids: set[str] = set()

        response.because_you_listen_to = self._build_because_sections(
            seed_artists, results, library_mbids, seen_artist_mbids,
            resolved_source=resolved_source,
        )
        await self._enrich_because_sections_audiodb(response.because_you_listen_to)

        response.fresh_releases = self._build_fresh_releases(results, library_mbids)

        post_tasks: dict[str, Any] = {
            "missing_essentials": self._build_missing_essentials(results, library_mbids),
            "lastfm_weekly_album_chart": self._build_lastfm_weekly_album_chart(
                results, library_mbids
            ),
            "lastfm_recent_scrobbles": self._build_lastfm_recent_scrobbles(
                results, library_mbids
            ),
            "daily_mixes": self._build_daily_mix_sections(user_id, resolved_source, library_mbids),
            "discover_picks": self._build_discover_picks(
                user_id, library_mbids, resolved_source, lb_enabled, username,
            ),
            "radio_sections": self._build_radio_sections(
                seed_artists, library_mbids, resolved_source,
            ),
        }
        if resolved_source == "listenbrainz" and lb_client and username:
            post_tasks["weekly_exploration"] = self._weekly_exploration.build_section(
                username, lb_repo=lb_client
            )
        post_results = await self._execute_tasks(post_tasks)
        response.missing_essentials = post_results.get("missing_essentials")
        response.weekly_exploration = post_results.get("weekly_exploration")
        response.daily_mixes = post_results.get("daily_mixes") or []
        response.discover_picks = post_results.get("discover_picks")
        response.radio_sections = post_results.get("radio_sections") or []

        response.rediscover = self._build_rediscover(results, library_mbids, jf_enabled)

        response.artists_you_might_like = self._build_artists_you_might_like(
            seed_artists, results, library_mbids, seen_artist_mbids,
            resolved_source=resolved_source,
        )

        response.popular_in_your_genres = await self._build_popular_in_genres(
            results, library_mbids, seen_artist_mbids,
            resolved_source=resolved_source,
        )

        response.genre_list = self._build_genre_list(results, lb_enabled)

        similar_artist_mbids: list[str] = []
        for i in range(3):
            similar = results.get(f"similar_{i}") or []
            for artist in similar:
                mbid = getattr(artist, 'artist_mbid', None) or getattr(artist, 'mbid', None)
                if mbid:
                    similar_artist_mbids.append(mbid)

        response.unexplored_genres = await self._build_unexplored_genres(
            response.because_you_listen_to, similar_artist_mbids
        )

        if response.genre_list and response.genre_list.items:
            genre_names = [
                g.name for g in response.genre_list.items[:20]
                if isinstance(g, HomeGenre)
            ]
            if genre_names:
                raw_mbids = await asyncio.gather(
                    *(self._get_genre_artist(g) for g in genre_names)
                )
                used_mbids: set[str] = set()
                genre_artists: dict[str, str | None] = {}
                for g, mbid in zip(genre_names, raw_mbids):
                    if mbid and mbid not in used_mbids:
                        genre_artists[g] = mbid
                        used_mbids.add(mbid)
                    elif mbid and mbid in used_mbids:
                        alt = await self._get_genre_artist(g, exclude_mbids=used_mbids)
                        genre_artists[g] = alt
                        if alt:
                            used_mbids.add(alt)
                    else:
                        genre_artists[g] = None
                response.genre_artists = genre_artists
                response.genre_artist_images = await self._resolve_genre_artist_images(
                    response.genre_artists
                )

        if home_settings.show_globally_trending:
            if resolved_source == "lastfm":
                response.globally_trending = self._build_lastfm_globally_trending(
                    results, library_mbids, seen_artist_mbids
                )
            else:
                response.globally_trending = self._build_globally_trending(
                    results, library_mbids, seen_artist_mbids
                )

        response.lastfm_weekly_artist_chart = self._build_lastfm_weekly_artist_chart(
            results, library_mbids, seen_artist_mbids
        )
        response.lastfm_weekly_album_chart = post_results.get("lastfm_weekly_album_chart")
        response.lastfm_recent_scrobbles = post_results.get("lastfm_recent_scrobbles")

        response.service_prompts = self._build_service_prompts(lb_enabled, lfm_enabled)

        return response

    async def build_playlist_suggestions(
        self,
        user_id: str,
        profile: PlaylistProfile,
        count: int = 10,
        source: str | None = None,
    ) -> HomeSection:
        _, _, _, _, lb_enabled, lfm_enabled, resolved_source = await self._resolve_user_music(
            user_id, source
        )
        source_available = (
            (resolved_source == "listenbrainz" and lb_enabled)
            or (resolved_source == "lastfm" and lfm_enabled)
        )
        if not source_available:
            return HomeSection(
                title="Suggestions for your playlist",
                type="albums",
                items=[],
                source=resolved_source,
                fallback_message="The music source you selected isn't set up yet.",
            )

        sample_size = min(3, len(profile.artist_mbids))
        seed_mbids = random.sample(profile.artist_mbids, sample_size)

        if resolved_source == "lastfm" and self._lfm_repo is not None:
            pools = await build_similar_artist_pools_lastfm(
                seed_mbids,
                excluded_mbids=set(profile.artist_mbids),
                similar_limit=15,
                albums_per=3,
                lfm_repo=self._lfm_repo,
                mbid_svc=self._mbid,
            )
        else:
            seeds = [
                ListenBrainzArtist(
                    artist_name=mbid,
                    artist_mbids=[mbid],
                    listen_count=0,
                )
                for mbid in seed_mbids
            ]
            pools = await build_similar_artist_pools(
                seeds,
                excluded_mbids=set(profile.artist_mbids),
                similar_limit=15,
                albums_per=3,
                lb_repo=self._lb_repo,
                mbid_svc=self._mbid,
            )

        if profile.genre_distribution:
            all_genres: list[str] = []
            seen_genres: set[str] = set()
            for genre_list in profile.genre_distribution.values():
                for g in genre_list:
                    gl = g.lower()
                    if gl not in seen_genres:
                        seen_genres.add(gl)
                        all_genres.append(g)
                    if len(all_genres) >= 4:
                        break
                if len(all_genres) >= 4:
                    break
            if all_genres:
                genre_items = await discover_by_genres(
                    all_genres,
                    excluded_mbids=set(profile.artist_mbids),
                    mb_repo=self._mb_repo,
                    mbid_svc=self._mbid,
                )
                if genre_items:
                    pools.append(genre_items)

        selected = round_robin_dedup_select(pools, count)
        albums = [queue_item_to_home_album(item) for item in selected]

        if not albums:
            return HomeSection(
                title="Suggestions for your playlist",
                type="albums",
                items=[],
                source=resolved_source,
                fallback_message="Not enough suggestions for this playlist yet. Try adding more tracks.",
            )

        return HomeSection(
            title="Suggestions for your playlist",
            type="albums",
            items=albums,
            source=resolved_source,
        )

    async def _get_seed_artists(
        self,
        lb_enabled: bool,
        username: str | None,
        jf_enabled: bool,
        resolved_source: str = "listenbrainz",
        lfm_enabled: bool = False,
        lfm_username: str | None = None,
        lb_client: Any = None,
    ) -> list[ListenBrainzArtist]:
        seeds: list[ListenBrainzArtist] = []
        seen_mbids: set[str] = set()

        if resolved_source == "lastfm" and lfm_enabled and lfm_username and self._lfm_repo:
            try:
                lfm_artists = await self._lfm_repo.get_user_top_artists(
                    lfm_username, period="3month", limit=10
                )
                for a in lfm_artists:
                    if len(seeds) >= 3:
                        break
                    mbid = a.mbid
                    if mbid and mbid not in seen_mbids:
                        seeds.append(
                            ListenBrainzArtist(
                                artist_name=a.name,
                                listen_count=a.playcount,
                                artist_mbids=[mbid],
                            )
                        )
                        seen_mbids.add(mbid)
            except Exception as e:  # noqa: BLE001
                logger.warning("Failed to get Last.fm seed artists: %s", e)

        # LB top-artists rely on the client's own identity, so use the per-user client
        seed_lb_repo = lb_client or self._lb_repo
        if resolved_source != "lastfm" and len(seeds) < 3 and lb_enabled and username:
            # fall back to broader windows so a quiet week/month still yields seeds
            for range_ in ("this_week", "this_month", "this_year", "all_time"):
                if len(seeds) >= 3:
                    break
                try:
                    artists = await seed_lb_repo.get_user_top_artists(count=10, range_=range_)
                    for a in artists:
                        if len(seeds) >= 3:
                            break
                        mbid = a.artist_mbids[0] if a.artist_mbids else None
                        if mbid and mbid not in seen_mbids:
                            seeds.append(a)
                            seen_mbids.add(mbid)
                except Exception as e:  # noqa: BLE001
                    logger.warning(f"Failed to get LB top artists ({range_}): {e}")

        if resolved_source != "lastfm" and len(seeds) < 3 and jf_enabled:
            for fetch_fn in (
                lambda: self._jf_repo.get_most_played_artists(limit=10),
                lambda: self._jf_repo.get_favorite_artists(limit=10),
            ):
                if len(seeds) >= 3:
                    break
                try:
                    jf_items = await fetch_fn()
                    for item in jf_items:
                        if len(seeds) >= 3:
                            break
                        mbid = None
                        if item.provider_ids:
                            mbid = item.provider_ids.get("MusicBrainzArtist")
                        if mbid and mbid not in seen_mbids:
                            seeds.append(ListenBrainzArtist(
                                artist_name=item.artist_name or item.name,
                                listen_count=item.play_count,
                                artist_mbids=[mbid],
                            ))
                            seen_mbids.add(mbid)
                except Exception as e:  # noqa: BLE001
                    logger.warning(f"Failed to get Jellyfin seed artists: {e}")
                    continue

        return seeds

    async def _enrich_because_sections_audiodb(
        self, sections: list[BecauseYouListenTo]
    ) -> None:
        if not self._audiodb_image_service:
            return
        for section in sections:
            if not section.seed_artist_mbid:
                continue
            images = await self._audiodb_image_service.get_cached_artist_images(
                section.seed_artist_mbid
            )
            if not images or images.is_negative:
                continue
            section.banner_url = images.banner_url
            section.wide_thumb_url = images.wide_thumb_url
            section.fanart_url = images.fanart_url

    def _build_because_sections(
        self,
        seed_artists: list,
        results: dict[str, Any],
        library_mbids: set[str],
        seen_artist_mbids: set[str],
        resolved_source: str = "listenbrainz",
    ) -> list[BecauseYouListenTo]:
        sections: list[BecauseYouListenTo] = []

        for i, seed in enumerate(seed_artists[:3]):
            similar = results.get(f"similar_{i}")
            if not similar:
                continue

            seed_name = getattr(seed, 'artist_name', 'Unknown')
            seed_mbid = ""
            if hasattr(seed, 'artist_mbids') and seed.artist_mbids:
                seed_mbid = seed.artist_mbids[0]
            elif hasattr(seed, 'artist_mbid'):
                seed_mbid = seed.artist_mbid

            items: list[HomeArtist] = []
            for artist in similar:
                mbid = getattr(artist, 'artist_mbid', None) or getattr(artist, 'mbid', None)
                name = getattr(artist, 'artist_name', None) or getattr(artist, 'name', '')
                listen_count = getattr(artist, 'listen_count', None) or getattr(artist, 'playcount', 0)
                if not mbid:
                    continue
                if mbid.lower() in seen_artist_mbids:
                    continue
                items.append(HomeArtist(
                    mbid=mbid,
                    name=name,
                    listen_count=listen_count,
                    in_library=mbid.lower() in library_mbids,
                ))
                seen_artist_mbids.add(mbid.lower())

            if not items:
                continue

            min_unique = 3
            if len(items) < min_unique and len(sections) > 0:
                continue

            source_label = "lastfm" if resolved_source == "lastfm" else "listenbrainz"
            sections.append(BecauseYouListenTo(
                seed_artist=seed_name,
                seed_artist_mbid=seed_mbid,
                listen_count=getattr(seed, 'listen_count', 0),
                section=HomeSection(
                    title=f"Because You Listen To {seed_name}",
                    type="artists",
                    items=items[:15],
                    source=source_label,
                ),
            ))

        return sections

    async def _build_daily_mix_sections(self, user_id: str, resolved_source: str, library_mbids: set[str]) -> list[HomeSection]:
        # 3-5 genre-clustered daily mixes, 60/40 new-to-familiar ratio
        try:
            if self._genre_index is None:
                return []

            if self._memory_cache:
                cache_key = self._daily_mix_cache_key(user_id, resolved_source)
                cached = await self._memory_cache.get(cache_key)
                if cached is not None:
                    return cached  # type: ignore[return-value]

            top_genres = await self._genre_index.get_top_genres(limit=20)
            if not top_genres:
                await self._cache_daily_mix_result([], user_id, resolved_source)
                return []

            genre_names = [g for g, _ in top_genres[:10]]
            artists_by_genre = await self._genre_index.get_artists_for_genres(genre_names)

            MIN_ARTISTS_PER_CLUSTER = 3
            MAX_CLUSTERS = 5
            candidate_clusters: list[tuple[str, list[str]]] = []
            seen_artists: set[str] = set()
            for genre_lower, _count in top_genres:
                artist_mbids = artists_by_genre.get(genre_lower, [])
                unique = [a for a in artist_mbids if a not in seen_artists]
                if len(unique) < MIN_ARTISTS_PER_CLUSTER:
                    continue
                candidate_clusters.append((genre_lower, unique))
                seen_artists.update(unique)

            candidate_clusters.sort(key=lambda c: len(c[1]), reverse=True)
            clusters = candidate_clusters[:MAX_CLUSTERS]

            if not clusters:
                await self._cache_daily_mix_result([], user_id, resolved_source)
                return []

            sections: list[HomeSection] = []
            for i, (genre_lower, cluster_artists) in enumerate(clusters):
                try:
                    section = await self._build_single_daily_mix(
                        i, genre_lower, cluster_artists, resolved_source, library_mbids,
                    )
                    if section:
                        sections.append(section)
                except Exception as e:  # noqa: BLE001
                    logger.warning(f"Daily mix cluster {i} ({genre_lower}) failed: {e}")
                    continue

            await self._cache_daily_mix_result(sections, user_id, resolved_source)
            return sections

        except Exception as e:  # noqa: BLE001
            logger.warning(f"Daily mix builder failed: {e}")
            return []

    async def _build_single_daily_mix(
        self,
        index: int,
        genre_lower: str,
        cluster_artists: list[str],
        resolved_source: str,
        library_mbids: set[str],
    ) -> HomeSection | None:
        genre_label = genre_lower.title()
        MAX_ITEMS = 12

        seed_count = min(3, len(cluster_artists))
        seed_mbids = random.sample(cluster_artists, seed_count)

        name_results = await asyncio.gather(
            *[
                self._lb_repo.get_artist_top_release_groups(mbid, count=1)
                for mbid in seed_mbids
            ],
            return_exceptions=True,
        )
        seed_names: dict[str, str] = {}
        for mbid, result in zip(seed_mbids, name_results):
            if isinstance(result, Exception) or not result:
                continue
            resolved_name = getattr(result[0], "artist_name", None)
            if resolved_name:
                seed_names[mbid] = resolved_name

        seeds = [
            ListenBrainzArtist(
                artist_mbids=[mbid],
                artist_name=seed_names.get(mbid, f"{genre_label} artist"),
                listen_count=0,
            )
            for mbid in seed_mbids
        ]

        new_items: list[HomeAlbum] = []
        try:
            pools = await build_similar_artist_pools(
                seeds=seeds,
                excluded_mbids=library_mbids,
                similar_limit=10,
                albums_per=3,
                lb_repo=self._lb_repo,
                mbid_svc=self._mbid,
            )
            for pool in pools:
                for item in pool:
                    new_items.append(HomeAlbum(
                        name=item.album_name,
                        mbid=item.release_group_mbid,
                        artist_name=item.artist_name,
                        artist_mbid=item.artist_mbid,
                        image_url=f"/api/v1/covers/release-group/{item.release_group_mbid}?size=500",
                    ))
        except Exception as e:  # noqa: BLE001
            logger.debug(f"Daily mix {index}: similar artist pools failed: {e}")

        familiar_items: list[HomeAlbum] = []
        try:
            library_albums = await self._genre_index.get_albums_by_genre(genre_lower, limit=20)
            for album in library_albums:
                if isinstance(album, dict):
                    mbid = album.get("release_group_mbid", album.get("mbid", ""))
                    familiar_items.append(HomeAlbum(
                        name=album.get("title", album.get("name", "Unknown")),
                        mbid=mbid,
                        artist_name=album.get("artist_name", album.get("artist", "")),
                        artist_mbid=album.get("artist_mbid"),
                        image_url=(
                            f"/api/v1/covers/release-group/{mbid}?size=500" if mbid else None
                        ),
                        in_library=True,
                    ))
        except Exception as e:  # noqa: BLE001
            logger.debug(f"Daily mix {index}: library albums fetch failed: {e}")

        seen_mbids: set[str] = set()
        deduped_new: list[HomeAlbum] = []
        for item in new_items:
            key = item.mbid.lower() if item.mbid else ""
            if key and key not in seen_mbids:
                seen_mbids.add(key)
                deduped_new.append(item)
        new_items = deduped_new

        deduped_familiar: list[HomeAlbum] = []
        for item in familiar_items:
            key = item.mbid.lower() if item.mbid else ""
            if key and key not in seen_mbids:
                seen_mbids.add(key)
                deduped_familiar.append(item)
        familiar_items = deduped_familiar

        new_count = min(len(new_items), round(MAX_ITEMS * 0.6))
        familiar_count = min(len(familiar_items), MAX_ITEMS - new_count)
        if new_count + familiar_count < MAX_ITEMS:
            extra_new = min(len(new_items) - new_count, MAX_ITEMS - new_count - familiar_count)
            if extra_new > 0:
                new_count += extra_new
            extra_familiar = min(
                len(familiar_items) - familiar_count,
                MAX_ITEMS - new_count - familiar_count,
            )
            if extra_familiar > 0:
                familiar_count += extra_familiar

        merged: list[HomeAlbum] = new_items[:new_count] + familiar_items[:familiar_count]
        if not merged:
            return None

        return HomeSection(
            title=f"Daily Mix {index + 1} - {genre_label}",
            type="albums",
            items=merged,
            source=resolved_source,
        )

    async def _cache_daily_mix_result(
        self, sections: list[HomeSection], user_id: str, source: str,
    ) -> None:
        # caches empty lists too (negative cache) so a barren run isn't retried for 24h
        if self._memory_cache:
            cache_key = self._daily_mix_cache_key(user_id, source)
            await self._memory_cache.set(cache_key, sections, DAILY_MIX_CACHE_TTL)

    async def _build_discover_picks(
        self,
        user_id: str,
        library_mbids: set[str],
        resolved_source: str,
        lb_enabled: bool,
        username: str | None,
    ) -> HomeSection | None:
        # genre-affinity-weighted undiscovered albums; library/genre-derived (no
        # external account) so it renders for unlinked users too (D1).
        try:
            if self._genre_index is None:
                return None

            if self._memory_cache is not None:
                cache_key = self._discover_picks_cache_key(user_id, resolved_source)
                cached = await self._memory_cache.get(cache_key)
                if isinstance(cached, dict) and "section" in cached:
                    return cached["section"]  # type: ignore[return-value]

            affinity_weight, count = self._integration.get_discover_picks_settings()

            candidates: list = []
            if resolved_source == "lastfm" and self._lfm_repo is not None:
                try:
                    top_artists = await asyncio.wait_for(
                        self._lfm_repo.get_global_top_artists(limit=30),
                        timeout=30,
                    )
                    valid_artists = [a for a in top_artists if a.mbid]
                    rg_results = await asyncio.gather(
                        *[
                            asyncio.wait_for(
                                self._lb_repo.get_artist_top_release_groups(
                                    artist.mbid, count=3,
                                ),
                                timeout=30,
                            )
                            for artist in valid_artists
                        ],
                        return_exceptions=True,
                    )
                    for result in rg_results:
                        if isinstance(result, Exception):
                            continue
                        candidates.extend(result)
                except asyncio.TimeoutError:
                    logger.warning("Timeout fetching top artists for discover picks")
                except Exception:  # noqa: BLE001
                    pass
            else:
                # sitewide candidates (no account needed) so unlinked users still get picks
                try:
                    candidates = await asyncio.wait_for(
                        self._lb_repo.get_sitewide_top_release_groups(count=100),
                        timeout=30,
                    )
                except asyncio.TimeoutError:
                    logger.warning("Timeout fetching sitewide top release groups for discover picks")
                except Exception:  # noqa: BLE001
                    pass

            if not candidates:
                await self._cache_discover_picks_result(None, user_id, resolved_source)
                return None

            ignored_mbids: set[str] = set()
            if self._mbid_store is not None:
                try:
                    ignored_mbids = await self._mbid_store.get_ignored_release_mbids(user_id)
                except Exception:  # noqa: BLE001
                    logger.warning("Failed to load ignored release MBIDs for discover picks")

            exclude_mbids = library_mbids | ignored_mbids
            filtered = [
                c for c in candidates
                if c.release_group_mbid
                and c.release_group_mbid.lower() not in exclude_mbids
            ]

            if not filtered:
                await self._cache_discover_picks_result(None, user_id, resolved_source)
                return None

            top_genres = await self._genre_index.get_top_genres(limit=10)
            user_genres: set[str] = {g for g, _ in top_genres} if top_genres else set()

            artist_mbids_to_lookup: list[str] = []
            for c in filtered:
                if c.artist_mbids:
                    artist_mbids_to_lookup.append(c.artist_mbids[0])

            genres_by_artist: dict[str, list[str]] = {}
            if artist_mbids_to_lookup:
                genres_by_artist = await self._genre_index.get_genres_for_artists(
                    artist_mbids_to_lookup,
                )

            scored: list[tuple[float, object]] = []
            for c in filtered:
                artist_mbid = c.artist_mbids[0] if c.artist_mbids else None
                candidate_genres = (
                    set(genres_by_artist.get(artist_mbid.lower(), []))
                    if artist_mbid
                    else set()
                )
                genre_overlap = (
                    len(candidate_genres & user_genres) / max(len(user_genres), 1)
                )
                score = affinity_weight * genre_overlap + (1 - affinity_weight) * random.random()
                scored.append((score, c))

            scored.sort(key=lambda x: x[0], reverse=True)
            selected = [c for _, c in scored[:count]]

            items: list[HomeAlbum] = []
            for release in selected:
                try:
                    items.append(
                        self._transformers.lb_release_to_home(release, library_mbids),
                    )
                except Exception:  # noqa: BLE001
                    continue

            if not items:
                await self._cache_discover_picks_result(None, user_id, resolved_source)
                return None

            section = HomeSection(
                title="Discover Picks",
                type="albums",
                items=items,
                source=resolved_source,
            )
            await self._cache_discover_picks_result(section, user_id, resolved_source)
            return section

        except Exception as e:  # noqa: BLE001
            logger.warning(f"Discover picks builder failed: {e}")
            return None

    async def _cache_discover_picks_result(
        self, result: HomeSection | None, user_id: str, source: str,
    ) -> None:
        if self._memory_cache:
            cache_key = self._discover_picks_cache_key(user_id, source)
            await self._memory_cache.set(
                cache_key, {"section": result}, DISCOVER_PICKS_CACHE_TTL,
            )

    def _build_fresh_releases(
        self, results: dict[str, Any], library_mbids: set[str],
    ) -> HomeSection | None:
        releases = results.get("lb_fresh")
        if not releases:
            return None
        items: list[HomeAlbum] = []
        for r in releases[:15]:
            try:
                if isinstance(r, dict):
                    mbid = r.get("release_group_mbid", "")
                    artist_mbids = r.get("artist_mbids", [])
                    in_lib = mbid.lower() in library_mbids if isinstance(mbid, str) and mbid else False
                    items.append(HomeAlbum(
                        mbid=mbid,
                        name=r.get("release_name", r.get("title", "Unknown")),
                        artist_name=r.get("artist_credit_name", r.get("artist_name", "")),
                        artist_mbid=artist_mbids[0] if artist_mbids else None,
                        listen_count=r.get("listen_count"),
                        in_library=in_lib,
                    ))
                else:
                    items.append(self._transformers.lb_release_to_home(r, library_mbids))
            except Exception as e:  # noqa: BLE001
                logger.debug(f"Skipping fresh release item: {e}")
                continue
        if not items:
            return None
        return HomeSection(
            title="Fresh Releases For You",
            type="albums",
            items=items,
            source="listenbrainz",
        )

    async def _build_missing_essentials(
        self, results: dict[str, Any], library_mbids: set[str],
    ) -> HomeSection | None:
        library_artists = results.get("library_artists") or []
        library_albums = results.get("library_albums") or []

        if not library_artists or not library_albums:
            return None

        from collections import Counter
        artist_album_counts: Counter[str] = Counter()
        for album in library_albums:
            artist_mbid = getattr(album, 'artist_mbid', None)
            if artist_mbid:
                artist_album_counts[artist_mbid.lower()] += 1

        library_album_mbids = set()
        for album in library_albums:
            mbid = getattr(album, 'musicbrainz_id', None)
            if mbid:
                library_album_mbids.add(mbid.lower())

        qualifying_artists = [
            (mbid, count) for mbid, count in artist_album_counts.items()
            if count >= MISSING_ESSENTIALS_MIN_ALBUMS
        ]
        qualifying_artists.sort(key=lambda x: -x[1])

        semaphore = asyncio.Semaphore(3)

        async def _fetch_artist_missing(artist_mbid: str) -> list[HomeAlbum]:
            try:
                async with semaphore:
                    top_releases = await self._lb_repo.get_artist_top_release_groups(
                        artist_mbid, count=10
                    )
            except Exception as e:  # noqa: BLE001
                logger.debug(f"Failed to get releases for artist {artist_mbid[:8]}: {e}")
                return []

            artist_missing = 0
            artist_items: list[HomeAlbum] = []
            for rg in top_releases:
                if artist_missing >= MISSING_ESSENTIALS_MAX_PER_ARTIST:
                    break
                rg_mbid = rg.release_group_mbid
                if not rg_mbid or rg_mbid.lower() in library_album_mbids:
                    continue
                artist_items.append(HomeAlbum(
                    mbid=rg_mbid,
                    name=rg.release_group_name,
                    artist_name=rg.artist_name,
                    listen_count=rg.listen_count,
                    in_library=False,
                ))
                artist_missing += 1

            return artist_items

        artist_results = await asyncio.gather(
            *(_fetch_artist_missing(artist_mbid) for artist_mbid, _ in qualifying_artists[:10]),
            return_exceptions=True,
        )

        all_missing: list[HomeAlbum] = []
        for result in artist_results:
            if isinstance(result, Exception):
                logger.debug("Failed to fetch missing essentials batch item: %s", result)
                continue
            all_missing.extend(result)

        if not all_missing:
            return None

        all_missing.sort(key=lambda x: x.listen_count or 0, reverse=True)
        return HomeSection(
            title="Missing Essentials",
            type="albums",
            items=all_missing[:15],
            source="library",
        )

    def _build_rediscover(
        self,
        results: dict[str, Any],
        library_mbids: set[str],
        jf_enabled: bool,
    ) -> HomeSection | None:
        if not jf_enabled:
            return None

        jf_artists = results.get("jf_most_played")
        if not jf_artists:
            return None

        now = datetime.now(timezone.utc)
        rediscover_items: list[HomeArtist] = []
        seen: set[str] = set()

        for item in jf_artists:
            if item.play_count < REDISCOVER_PLAY_THRESHOLD:
                continue
            if not item.last_played:
                continue

            try:
                last_played = datetime.fromisoformat(item.last_played.replace("Z", "+00:00"))
                months_since = (now - last_played).days / 30.0
                if months_since < REDISCOVER_MONTHS_AGO:
                    continue
            except (ValueError, TypeError):
                continue

            artist_name = item.artist_name or item.name
            if artist_name.lower() in seen:
                continue
            seen.add(artist_name.lower())

            mbid = None
            if item.provider_ids:
                mbid = item.provider_ids.get("MusicBrainzArtist")

            image_url = None
            if self._jf_repo and hasattr(self._jf_repo, 'get_image_url'):
                target_id = item.artist_id or item.id
                image_url = prefer_artist_cover_url(
                    mbid,
                    self._jf_repo.get_image_url(target_id, item.image_tag),
                    size=500,
                )

            rediscover_items.append(HomeArtist(
                mbid=mbid,
                name=artist_name,
                listen_count=item.play_count,
                image_url=image_url,
                in_library=mbid.lower() in library_mbids if mbid else False,
            ))

            if len(rediscover_items) >= 15:
                break

        if not rediscover_items:
            return None

        return HomeSection(
            title="Rediscover",
            type="artists",
            items=rediscover_items,
            source="jellyfin",
        )

    def _build_artists_you_might_like(
        self,
        seed_artists: list,
        results: dict[str, Any],
        library_mbids: set[str],
        seen_artist_mbids: set[str],
        resolved_source: str = "listenbrainz",
    ) -> HomeSection | None:
        aggregated: list[HomeArtist] = []
        for i in range(len(seed_artists[:3])):
            similar = results.get(f"similar_{i}")
            if not similar:
                continue
            for artist in similar:
                mbid = getattr(artist, 'artist_mbid', None) or getattr(artist, 'mbid', None)
                name = getattr(artist, 'artist_name', None) or getattr(artist, 'name', '')
                listen_count = getattr(artist, 'listen_count', None) or getattr(artist, 'playcount', 0)
                if not mbid:
                    continue
                if mbid.lower() in seen_artist_mbids:
                    continue
                aggregated.append(HomeArtist(
                    mbid=mbid,
                    name=name,
                    listen_count=listen_count,
                    in_library=mbid.lower() in library_mbids,
                ))
                seen_artist_mbids.add(mbid.lower())

        if not aggregated:
            return None

        aggregated.sort(key=lambda x: x.listen_count or 0, reverse=True)
        source_label = "lastfm" if resolved_source == "lastfm" else "listenbrainz"
        return HomeSection(
            title="Artists You Might Like",
            type="artists",
            items=aggregated[:15],
            source=source_label,
        )

    async def _build_popular_in_genres(
        self,
        results: dict[str, Any],
        library_mbids: set[str],
        seen_artist_mbids: set[str],
        resolved_source: str = "listenbrainz",
    ) -> HomeSection | None:
        if resolved_source == "lastfm" and self._lfm_repo:
            return await self._build_popular_in_genres_lastfm(
                results, library_mbids, seen_artist_mbids
            )

        genres = results.get("lb_genres")

        if not genres:
            return None
        else:
            genre_names = []
            for genre in genres[:3]:
                name = genre.genre if hasattr(genre, 'genre') else str(genre)
                genre_names.append(name)

        all_artists: list[HomeArtist] = []
        tag_results = await asyncio.gather(
            *(self._mb_repo.search_artists_by_tag(genre_name, limit=10) for genre_name in genre_names),
            return_exceptions=True,
        )

        for genre_name, tag_artists in zip(genre_names, tag_results):
            if isinstance(tag_artists, Exception):
                logger.debug(f"Failed to search artists for genre '{genre_name}': {tag_artists}")
                continue
            for artist in tag_artists:
                if artist is None:
                    continue
                mbid = artist.musicbrainz_id
                if not mbid or mbid.lower() in seen_artist_mbids:
                    continue
                all_artists.append(HomeArtist(
                    mbid=mbid,
                    name=artist.title if hasattr(artist, 'title') else str(artist),
                    in_library=mbid.lower() in library_mbids,
                ))
                seen_artist_mbids.add(mbid.lower())

        if not all_artists:
            return None

        return HomeSection(
            title="Popular In Your Genres",
            type="artists",
            items=all_artists[:15],
            source="musicbrainz",
        )

    async def _build_popular_in_genres_lastfm(
        self,
        results: dict[str, Any],
        library_mbids: set[str],
        seen_artist_mbids: set[str],
    ) -> HomeSection | None:
        top_artists = results.get("lfm_user_top_artists_for_genres") or []
        if not top_artists or not self._lfm_repo:
            return None

        artist_info_results = await asyncio.gather(
            *(
                self._lfm_repo.get_artist_info(artist.name, mbid=artist.mbid)
                for artist in top_artists[:5]
            ),
            return_exceptions=True,
        )

        genre_names: list[str] = []
        seen_genres: set[str] = set()
        for info in artist_info_results:
            if isinstance(info, Exception):
                logger.debug("Failed to get artist info for genre extraction: %s", info)
                continue
            if info and info.tags:
                for tag in info.tags[:2]:
                    if tag.name and tag.name.lower() not in seen_genres:
                        genre_names.append(tag.name)
                        seen_genres.add(tag.name.lower())
                        if len(genre_names) >= 3:
                            break
            if len(genre_names) >= 3:
                break

        if not genre_names:
            return None

        tag_top_artist_results = await asyncio.gather(
            *(
                self._lfm_repo.get_tag_top_artists(genre_name, limit=10)
                for genre_name in genre_names
            ),
            return_exceptions=True,
        )

        all_artists: list[HomeArtist] = []
        for genre_name, tag_artists in zip(genre_names, tag_top_artist_results):
            if isinstance(tag_artists, Exception):
                logger.debug("Failed to get tag top artists for '%s': %s", genre_name, tag_artists)
                continue
            for artist in tag_artists:
                mbid = artist.mbid
                if not mbid or mbid.lower() in seen_artist_mbids:
                    continue
                all_artists.append(HomeArtist(
                    mbid=mbid,
                    name=artist.name,
                    listen_count=artist.playcount,
                    in_library=mbid.lower() in library_mbids,
                ))
                seen_artist_mbids.add(mbid.lower())

        if not all_artists:
            return None

        return HomeSection(
            title="Popular In Your Genres",
            type="artists",
            items=all_artists[:15],
            source="lastfm",
        )

    def _build_genre_list(
        self, results: dict[str, Any], lb_enabled: bool
    ) -> HomeSection | None:
        lb_genres = results.get("lb_genres")
        library_albums = results.get("library_albums") or []
        genres = self._transformers.extract_genres_from_library(library_albums, lb_genres)
        if not genres:
            return None
        source = "listenbrainz" if lb_genres else ("library" if library_albums else None)
        return HomeSection(title="Browse by Genre", type="genres", items=genres, source=source)

    async def _build_unexplored_genres(
        self,
        because_sections: list[BecauseYouListenTo],
        similar_artist_mbids: list[str],
    ) -> HomeSection | None:
        if self._genre_index is None:
            return None
        try:
            candidate_mbids: set[str] = set()
            for section in because_sections:
                for item in section.section.items:
                    if isinstance(item, HomeArtist) and item.mbid:
                        candidate_mbids.add(item.mbid)
            for mbid in similar_artist_mbids:
                candidate_mbids.add(mbid)

            genres_by_artist = await self._genre_index.get_genres_for_artists(list(candidate_mbids))
            candidate_genres: dict[str, str] = {}
            for _artist, genre_list in genres_by_artist.items():
                for display_name in genre_list:
                    lower = display_name.lower()
                    if lower not in candidate_genres:
                        candidate_genres[lower] = display_name

            if candidate_genres:
                counts = await self._genre_index.get_genre_artist_counts(list(candidate_genres.values()))
            else:
                counts = {}

            top_genres_raw = await self._genre_index.get_top_genres(limit=20)
            top_genre_lowers = {g.lower() for g, _ in top_genres_raw}

            filtered: list[tuple[str, str, int]] = []
            for lower, display in candidate_genres.items():
                count = counts.get(lower, 0)
                if count >= UNEXPLORED_GENRES_THRESHOLD:
                    continue
                if lower in top_genre_lowers:
                    continue
                filtered.append((lower, display, count))

            random.shuffle(filtered)
            filtered = filtered[:UNEXPLORED_GENRES_MAX]

            if not filtered:
                top_genre_names = [g for g, _ in top_genres_raw]
                fallback = await self._genre_index.get_underrepresented_genres(
                    top_genre_names, threshold=UNEXPLORED_GENRES_THRESHOLD
                )
                random.shuffle(fallback)
                fallback = fallback[:UNEXPLORED_GENRES_MAX]
                if not fallback:
                    return None
                fallback_counts = await self._genre_index.get_genre_artist_counts(fallback)
                genre_items: list[HomeGenre] = [
                    HomeGenre(name=g.title(), artist_count=fallback_counts.get(g, 0))
                    for g in fallback
                ]
            else:
                genre_items = [
                    HomeGenre(name=display, artist_count=count)
                    for _lower, display, count in filtered
                ]

            if not genre_items:
                return None

            return HomeSection(
                title="Genres to Explore",
                type="genres",
                items=genre_items,
                source=None,
            )
        except Exception as e:  # noqa: BLE001
            logger.warning(f"Unexplored genres builder failed: {e}")
            return None

    def _build_globally_trending(
        self,
        results: dict[str, Any],
        library_mbids: set[str],
        seen_artist_mbids: set[str],
    ) -> HomeSection | None:
        artists = results.get("lb_trending") or []
        items = []
        for artist in artists[:20]:
            home_artist = self._transformers.lb_artist_to_home(artist, library_mbids)
            if home_artist and home_artist.mbid and home_artist.mbid.lower() not in seen_artist_mbids:
                items.append(home_artist)
                seen_artist_mbids.add(home_artist.mbid.lower())

        if not items:
            return None

        return HomeSection(
            title="Globally Trending",
            type="artists",
            items=items[:15],
            source="listenbrainz",
        )

    def _build_lastfm_globally_trending(
        self,
        results: dict[str, Any],
        library_mbids: set[str],
        seen_artist_mbids: set[str],
    ) -> HomeSection | None:
        artists = results.get("lfm_global_top") or []
        items = []
        for artist in artists[:20]:
            home_artist = self._transformers.lastfm_artist_to_home(artist, library_mbids)
            if home_artist and home_artist.mbid and home_artist.mbid.lower() not in seen_artist_mbids:
                items.append(home_artist)
                seen_artist_mbids.add(home_artist.mbid.lower())

        if not items:
            return None

        return HomeSection(
            title="Globally Trending",
            type="artists",
            items=items[:15],
            source="lastfm",
        )

    def _build_lastfm_weekly_artist_chart(
        self,
        results: dict[str, Any],
        library_mbids: set[str],
        seen_artist_mbids: set[str],
    ) -> HomeSection | None:
        artists = results.get("lfm_weekly_artists") or []
        items = []
        for artist in artists[:20]:
            home_artist = self._transformers.lastfm_artist_to_home(artist, library_mbids)
            if home_artist and home_artist.mbid and home_artist.mbid.lower() not in seen_artist_mbids:
                items.append(home_artist)
                seen_artist_mbids.add(home_artist.mbid.lower())

        if not items:
            return None

        return HomeSection(
            title="Your Weekly Top Artists",
            type="artists",
            items=items[:15],
            source="lastfm",
        )

    async def _build_lastfm_weekly_album_chart(
        self,
        results: dict[str, Any],
        library_mbids: set[str],
    ) -> HomeSection | None:
        albums = results.get("lfm_weekly_albums") or []
        if not albums:
            return None

        release_mbids = list({a.mbid for a in albums[:20] if a.mbid})
        rg_map = await self._resolve_release_mbids(release_mbids) if release_mbids else {}

        items = []
        for album in albums[:20]:
            home_album = self._transformers.lastfm_album_to_home(album, library_mbids)
            if home_album and home_album.mbid:
                home_album.mbid = rg_map.get(home_album.mbid, home_album.mbid)
                items.append(home_album)

        if not items:
            return None

        return HomeSection(
            title="Your Top Albums This Week",
            type="albums",
            items=items[:15],
            source="lastfm",
        )

    async def _build_lastfm_recent_scrobbles(
        self,
        results: dict[str, Any],
        library_mbids: set[str],
    ) -> HomeSection | None:
        tracks = results.get("lfm_recent") or []
        if not tracks:
            return None

        release_mbids = list({t.album_mbid for t in tracks[:30] if t.album_mbid})
        rg_map = await self._resolve_release_mbids(release_mbids) if release_mbids else {}

        items = []
        seen_album_mbids: set[str] = set()
        for track in tracks[:30]:
            home_album = self._transformers.lastfm_recent_to_home(track, library_mbids)
            if home_album and home_album.mbid:
                resolved = rg_map.get(home_album.mbid, home_album.mbid)
                home_album.mbid = resolved
                if resolved.lower() not in seen_album_mbids:
                    items.append(home_album)
                    seen_album_mbids.add(resolved.lower())

        if not items:
            return None

        return HomeSection(
            title="Recently Scrobbled",
            type="albums",
            items=items[:15],
            source="lastfm",
        )

    async def _resolve_release_mbids(self, release_ids: list[str]) -> dict[str, str]:
        if not release_ids:
            return {}
        unique_ids = list(dict.fromkeys(release_ids))
        tasks = [self._mb_repo.get_release_group_id_from_release(rid) for rid in unique_ids]
        results = await asyncio.gather(*tasks, return_exceptions=True)
        rg_map: dict[str, str] = {}
        for rid, rg_id in zip(unique_ids, results):
            if isinstance(rg_id, str) and rg_id:
                rg_map[rid] = rg_id
        return rg_map

    async def _get_genre_artist(self, genre_name: str, exclude_mbids: set[str] | None = None) -> str | None:
        try:
            artists = await self._mb_repo.search_artists_by_tag(genre_name, limit=10)
            for artist in artists:
                if not artist.musicbrainz_id or artist.musicbrainz_id == VARIOUS_ARTISTS_MBID:
                    continue
                if exclude_mbids and artist.musicbrainz_id in exclude_mbids:
                    continue
                return artist.musicbrainz_id
        except Exception:  # noqa: BLE001
            logger.debug("Failed to resolve genre artist from library")
        return None

    async def _resolve_genre_artist_images(
        self, genre_artists: dict[str, str | None]
    ) -> dict[str, str | None]:
        if not self._audiodb_image_service or not genre_artists:
            return {}

        sem = asyncio.Semaphore(5)

        async def _resolve_one(genre: str, mbid: str) -> tuple[str, str | None]:
            async with sem:
                try:
                    images = await self._audiodb_image_service.fetch_and_cache_artist_images(mbid)
                    if images and not images.is_negative:
                        url = images.wide_thumb_url or images.banner_url or images.fanart_url
                        if url:
                            return (genre, url)
                except Exception as exc:  # noqa: BLE001
                    logger.debug("Failed to resolve discover genre image for %s: %s", genre, exc)
                return (genre, None)

        tasks = [
            _resolve_one(genre, mbid)
            for genre, mbid in genre_artists.items()
            if mbid
        ]
        if not tasks:
            return {}

        results = await asyncio.gather(*tasks)
        return {genre: url for genre, url in results if url}

    def _build_service_prompts(self, lb_enabled: bool, lfm_enabled: bool) -> list[ServicePrompt]:
        # LB/Last.fm prompts are per-user; jellyfin/download-client stay global (D2)
        prompts = []
        if not lb_enabled:
            prompts.append(ServicePrompt(
                service="listenbrainz",
                title="Connect ListenBrainz",
                description="Pulls recommendations from your listening history, finds similar artists, and tracks your top genres. Add Last.fm for global listener stats.",
                icon="LB",
                color="primary",
                features=["Personalized recommendations", "Similar artists", "Listening stats", "Genre insights"],
            ))
        if not self._integration.is_jellyfin_enabled():
            prompts.append(ServicePrompt(
                service="jellyfin",
                title="Connect Jellyfin",
                description="Uses your play history to bring back old favorites and improve recommendations.",
                icon="JF",
                color="secondary",
                features=["Rediscover favorites", "Play statistics", "Listening history", "Better recommendations"],
            ))
        if not self._integration.is_download_client_configured():
            prompts.append(ServicePrompt(
                service="download-client",
                title="Connect Download Client",
                description="Lets you request and download albums and tracks straight into your library.",
                icon="DL",
                color="accent",
                features=["Album requests", "Track requests", "Automatic import", "Library management"],
            ))
        if not lfm_enabled:
            prompts.append(ServicePrompt(
                service="lastfm",
                title="Connect Last.fm",
                description="Tracks what you listen to, shows your stats, and suggests music based on your taste.",
                icon="FM",
                color="primary",
                features=["Scrobbling", "Global listener stats", "Artist recommendations", "Play history"],
            ))
        return prompts

    async def _execute_tasks(self, tasks: dict[str, Any]) -> dict[str, Any]:
        if not tasks:
            return {}
        keys = list(tasks.keys())
        # bound each upstream call so one slow/hanging service can't stall the whole
        # build (a timeout is caught below and that section is just dropped)
        coros = [asyncio.wait_for(c, timeout=DISCOVER_TASK_TIMEOUT_SECONDS) for c in tasks.values()]
        raw_results = await asyncio.gather(*coros, return_exceptions=True)
        results = {}
        for key, result in zip(keys, raw_results):
            if isinstance(result, Exception):
                logger.warning(f"Discover task {key} failed: {result}")
                results[key] = None
            else:
                results[key] = result
        return results

    async def _build_radio_sections(
        self,
        seed_artists: list[ListenBrainzArtist],
        library_mbids: set[str],
        source: str,
    ) -> list[HomeSection]:
        valid_seeds = [
            seed for seed in seed_artists[:3]
            if seed.artist_mbids
        ]
        if not valid_seeds:
            return []

        async def _build_one(seed: ListenBrainzArtist) -> HomeSection | None:
            seed_mbid = seed.artist_mbids[0]
            try:
                pools = await asyncio.wait_for(
                    build_similar_artist_pools(
                        [seed],
                        excluded_mbids=library_mbids,
                        similar_limit=15,
                        albums_per=3,
                        lb_repo=self._lb_repo,
                        mbid_svc=self._mbid,
                    ),
                    timeout=30,
                )
                selected = round_robin_dedup_select(pools, count=10)
                albums = [queue_item_to_home_album(item) for item in selected]
                return HomeSection(
                    title=f"Radio: {seed.artist_name}",
                    type="albums",
                    items=albums,
                    source=source,
                    radio_seed_type="artist",
                    radio_seed_id=seed_mbid,
                )
            except asyncio.TimeoutError:
                logger.warning("Radio section for seed %s timed out", seed_mbid[:8])
                return None
            except Exception as e:  # noqa: BLE001
                logger.warning("Radio section for seed %s failed: %s", seed_mbid[:8], e)
                return None

        results = await asyncio.gather(*[_build_one(seed) for seed in valid_seeds])
        return [s for s in results if s is not None]
