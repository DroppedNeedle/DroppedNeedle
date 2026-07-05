import asyncio
import hashlib
import logging
import random
from types import SimpleNamespace
import time
from datetime import datetime, timezone
from typing import Any

from api.v1.schemas.discover import (
    DiscoverResponse,
    BecauseYouListenTo,
    PlaylistProfile,
    TopPickItem,
    TopPicksSection,
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
from services.discover.mbid_resolution_service import MbidResolutionService, discover_build_thorough
from services.discover.queue_strategies import build_similar_artist_pools, build_similar_artist_pools_lastfm, discover_by_genres, queue_item_to_home_album, round_robin_dedup_select
from repositories.listenbrainz_repository import lb_popularity_degraded
from services.discover.top_picks import TopPickCandidate, score_candidates
from services.weekly_exploration_service import WeeklyExplorationService

logger = logging.getLogger(__name__)


def _log_task_error(task: "asyncio.Task[None]") -> None:
    if task.cancelled():
        return
    exc = task.exception()
    if exc is not None:
        logger.error("Discover background task failed: %s", exc, exc_info=exc)


# Ceilings on how many covers one prewarm pass fetches, so a huge grid can't turn a warm
# cycle into an unbounded background crawl. First-seen order (top picks, then rows top-to-
# bottom) means the covers most likely on screen warm first.
_PREWARM_MAX_ALBUMS = 120
_PREWARM_MAX_ARTISTS = 60


def _collect_cover_prewarm_mbids(response: DiscoverResponse) -> tuple[list[str], list[str]]:
    """Album and artist MBIDs across every visible Discover section, de-duped in first-seen
    (visual-priority) order. Tracks/genres carry no cover of their own and are skipped."""
    album_mbids: list[str] = []
    artist_mbids: list[str] = []
    seen_albums: set[str] = set()
    seen_artists: set[str] = set()

    def add_album(mbid: str | None) -> None:
        if mbid and mbid not in seen_albums:
            seen_albums.add(mbid)
            album_mbids.append(mbid)

    def add_artist(mbid: str | None) -> None:
        if mbid and mbid not in seen_artists:
            seen_artists.add(mbid)
            artist_mbids.append(mbid)

    def walk_section(section: HomeSection | None) -> None:
        if section is None:
            return
        for item in section.items:
            if isinstance(item, HomeAlbum):
                add_album(item.mbid)
            elif isinstance(item, HomeArtist):
                add_artist(item.mbid)

    if response.top_picks:
        for pick in response.top_picks.items:
            add_album(pick.album.mbid)
    for bylt in response.because_you_listen_to:
        add_artist(bylt.seed_artist_mbid)
        walk_section(bylt.section)
    for section in (
        response.fresh_releases,
        response.missing_essentials,
        response.rediscover,
        response.artists_you_might_like,
        response.popular_in_your_genres,
        response.globally_trending,
        response.lastfm_weekly_artist_chart,
        response.lastfm_weekly_album_chart,
        response.lastfm_recent_scrobbles,
        response.listeners_like_you,
        response.anniversaries,
        response.new_from_followed,
        response.unexplored_genres,
        response.genre_list,
    ):
        walk_section(section)
    for section in response.daily_mixes:
        walk_section(section)
    for section in response.radio_sections:
        walk_section(section)
    # Weekly Exploration is a bespoke shape (tracks, not HomeSection items) but its rows still
    # render album + artist covers through our endpoint, so warm those too.
    if response.weekly_exploration is not None:
        for track in response.weekly_exploration.tracks:
            add_album(track.release_group_mbid)
            add_artist(track.artist_mbid)

    return album_mbids[:_PREWARM_MAX_ALBUMS], artist_mbids[:_PREWARM_MAX_ARTISTS]


DISCOVER_CACHE_TTL = 43200  # 12 hours
# stale-while-revalidate: serve a cached response immediately but rebuild it in the
# background once it's older than this. Kept comfortably above a build's duration so
# active use can't trigger back-to-back rebuilds (external API rate limits). Tunable.
STALE_REVALIDATE_SECONDS = 300  # 5 minutes
# cap any single upstream call within a build so one slow service can't hang the update
DISCOVER_TASK_TIMEOUT_SECONDS = 25
# Inner budgets for the Last.fm->MusicBrainz release-group resolution that every section
# falls back to during a ListenBrainz-popularity outage. MB is a hard 1 req/s, so with
# every section resolving at once the sections mutually starve and hit the 25s task
# timeout. Bounding each MB-heavy pool well under 25s lets the section degrade gracefully
# (Top Picks -> MB-free trending; the Last.fm chart/scrobble sections -> raw release mbids)
# instead of the whole task being cancelled mid-resolution and vanishing.
TOP_PICKS_SIMILARITY_BUDGET_SECONDS = 12
DISCOVER_MB_RESOLVE_BUDGET_SECONDS = 15
# Per-build multiplier on every section budget below. Defaults to 1.0 (on-visit builds stay
# tightly budgeted so the page paints fast). The background warmer sets it high via a
# ContextVar so its build runs effectively unbudgeted - the Last.fm->MusicBrainz resolution
# gets the TIME it needs to complete and (with the incremental persist) bank exactly the
# albums each section uses, so the next on-visit build is cheap and fully personalised. It's
# a ContextVar (per-task), so a concurrent on-visit build on the same @singleton is unaffected.
PREWARM_BUDGET_SCALE = 40.0


def _scaled(base: float) -> float:
    """A section budget, relaxed PREWARM_BUDGET_SCALE-fold during a thorough (warmer) build so
    the rate-limited MusicBrainz resolution can complete; unchanged for on-visit builds."""
    return base * PREWARM_BUDGET_SCALE if discover_build_thorough.get() else base
# Per-radio-station pool budget. MUST stay under DISCOVER_TASK_TIMEOUT_SECONDS or the
# outer task timeout cancels the whole radio_sections task (a hard failure) before this
# inner one can fire and degrade a slow station to empty. The stations run concurrently,
# so wall-clock is ~this value, not N times it.
RADIO_POOL_BUDGET_SECONDS = 18
REDISCOVER_PLAY_THRESHOLD = 5
REDISCOVER_MONTHS_AGO = 3
MISSING_ESSENTIALS_MIN_ALBUMS = 3
MISSING_ESSENTIALS_MAX_PER_ARTIST = 3
VARIOUS_ARTISTS_MBID = "89ad4ac3-39f7-470e-963a-56509c546377"
DAILY_MIX_CACHE_TTL = 86400  # 24 hours
DISCOVER_PICKS_CACHE_TTL = 14400  # 4 hours
# When Top Picks had to fall back to trending-only (LB popularity degraded AND the
# Last.fm->MusicBrainz personalisation timed out), cache it only briefly so the next
# build retries: the MB resolutions warm across builds, so personalised picks converge
# instead of a cold trending-only result being frozen for the full 4h.
DISCOVER_PICKS_DEGRADED_TTL = 300  # 5 minutes
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
        library_db: Any = None,
        follow_service: Any = None,
        cover_repo: Any = None,
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
        self._library_db = library_db
        self._follow_service = follow_service
        self._cover_repo = cover_repo
        self._transformers = HomeDataTransformers(jellyfin_repo)
        self._weekly_exploration = WeeklyExplorationService(listenbrainz_repo, musicbrainz_repo)
        # per-user in-flight warm guard so one user never blocks another
        self._building_keys: set[str] = set()
        # cache_key -> unix time of the last successful build, for stale-while-revalidate
        self._built_at: dict[str, float] = {}
        # task keys that failed in the most recent _execute_tasks call
        self._last_failed_task_keys: set[str] = set()

    @staticmethod
    def _daily_rng(*parts: str) -> random.Random:
        """Deterministic-per-day randomness: a refresh within the same day
        reproduces the same sections instead of reshuffling under the user."""
        today = datetime.now(timezone.utc).date().isoformat()
        seed = hashlib.md5(":".join([today, *parts]).encode()).hexdigest()
        return random.Random(int(seed[:12], 16))

    def _daily_mix_cache_key(self, user_id: str, source: str) -> str:
        today = datetime.now(timezone.utc).date().isoformat()
        return f"daily_mix:{user_id}:{source}:{today}"

    def _top_picks_cache_key(self, user_id: str, source: str) -> str:
        return f"top_picks:{user_id}:{source}"

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

    def _trigger_warm(self, user_id: str) -> None:
        """Kick off a background rebuild for the user if one isn't already running."""
        from core.task_registry import TaskRegistry

        registry = TaskRegistry.get_instance()
        task_name = f"discover-homepage-warm-{user_id}"
        if registry.is_running(task_name):
            return
        task = asyncio.create_task(self.warm_cache(user_id))
        try:
            registry.register(task_name, task)
        except RuntimeError:
            pass

    async def get_discover_data(self, user_id: str) -> DiscoverResponse:
        _, _, _, _, lb_enabled, lfm_enabled, _ = await self._resolve_user_music(user_id, None)
        building = user_id in self._building_keys
        if self._memory_cache:
            cache_key = self._integration.get_discover_cache_key(
                user_id, lb_enabled, lfm_enabled
            )
            cached = await self._memory_cache.get(cache_key)
            if cached is not None and isinstance(cached, DiscoverResponse):
                # stale-while-revalidate: serve the cached copy immediately, but rebuild
                # in the background once it's older than the freshness window so the data
                # always converges to fresh without ever showing the build screen again
                age = time.time() - self._built_at.get(cache_key, 0.0)
                if not building and age > STALE_REVALIDATE_SECONDS:
                    self._trigger_warm(user_id)
                    building = True
                return clone_with_updates(cached, {"refreshing": building})
        # cache miss (first build, restart-wiped cache, or a user whose build is
        # legitimately empty). Back off if a build was attempted within the freshness
        # window so an empty/failed build doesn't rebuild on every 3s poll and hammer
        # upstream APIs; if backed off we report refreshing=false so the UI settles on
        # the empty state instead of polling forever.
        cache_key = self._integration.get_discover_cache_key(
            user_id, lb_enabled, lfm_enabled
        )
        attempted_recently = (
            time.time() - self._built_at.get(cache_key, 0.0) <= STALE_REVALIDATE_SECONDS
        )
        if not building and not attempted_recently:
            self._trigger_warm(user_id)
            building = True
        return DiscoverResponse(
            integration_status=self._integration.get_integration_status(),
            service_prompts=self._build_service_prompts(lb_enabled, lfm_enabled),
            refreshing=building,
        )

    async def get_discover_preview(self, user_id: str) -> DiscoverPreview | None:
        if not self._memory_cache:
            return None
        _, _, _, _, lb_enabled, lfm_enabled, _ = await self._resolve_user_music(user_id, None)
        cache_key = self._integration.get_discover_cache_key(user_id, lb_enabled, lfm_enabled)
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

    async def peek_freshness(self, user_id: str) -> tuple[bool, bool]:
        """Read-only warmer signal: (has_cache, still_converging). Never triggers a build.
        still_converging is True when the cached discover is trending-only
        (top_picks.personalizing) OR - while LB popularity is degraded - has no top_picks at
        all yet (the both-pools-empty degraded case caches top_picks=None), so the warmer
        keeps re-warming until real personalised picks land instead of giving up for 6h."""
        if not self._memory_cache:
            return (False, False)
        _, _, _, _, lb_enabled, lfm_enabled, _ = await self._resolve_user_music(user_id, None)
        cache_key = self._integration.get_discover_cache_key(user_id, lb_enabled, lfm_enabled)
        cached = await self._memory_cache.get(cache_key)
        if not cached or not isinstance(cached, DiscoverResponse):
            return (False, False)
        tp = cached.top_picks
        still_converging = bool(tp and tp.personalizing) or (
            tp is None and self._use_lastfm_for_popularity(lfm_enabled)
        )
        return (True, still_converging)

    async def warm_cache_thorough(self, user_id: str) -> None:
        """Background-warmer build. Runs the REAL discover build in THOROUGH mode: every section
        budget relaxed AND all album->release-group lookups resolved (not capped at max_lookups),
        so during a ListenBrainz-popularity outage the Last.fm->MusicBrainz resolution completes
        and Top Picks FULLY personalises in one pass (banked durably via the incremental persist).
        Harmless when LB is healthy (sections use LB directly and finish fast).

        First it PROBES LB popularity, so the degraded gate reflects reality NOW before the section
        builders choose their path. Without this, the first build after an idle gap (the gate's TTL
        expired with no call to re-mark it) would take the stale LB path, 500, and cache a
        trending-only result."""
        (lb_client, _lfm, username, lfm_username, lb_enabled, lfm_enabled, primary) = (
            await self._resolve_user_music(user_id, None)
        )
        try:
            seeds = await self._get_seed_artists(
                lb_enabled, username, self._integration.is_jellyfin_enabled(),
                resolved_source=primary, lfm_enabled=lfm_enabled,
                lfm_username=lfm_username, lb_client=lb_client,
            )
            seed_mbid = next(
                (s.artist_mbids[0] for s in seeds if getattr(s, "artist_mbids", None)), None
            )
            if seed_mbid:
                # side effect is the point: a 500 marks the gate degraded, a 200 heals it
                await self._lb_repo.get_artist_top_release_groups(seed_mbid, count=1)
        except Exception:  # noqa: BLE001 - probe is best-effort; its gate side effect already fired
            pass

        token = discover_build_thorough.set(True)
        try:
            await self.warm_cache(user_id)
        finally:
            discover_build_thorough.reset(token)

    async def refresh_discover_data(self, user_id: str) -> None:
        _, _, _, _, lb_enabled, lfm_enabled, _ = await self._resolve_user_music(user_id, None)
        if user_id in self._building_keys:
            return
        # mark the current cache stale so the next GET's stale-while-revalidate reliably
        # reports refreshing=true (the UI then shows the updating indicator and polls),
        # even when the manual refresh hits already-fresh data
        cache_key = self._integration.get_discover_cache_key(
            user_id, lb_enabled, lfm_enabled
        )
        self._built_at.pop(cache_key, None)
        self._trigger_warm(user_id)

    async def warm_cache(self, user_id: str) -> None:
        _, _, _, _, lb_enabled, lfm_enabled, _ = await self._resolve_user_music(user_id, None)
        if user_id in self._building_keys:
            return
        self._building_keys.add(user_id)
        cache_key = self._integration.get_discover_cache_key(
            user_id, lb_enabled, lfm_enabled
        )
        try:
            response = await self.build_discover_data(user_id)
            if self._memory_cache and self._has_meaningful_content(response):
                await self._memory_cache.set(cache_key, response, DISCOVER_CACHE_TTL)
                self._spawn_cover_prewarm(user_id, response)
            else:
                logger.warning("Discover build produced no meaningful content, keeping existing cache")
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to build discover data: {e}")
        finally:
            self._building_keys.discard(user_id)
            # record the attempt time on every build (success, empty, or failure) so both
            # the stale-while-revalidate window and the cache-miss path back off instead of
            # rebuilding on every poll - including users whose build is legitimately empty
            self._built_at[cache_key] = time.time()

    def _spawn_cover_prewarm(self, user_id: str, response: DiscoverResponse) -> None:
        """Warm covers for the WHOLE Discover grid (albums + artists) so the page paints from
        disk instead of a burst of cold, rate-limited external fetches on the user's next visit.
        BACKGROUND_SYNC keeps live user requests ahead of it, and the proactive warmer drives
        this ahead of the visit entirely - the single biggest lever against cold cover grids."""
        if self._cover_repo is None:
            return
        album_mbids, artist_mbids = _collect_cover_prewarm_mbids(response)
        # The Top Picks featured card renders at 500; every grid renders at 250. Warming both
        # sizes for the picks (a handful) and 250 for the rest matches what the UI requests.
        top_pick_mbids = (
            [i.album.mbid for i in response.top_picks.items if i.album.mbid]
            if response.top_picks
            else []
        )
        if not album_mbids and not artist_mbids:
            return
        from core.task_registry import TaskRegistry

        registry = TaskRegistry.get_instance()
        # Distinct from the queue manager's "discover-cover-prewarm-{uid}" (which warms the
        # deck): this warms the full Discover grid, and the two must not share a registry name.
        task_name = f"discover-grid-cover-prewarm-{user_id}"
        if registry.is_running(task_name):
            return  # a grid prewarm for this user is already draining - don't stack another

        async def _warm() -> None:
            from infrastructure.queue.priority_queue import RequestPriority

            semaphore = asyncio.Semaphore(4)

            async def warm_album(mbid: str, size: str) -> None:
                async with semaphore:
                    try:
                        await self._cover_repo.get_release_group_cover(
                            mbid, size=size, priority=RequestPriority.BACKGROUND_SYNC
                        )
                    except Exception as exc:  # noqa: BLE001
                        logger.debug("Discover cover prewarm (album %s) failed: %s", mbid[:8], exc)

            async def warm_artist(mbid: str) -> None:
                async with semaphore:
                    try:
                        await self._cover_repo.get_artist_image(
                            mbid, size=250, priority=RequestPriority.BACKGROUND_SYNC
                        )
                    except Exception as exc:  # noqa: BLE001
                        logger.debug("Discover cover prewarm (artist %s) failed: %s", mbid[:8], exc)

            jobs = [warm_album(m, "250") for m in album_mbids]
            jobs += [warm_album(m, "500") for m in top_pick_mbids]
            jobs += [warm_artist(m) for m in artist_mbids]
            await asyncio.gather(*jobs, return_exceptions=True)

        task = asyncio.create_task(_warm())
        task.add_done_callback(_log_task_error)
        try:
            registry.register(task_name, task)
        except RuntimeError:
            pass

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
            or response.top_picks
            or response.radio_sections
            or response.unexplored_genres
            or response.listeners_like_you
            or response.anniversaries
            or response.new_from_followed
        )

    async def build_discover_data(self, user_id: str) -> DiscoverResponse:
        (lb_client, lfm_client, username, lfm_username,
         lb_enabled, lfm_enabled, primary) = await self._resolve_user_music(user_id, None)
        jf_enabled = self._integration.is_jellyfin_enabled()
        library_configured = self._integration.is_library_configured()

        library_mbids = await self._mbid.get_library_artist_mbids(library_configured)
        # album sections must check ALBUM membership; checking release-group mbids
        # against the artist set made in_library always-false (owned albums showed
        # download buttons) and neutered the don't-suggest-what-you-own exclusions
        library_album_mbids = await self._mbid.get_library_album_mbids(library_configured)

        seed_artists = await self._get_seed_artists(
            lb_enabled, username, jf_enabled,
            resolved_source=primary,
            lfm_enabled=lfm_enabled,
            lfm_username=lfm_username,
            lb_client=lb_client,
        )

        tasks: dict[str, Any] = {}

        # Similar artists normally come from ListenBrainz (lb-radio/artist). But that endpoint
        # is popularity-ranked (pop_begin/pop_end), so during the LB-popularity outage it
        # returns empty/zero-score - leaving Top Picks with no personalised candidates. When
        # popularity is degraded (or Last.fm is the primary), source similar artists from
        # Last.fm's artist.getSimilar instead - it's independent of LB and carries mbids +
        # match scores, so the whole personalisation chain (similar -> albums) stays alive.
        use_lastfm_similar = bool(
            self._lfm_repo and lfm_enabled
            and (primary == "lastfm" or self._use_lastfm_for_popularity(lfm_enabled))
        )
        for i, seed in enumerate(seed_artists[:3]):
            mbid = seed.artist_mbids[0] if hasattr(seed, 'artist_mbids') and seed.artist_mbids else getattr(seed, 'artist_mbid', None)
            if mbid:
                if use_lastfm_similar:
                    tasks[f"similar_{i}"] = self._lfm_repo.get_similar_artists(
                        seed.artist_name, mbid=mbid, limit=20
                    )
                else:
                    tasks[f"similar_{i}"] = self._lb_repo.get_similar_artists(mbid, max_similar=20)

        if primary == "lastfm" and self._lfm_repo and lfm_enabled:
            tasks["lfm_global_top"] = self._lfm_repo.get_global_top_artists(limit=20)
        else:
            tasks["lb_trending"] = self._lb_repo.get_sitewide_top_artists(count=20)

        # Last.fm-specific sections run whenever the user's Last.fm is linked
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

        # LB-specific sections run whenever the user's ListenBrainz is linked
        if lb_client and username:
            tasks["lb_fresh"] = lb_client.get_user_fresh_releases()
            tasks["lb_genres"] = lb_client.get_user_genre_activity(username)
        if primary == "lastfm" and self._lfm_repo and lfm_enabled and lfm_username:
            tasks["lfm_user_top_artists_for_genres"] = self._lfm_repo.get_user_top_artists(
                lfm_username, period="3month", limit=5
            )

        if jf_enabled:
            tasks["jf_most_played"] = self._jf_repo.get_most_played_artists(limit=50)

        if library_configured:
            tasks["library_artists"] = self._library_repo.get_artists_from_library(include_unmonitored=True)
            tasks["library_albums"] = self._library_repo.get_library(include_unmonitored=True)

        results = await self._execute_tasks(tasks)
        degraded = self._degraded_sources(list(tasks.keys()), self._last_failed_task_keys)

        response = DiscoverResponse(
            integration_status=self._integration.get_integration_status(),
            service_status=degraded or None,
        )

        seen_artist_mbids: set[str] = set()

        response.because_you_listen_to = self._build_because_sections(
            seed_artists, results, library_mbids, seen_artist_mbids,
            resolved_source=primary,
        )
        await self._enrich_because_sections_audiodb(response.because_you_listen_to)

        response.fresh_releases = self._build_fresh_releases(results, library_album_mbids)

        post_tasks: dict[str, Any] = {
            "missing_essentials": self._build_missing_essentials(results, library_album_mbids),
            "lastfm_weekly_album_chart": self._build_lastfm_weekly_album_chart(
                results, library_album_mbids
            ),
            "lastfm_recent_scrobbles": self._build_lastfm_recent_scrobbles(
                results, library_album_mbids
            ),
            "daily_mixes": self._build_daily_mix_sections(user_id, primary, library_album_mbids, lfm_enabled),
            "top_picks": self._build_top_picks(
                user_id, primary, lb_enabled, username, results, seed_artists, lfm_enabled,
            ),
            "radio_sections": self._build_radio_sections(
                seed_artists, library_album_mbids, primary, lfm_enabled,
            ),
            "anniversaries": self._build_anniversaries(),
            "new_from_followed": self._build_new_from_followed(user_id),
        }
        if lb_client and username:
            post_tasks["listeners_like_you"] = self._build_listeners_like_you(
                lb_client, username, user_id,
            )
        # LB-specific: runs whenever the user's ListenBrainz is linked
        if lb_client and username:
            post_tasks["weekly_exploration"] = self._weekly_exploration.build_section(
                username, lb_repo=lb_client
            )
        post_results = await self._execute_tasks(post_tasks)
        response.missing_essentials = post_results.get("missing_essentials")
        response.weekly_exploration = post_results.get("weekly_exploration")
        response.daily_mixes = post_results.get("daily_mixes") or []
        response.top_picks = post_results.get("top_picks")
        response.radio_sections = post_results.get("radio_sections") or []
        response.listeners_like_you = post_results.get("listeners_like_you")
        response.anniversaries = post_results.get("anniversaries")
        response.new_from_followed = post_results.get("new_from_followed")

        response.rediscover = self._build_rediscover(results, library_mbids, jf_enabled)

        response.artists_you_might_like = self._build_artists_you_might_like(
            seed_artists, results, library_mbids, seen_artist_mbids,
            resolved_source=primary,
        )

        response.popular_in_your_genres = await self._build_popular_in_genres(
            results, library_mbids, seen_artist_mbids,
            resolved_source=primary,
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

        if primary == "lastfm":
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

    async def _build_daily_mix_sections(self, user_id: str, resolved_source: str, library_mbids: set[str], lfm_enabled: bool = False) -> list[HomeSection]:
        # 3-5 genre-clustered daily mixes, 60/40 new-to-familiar ratio
        try:
            if self._genre_index is None:
                return []

            if self._memory_cache and not discover_build_thorough.get():
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
                        lfm_enabled,
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
        lfm_enabled: bool = False,
    ) -> HomeSection | None:
        genre_label = genre_lower.title()
        MAX_ITEMS = 12

        seed_count = min(3, len(cluster_artists))
        seed_mbids = self._daily_rng("daily-mix", genre_lower).sample(cluster_artists, seed_count)

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
            pools = await self._similar_artist_album_pools(
                seeds,
                seed_mbids,
                excluded_mbids=library_mbids,
                similar_limit=10,
                albums_per=3,
                lfm_enabled=lfm_enabled,
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

    async def _add_lastfm_top_pick_candidates(
        self,
        sim_artist_list: list,
        exclude: set[str],
        seen_rgs: set[str],
        candidates: list,
    ) -> None:
        """Build Top Picks candidates from Last.fm top-albums for each already-scored
        similar artist (used only when LB popularity is degraded). Reuses the queue's
        Last.fm album -> release-group-mbid resolver so covers/links/dedup stay correct;
        the LB-derived similarity score rides along."""
        if self._lfm_repo is None:
            return
        sim_lookup: dict[str, tuple[float, str, str | None]] = {}
        for mbid, (sim, artist_name, seed_name) in sim_artist_list:
            norm = self._mbid.normalize_mbid(mbid) or mbid
            sim_lookup[norm] = (sim, artist_name, seed_name or None)

        album_results = await asyncio.gather(
            *(
                self._lfm_repo.get_artist_top_albums(artist_name, mbid=mbid, limit=3)
                for mbid, (_sim, artist_name, _seed) in sim_artist_list
            ),
            return_exceptions=True,
        )
        pairs: list[tuple[Any, list]] = []
        for (mbid, (_sim, artist_name, _seed)), albums in zip(sim_artist_list, album_results):
            if isinstance(albums, Exception) or not albums:
                continue
            pairs.append((SimpleNamespace(mbid=mbid, name=artist_name), albums))
        if not pairs:
            return

        items = await self._mbid.lastfm_albums_to_queue_items(
            pairs, exclude=exclude, target=40, reason="top_picks"
        )
        for it in items:
            if it.release_group_mbid in seen_rgs:
                continue
            look = sim_lookup.get(self._mbid.normalize_mbid(it.artist_mbid) or it.artist_mbid)
            sim, _name, seed_name = look if look else (0.0, "", None)
            seen_rgs.add(it.release_group_mbid)
            candidates.append(TopPickCandidate(
                release_group_mbid=it.release_group_mbid,
                album_name=it.album_name,
                artist_name=it.artist_name,
                artist_mbid=it.artist_mbid,
                sim=sim,
                # Last.fm playcount isn't on LB's listen-count scale; leave the
                # popularity term neutral and let similarity + genre carry the score
                listen_count=0,
                seed_artist=seed_name,
            ))

    def _use_lastfm_for_popularity(self, lfm_enabled: bool) -> bool:
        """The ONE gate for popularity fallback. We ALWAYS prefer ListenBrainz; we only
        use Last.fm when LB's popularity API is DEFINITELY degraded - meaning LB itself
        returned an explicit outage response ("Popularity API currently disabled" or the
        anti-scraper 401), recorded in the ServiceHealthRegistry with a sliding TTL (see
        repositories/listenbrainz_repository._mark_popularity_degraded). A transient
        timeout, a network blip, or a merely-empty result does NOT count. And only then,
        if Last.fm is actually available."""
        return lb_popularity_degraded() and lfm_enabled and self._lfm_repo is not None

    async def _similar_artist_album_pools(
        self,
        seeds: list,
        seed_mbids: list[str],
        *,
        excluded_mbids: set[str],
        similar_limit: int,
        albums_per: int,
        lfm_enabled: bool,
    ) -> list[list[Any]]:
        """Turn seed artists into pools of candidate albums. ALWAYS prefers
        ListenBrainz; swaps to the Last.fm pool builder ONLY when LB popularity is
        DEFINITELY degraded (see _use_lastfm_for_popularity). Used by Daily Mixes and
        the Radio shelves, whose albums otherwise come from LB's dead popularity API."""
        if self._use_lastfm_for_popularity(lfm_enabled):
            return await build_similar_artist_pools_lastfm(
                seed_mbids,
                excluded_mbids=excluded_mbids,
                similar_limit=similar_limit,
                albums_per=albums_per,
                lfm_repo=self._lfm_repo,
                mbid_svc=self._mbid,
            )
        return await build_similar_artist_pools(
            seeds,
            excluded_mbids=excluded_mbids,
            similar_limit=similar_limit,
            albums_per=albums_per,
            lb_repo=self._lb_repo,
            mbid_svc=self._mbid,
        )

    async def _build_top_picks(
        self,
        user_id: str,
        primary: str,
        lb_enabled: bool,
        username: str | None,
        results: dict[str, Any],
        seed_artists: list,
        lfm_enabled: bool = False,
    ) -> TopPicksSection | None:
        """Scored "we think you'd like X - N% match" picks. Candidates come from the
        already-fetched similar-artist pools plus sitewide trending; scoring is pure
        (services.discover.top_picks) and deterministic within a day."""
        try:
            # A thorough (warmer) build must REBUILD - never short-circuit on the cached
            # section. Otherwise a cold on-visit build that cached a trending-only result
            # (short degraded TTL) makes the very warm meant to fix it a no-op, and it never
            # converges. On-visit builds still read the cache for speed.
            if self._memory_cache is not None and not discover_build_thorough.get():
                cache_key = self._top_picks_cache_key(user_id, primary)
                cached = await self._memory_cache.get(cache_key)
                if isinstance(cached, dict) and "section" in cached:
                    return cached["section"]  # type: ignore[return-value]

            _, count = self._integration.get_discover_picks_settings()
            library_configured = self._integration.is_library_configured()
            library_album_mbids = await self._mbid.get_library_album_mbids(library_configured)
            listened = await self._mbid.get_user_listened_release_group_mbids(
                lb_enabled, username, primary
            )
            ignored: set[str] = set()
            if self._mbid_store is not None:
                try:
                    ignored = await self._mbid_store.get_ignored_release_mbids(user_id)
                except Exception:  # noqa: BLE001
                    logger.warning("Failed to load ignored release MBIDs for top picks")
            exclude = library_album_mbids | listened | ignored

            # 1) similarity pool: the similar-artist results fetched for the
            #    because-you-listen-to sections (no extra similarity calls)
            similar_artists: list[tuple[str, str, float, str]] = []  # (mbid, name, raw, seed)
            for i, seed in enumerate(seed_artists[:3]):
                seed_name = getattr(seed, "artist_name", "")
                for artist in results.get(f"similar_{i}") or []:
                    mbid = self._mbid.normalize_mbid(
                        getattr(artist, "artist_mbid", None) or getattr(artist, "mbid", None)
                    )
                    if not mbid or mbid == VARIOUS_ARTISTS_MBID:
                        continue
                    name = getattr(artist, "artist_name", None) or getattr(artist, "name", "")
                    raw = getattr(artist, "score", None)
                    if raw is None:
                        raw = getattr(artist, "match", 0.0) or 0.0
                    similar_artists.append((mbid, name, float(raw or 0.0), seed_name))
            # LB scores are unbounded counts: normalise by the batch max
            max_raw = max((raw for _, _, raw, _ in similar_artists), default=0.0)
            sim_by_artist: dict[str, tuple[float, str, str]] = {}
            for mbid, name, raw, seed_name in similar_artists:
                sim = (raw / max_raw) if max_raw > 1.0 else min(1.0, raw)
                prev = sim_by_artist.get(mbid)
                if prev is None or sim > prev[0]:
                    sim_by_artist[mbid] = (sim, name, seed_name)

            candidates: list[TopPickCandidate] = []
            seen_rgs: set[str] = set()

            # cast a wide net: with a big owned/heard library, many similar artists have all
            # their top albums excluded, so 12 left too few distinct artists to personalise from
            sim_artist_list = sorted(
                sim_by_artist.items(), key=lambda kv: kv[1][0], reverse=True
            )[:20]

            if self._use_lastfm_for_popularity(lfm_enabled):
                # LB popularity is DEFINITELY degraded: turn these (LB-derived) similar
                # artists into albums via Last.fm's top-albums instead of LB's dead
                # popularity endpoint. The similarity scores still come from LB.
                # This path resolves Last.fm albums -> release-group mbids via MusicBrainz
                # (1/s); under the outage it competes with every other section. Bound it
                # so a starved resolution can't consume the whole task budget and cancel
                # the build before the MB-free trending pool below runs (which would leave
                # Top Picks empty). On timeout we keep whatever resolved and fall through
                # to trending, so the section always populates.
                try:
                    await asyncio.wait_for(
                        self._add_lastfm_top_pick_candidates(
                            sim_artist_list, exclude, seen_rgs, candidates
                        ),
                        timeout=_scaled(TOP_PICKS_SIMILARITY_BUDGET_SECONDS),
                    )
                except Exception:  # noqa: BLE001 - starved similarity, use trending only
                    logger.warning(
                        "Top Picks Last.fm similarity pool exceeded its budget; "
                        "building from the trending pool only"
                    )
            else:
                # ALWAYS-preferred path: ListenBrainz popularity
                rg_results = await asyncio.gather(
                    *(
                        self._lb_repo.get_artist_top_release_groups(mbid, count=2)
                        for mbid, _ in sim_artist_list
                    ),
                    return_exceptions=True,
                )
                for (artist_mbid, (sim, artist_name, seed_name)), rgs in zip(
                    sim_artist_list, rg_results
                ):
                    if isinstance(rgs, Exception):
                        continue
                    for rg in rgs:
                        rg_mbid = self._mbid.normalize_mbid(rg.release_group_mbid)
                        if not rg_mbid or rg_mbid in exclude or rg_mbid in seen_rgs:
                            continue
                        seen_rgs.add(rg_mbid)
                        candidates.append(TopPickCandidate(
                            release_group_mbid=rg_mbid,
                            album_name=rg.release_group_name,
                            artist_name=rg.artist_name or artist_name,
                            artist_mbid=artist_mbid,
                            sim=sim,
                            listen_count=getattr(rg, "listen_count", 0) or 0,
                            seed_artist=seed_name or None,
                        ))

            # 2) trending pool: the diversity tail (no account needed)
            try:
                trending = await asyncio.wait_for(
                    self._lb_repo.get_sitewide_top_release_groups(count=100), timeout=30
                )
            except Exception:  # noqa: BLE001
                trending = []
            for rg in trending:
                rg_mbid = self._mbid.normalize_mbid(rg.release_group_mbid)
                if not rg_mbid or rg_mbid in exclude or rg_mbid in seen_rgs:
                    continue
                artist_mbid = self._mbid.normalize_mbid(
                    rg.artist_mbids[0] if rg.artist_mbids else None
                )
                if artist_mbid == VARIOUS_ARTISTS_MBID:
                    continue
                seen_rgs.add(rg_mbid)
                candidates.append(TopPickCandidate(
                    release_group_mbid=rg_mbid,
                    album_name=rg.release_group_name,
                    artist_name=rg.artist_name,
                    artist_mbid=artist_mbid or "",
                    listen_count=getattr(rg, "listen_count", 0) or 0,
                    from_trending=True,
                ))

            # Degraded = we're leaning on the Last.fm->MB personalisation path but it
            # produced no personalised (non-trending) picks this build. Cache such a
            # result briefly so it retries as the MB cache warms, rather than freezing a
            # trending-only ("48% match") Top Picks for the full 4h.
            personalized = sum(1 for c in candidates if not c.from_trending)
            degraded = self._use_lastfm_for_popularity(lfm_enabled) and personalized == 0
            picks_ttl = DISCOVER_PICKS_DEGRADED_TTL if degraded else DISCOVER_PICKS_CACHE_TTL

            if not candidates:
                await self._cache_top_picks_result(None, user_id, primary, ttl=picks_ttl)
                return None

            # genre signals
            user_genres: set[str] = set()
            if self._genre_index is not None:
                try:
                    top_genres = await self._genre_index.get_top_genres(limit=10)
                    user_genres = {g.lower() for g, _ in top_genres}
                except Exception:  # noqa: BLE001
                    pass
            for g in results.get("lb_genres") or []:
                name = getattr(g, "genre", None)
                if name:
                    user_genres.add(name.lower())
            genres_by_artist: dict[str, list[str]] = {}
            if self._genre_index is not None:
                artist_mbids = [c.artist_mbid for c in candidates if c.artist_mbid]
                try:
                    genres_by_artist = await self._genre_index.get_genres_for_artists(artist_mbids)
                except Exception:  # noqa: BLE001
                    pass

            picks = score_candidates(
                candidates,
                user_id=user_id,
                date_iso=datetime.now(timezone.utc).date().isoformat(),
                user_genres=user_genres,
                genres_by_artist=genres_by_artist,
                count=count,
            )
            if not picks:
                await self._cache_top_picks_result(None, user_id, primary, ttl=picks_ttl)
                return None

            section = TopPicksSection(
                items=[
                    TopPickItem(
                        album=HomeAlbum(
                            name=p_.candidate.album_name,
                            mbid=p_.candidate.release_group_mbid,
                            artist_name=p_.candidate.artist_name,
                            artist_mbid=p_.candidate.artist_mbid or None,
                            image_url=f"/api/v1/covers/release-group/{p_.candidate.release_group_mbid}?size=500",
                            listen_count=p_.candidate.listen_count or None,
                        ),
                        match_pct=p_.match_pct,
                        reasons=p_.reasons,
                        seed_artist=p_.candidate.seed_artist,
                    )
                    for p_ in picks
                ],
                source=primary,
                personalizing=degraded,
            )
            personalised_picks = sum(1 for p_ in picks if not p_.candidate.from_trending)
            logger.info(
                "Top Picks built for %s: %d picks (%d personalised, %d trending), degraded=%s",
                user_id[:8], len(picks), personalised_picks,
                len(picks) - personalised_picks, degraded,
            )
            await self._cache_top_picks_result(section, user_id, primary, ttl=picks_ttl)
            return section

        except Exception as e:  # noqa: BLE001
            logger.warning(f"Top picks builder failed: {e}")
            return None

    async def _cache_top_picks_result(
        self, result: TopPicksSection | None, user_id: str, source: str,
        ttl: int = DISCOVER_PICKS_CACHE_TTL,
    ) -> None:
        if self._memory_cache:
            cache_key = self._top_picks_cache_key(user_id, source)
            await self._memory_cache.set(
                cache_key, {"section": result}, ttl,
            )

    async def _build_listeners_like_you(
        self, lb_client: Any, username: str, user_id: str,
    ) -> HomeSection | None:
        """Albums that ListenBrainz users with similar taste are playing this month.
        Optional enrichment: any failure degrades to None (no banner - LB partial)."""
        try:
            similar_users = await lb_client.get_similar_users(username)
            if not similar_users:
                return None
            names = [
                u.get("user_name")
                for u in similar_users[:3]
                if isinstance(u, dict) and u.get("user_name")
            ]
            if not names:
                return None

            library_configured = self._integration.is_library_configured()
            library_album_mbids = await self._mbid.get_library_album_mbids(library_configured)
            ignored: set[str] = set()
            if self._mbid_store is not None:
                try:
                    ignored = await self._mbid_store.get_ignored_release_mbids(user_id)
                except Exception:  # noqa: BLE001
                    pass
            exclude = library_album_mbids | ignored

            rg_lists = await asyncio.gather(
                *(
                    lb_client.get_user_top_release_groups(
                        username=name, range_="this_month", count=15
                    )
                    for name in names
                ),
                return_exceptions=True,
            )
            items: list[HomeAlbum] = []
            seen: set[str] = set()
            for rg_list in rg_lists:
                if isinstance(rg_list, Exception):
                    continue
                for rg in rg_list:
                    rg_mbid = self._mbid.normalize_mbid(rg.release_group_mbid)
                    if not rg_mbid or rg_mbid in exclude or rg_mbid in seen:
                        continue
                    artist_mbid = self._mbid.normalize_mbid(
                        rg.artist_mbids[0] if rg.artist_mbids else None
                    )
                    if artist_mbid == VARIOUS_ARTISTS_MBID:
                        continue
                    seen.add(rg_mbid)
                    items.append(HomeAlbum(
                        name=rg.release_group_name,
                        mbid=rg_mbid,
                        artist_name=rg.artist_name,
                        artist_mbid=artist_mbid or None,
                        image_url=f"/api/v1/covers/release-group/{rg_mbid}?size=500",
                        listen_count=getattr(rg, "listen_count", 0) or None,
                    ))
                    if len(items) >= 15:
                        break
                if len(items) >= 15:
                    break
            if not items:
                return None
            return HomeSection(
                title="Listeners Like You Are Playing",
                type="albums",
                items=items,
                source="listenbrainz",
            )
        except Exception as e:  # noqa: BLE001
            logger.debug("Listeners-like-you builder failed: %s", e)
            return None

    # milestone birthdays celebrated per anniversary year; the library stores only a
    # release YEAR (no month/day), so "on this day" precision isn't possible without
    # per-album MusicBrainz lookups - see DECISIONS-LIVE 2026-07-03
    _ANNIVERSARY_YEARS = (10, 20, 25, 30, 40, 50, 60)

    async def _build_anniversaries(self) -> HomeSection | None:
        """Library albums hitting a round anniversary this year."""
        if self._library_db is None:
            return None
        try:
            albums = await self._library_db.get_albums()
        except Exception as e:  # noqa: BLE001
            logger.debug("Anniversaries builder failed to read the library: %s", e)
            return None
        this_year = datetime.now(timezone.utc).year
        matches: list[tuple[int, dict[str, Any]]] = []
        for album in albums:
            year = album.get("year")
            if not isinstance(year, int) or year <= 0:
                continue
            age = this_year - year
            if age in self._ANNIVERSARY_YEARS:
                matches.append((age, album))
        if not matches:
            return None
        # roundest birthdays first (50 before 10), newest within a tier
        matches.sort(key=lambda m: (-m[0], -(m[1].get("year") or 0)))
        items: list[HomeAlbum] = []
        for age, album in matches[:12]:
            mbid = album.get("mbid") or album.get("mbid_lower")
            items.append(HomeAlbum(
                name=album.get("title", "Unknown"),
                mbid=mbid,
                artist_name=album.get("artist_name"),
                artist_mbid=album.get("artist_mbid"),
                image_url=(
                    f"/api/v1/covers/release-group/{mbid}?size=500" if mbid else None
                ),
                release_date=str(album.get("year") or ""),
                in_library=True,
            ))
        return HomeSection(
            title="Milestone Anniversaries",
            type="albums",
            items=items,
            source="library",
        )

    async def _build_new_from_followed(self, user_id: str) -> HomeSection | None:
        """Newest releases from artists this user follows (native follow system)."""
        if self._follow_service is None:
            return None
        try:
            releases, _total = await self._follow_service.list_new_releases(user_id, 10, 0)
        except Exception as e:  # noqa: BLE001
            logger.debug("New-from-followed builder failed: %s", e)
            return None
        if not releases:
            return None
        items = [
            HomeAlbum(
                name=r.title,
                mbid=r.release_group_mbid,
                artist_name=r.artist_name,
                artist_mbid=r.artist_mbid,
                image_url=f"/api/v1/covers/release-group/{r.release_group_mbid}?size=500",
                release_date=r.first_release_date,
            )
            for r in releases
        ]
        return HomeSection(
            title="New From Artists You Follow",
            type="albums",
            items=items,
            source="library",
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

            self._daily_rng("unexplored").shuffle(filtered)
            filtered = filtered[:UNEXPLORED_GENRES_MAX]

            if not filtered:
                top_genre_names = [g for g, _ in top_genres_raw]
                fallback = await self._genre_index.get_underrepresented_genres(
                    top_genre_names, threshold=UNEXPLORED_GENRES_THRESHOLD
                )
                self._daily_rng("unexplored-fallback").shuffle(fallback)
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
                # library_mbids is keyed by release-GROUP mbid; the transformer set
                # in_library against the raw Last.fm RELEASE mbid, so recompute against
                # the resolved release-group mbid or owned albums show a download button.
                home_album.in_library = resolved.lower() in library_mbids
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
        # Bound the MB resolution: during the LB-popularity outage this competes with
        # every other section on MB's 1/s limit. On starvation return an empty map -
        # callers fall back to the raw release mbid - instead of letting the whole
        # section time out at the 25s task budget and vanish.
        try:
            results = await asyncio.wait_for(
                asyncio.gather(*tasks, return_exceptions=True),
                timeout=_scaled(DISCOVER_MB_RESOLVE_BUDGET_SECONDS),
            )
        except Exception:  # noqa: BLE001 - MB starved, degrade to raw release mbids
            logger.warning("Release->release-group resolution exceeded its budget; using raw mbids")
            return {}
        rg_map: dict[str, str] = {}
        for rid, rg_id in zip(unique_ids, results):
            if isinstance(rg_id, str) and rg_id:
                rg_map[rid] = rg_id
        return rg_map

    async def _artist_image_url(self, mbid: str) -> str | None:
        """Best AudioDB image URL for an artist, or None. Cached per-MBID on disk."""
        if not self._audiodb_image_service:
            return None
        try:
            images = await self._audiodb_image_service.fetch_and_cache_artist_images(mbid)
            if images and not images.is_negative:
                return images.wide_thumb_url or images.banner_url or images.fanart_url
        except Exception as exc:  # noqa: BLE001
            logger.debug("Failed to fetch artist image for %s: %s", mbid, exc)
        return None

    async def _get_genre_artist(self, genre_name: str, exclude_mbids: set[str] | None = None) -> str | None:
        chosen: str | None = None
        try:
            artists = await self._mb_repo.search_artists_by_tag(genre_name, limit=10)
            first_valid: str | None = None
            for artist in artists:
                mbid = artist.musicbrainz_id
                if not mbid or mbid == VARIOUS_ARTISTS_MBID:
                    continue
                if exclude_mbids and mbid in exclude_mbids:
                    continue
                if first_valid is None:
                    first_valid = mbid
                # Prefer an artist AudioDB has a picture for so the tile shows art.
                if await self._artist_image_url(mbid):
                    return mbid
            chosen = first_valid
        except Exception:  # noqa: BLE001
            logger.debug("Failed to resolve genre artist from library")
        return chosen

    async def _resolve_genre_artist_images(
        self, genre_artists: dict[str, str | None]
    ) -> dict[str, str | None]:
        if not self._audiodb_image_service or not genre_artists:
            return {}

        sem = asyncio.Semaphore(5)

        async def _resolve_one(genre: str, mbid: str) -> tuple[str, str | None]:
            async with sem:
                return (genre, await self._artist_image_url(mbid))

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
        _task_timeout = _scaled(DISCOVER_TASK_TIMEOUT_SECONDS)
        coros = [asyncio.wait_for(c, timeout=_task_timeout) for c in tasks.values()]
        raw_results = await asyncio.gather(*coros, return_exceptions=True)
        results = {}
        self._last_failed_task_keys = set()
        for key, result in zip(keys, raw_results):
            if isinstance(result, Exception):
                logger.warning(f"Discover task {key} failed: {result}")
                results[key] = None
                self._last_failed_task_keys.add(key)
            else:
                results[key] = result
        return results

    @staticmethod
    def _degraded_sources(task_keys: list[str], failed: set[str]) -> dict[str, str]:
        """When EVERY task of a source family failed, tell the user (DegradedBanner)
        instead of silently rendering a page with holes."""
        status: dict[str, str] = {}
        for prefix, label in (("lb_", "listenbrainz"), ("lfm_", "lastfm")):
            family = [k for k in task_keys if k.startswith(prefix)]
            if family and all(k in failed for k in family):
                status[label] = "unavailable"
        return status

    async def _build_radio_sections(
        self,
        seed_artists: list[ListenBrainzArtist],
        library_mbids: set[str],
        source: str,
        lfm_enabled: bool = False,
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
                    self._similar_artist_album_pools(
                        [seed],
                        [seed_mbid],
                        excluded_mbids=library_mbids,
                        similar_limit=15,
                        albums_per=3,
                        lfm_enabled=lfm_enabled,
                    ),
                    timeout=_scaled(RADIO_POOL_BUDGET_SECONDS),
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
