import asyncio
import logging
from time import monotonic
from typing import TYPE_CHECKING, Any, Literal, Optional

from api.v1.schemas.discovery import (
    SimilarArtist,
    SimilarArtistsResponse,
    TopSong,
    TopSongsResponse,
    TopAlbum,
    TopAlbumsResponse,
)
from repositories.protocols import (
    ListenBrainzRepositoryProtocol,
    LastFmRepositoryProtocol,
    MusicBrainzRepositoryProtocol,
    LibraryRepositoryProtocol,
)
from infrastructure.cache.cache_keys import (
    ARTIST_DISCOVERY_PREFIX,
    ARTIST_DISCOVERY_TOP_ALBUMS_PREFIX,
    ARTIST_DISCOVERY_TOP_SONGS_PREFIX,
)
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.persistence import LibraryDB
from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.resilience.retry import CircuitOpenError
from services.per_user_client_factory import PerUserClientFactory
from services.preferences_service import PreferencesService
from infrastructure.degradation import (
    clear_degradation_context,
    init_degradation_context,
    try_get_degradation_context,
)
from repositories.musicbrainz_base import (
    MbSourceContext,
    capture_mb_source_context,
    is_mb_source_current,
    mb_publish_if_current,
)
from repositories.listenbrainz_repository import lb_popularity_degraded


if TYPE_CHECKING:
    from infrastructure.persistence.auth_store import AuthStore
    from services.native.background_workload_gate import BackgroundWorkloadGate

logger = logging.getLogger(__name__)

DISCOVERY_CACHE_TTL_LIBRARY = 21600
DISCOVERY_CACHE_TTL_NON_LIBRARY = 3600
DISCOVERY_EMPTY_CACHE_TTL = 600
CIRCUIT_OPEN_CACHE_TTL = 30
DEFAULT_SIMILAR_COUNT = 15
DEFAULT_TOP_SONGS_COUNT = 10
DEFAULT_TOP_ALBUMS_COUNT = 10
_DISCOVERY_WORKER_TIMEOUT = 120
_PRECACHE_MAX_CONSECUTIVE_FAILURES = 5
_PRECACHE_PAUSE_SECONDS = 1800.0  # matches ListenBrainz _POPULARITY_DEGRADED_TTL

# Module-level flag survives singleton cache invalidation / instance recreation
_discovery_precache_running = False

# Module-level pause state survives singleton cache invalidation / instance
# recreation, same rationale as _discovery_precache_running above.
_precache_consecutive_failures = 0
_precache_paused_until = 0.0  # time.monotonic deadline; 0 = not paused

from infrastructure.observability.library_metrics import LibraryMetrics

_precache_metrics = LibraryMetrics.for_library_workload()


def _record_precache_unit_failure() -> None:
    global _precache_consecutive_failures, _precache_paused_until
    _precache_consecutive_failures += 1
    if _precache_consecutive_failures >= _PRECACHE_MAX_CONSECUTIVE_FAILURES:
        _precache_paused_until = monotonic() + _PRECACHE_PAUSE_SECONDS
        _precache_consecutive_failures = 0
        logger.info(
            "Discovery precache paused for %ds after %d consecutive unit "
            "failures (upstream outage backoff); will probe again after the pause",
            int(_PRECACHE_PAUSE_SECONDS),
            _PRECACHE_MAX_CONSECUTIVE_FAILURES,
        )


def _record_precache_unit_success() -> None:
    global _precache_consecutive_failures
    _precache_consecutive_failures = 0


_PrecacheOutcome = Literal[
    "healthy_data", "healthy_empty", "degraded", "failed", "configured_absent"
]


def _classify_precache_outcome(
    results: list[Any],
    degraded: bool,
    has_data: bool,
    configured_absent: bool,
) -> _PrecacheOutcome:
    if any(isinstance(r, Exception) for r in results):
        return "failed"
    if configured_absent:
        return "configured_absent"
    if has_data:
        return "healthy_data"
    if degraded:
        return "degraded"
    return "healthy_empty"


def _dedupe_similar_artists(artists: list[SimilarArtist]) -> list[SimilarArtist]:
    """Drop entries with a missing or duplicate musicbrainz_id.

    ListenBrainz/Last.fm can return the same artist more than once (or with no
    mbid). The frontend keys the similar-artists carousel by musicbrainz_id, so
    a missing/duplicate id would collide and throw svelte's each_key_duplicate,
    blanking the artist page.
    """
    seen: set[str] = set()
    deduped: list[SimilarArtist] = []
    for artist in artists:
        key = (artist.musicbrainz_id or "").lower()
        if not key or key in seen:
            continue
        seen.add(key)
        deduped.append(artist)
    return deduped


class ArtistDiscoveryService:
    def __init__(
        self,
        listenbrainz_repo: ListenBrainzRepositoryProtocol,
        musicbrainz_repo: MusicBrainzRepositoryProtocol,
        library_db: LibraryDB,
        library_repo: LibraryRepositoryProtocol,
        memory_cache: CacheInterface,
        lastfm_repo: Optional[LastFmRepositoryProtocol] = None,
        preferences_service: Optional[PreferencesService] = None,
        client_factory: Optional[PerUserClientFactory] = None,
        auth_store: Optional["AuthStore"] = None,
        workload_gate: "BackgroundWorkloadGate | None" = None,
    ):
        self._lb_repo = listenbrainz_repo
        self._mb_repo = musicbrainz_repo
        self._library_db = library_db
        self._library_repo = library_repo
        self._cache = memory_cache
        # A2 part 4 (B5): stampede maps for the artist-page satellites,
        # mirroring ArtistService._artist_basic_in_flight lifecycle.
        self._similar_in_flight: dict[
            tuple[str, str, int, str], asyncio.Future[SimilarArtistsResponse]
        ] = {}
        self._top_albums_in_flight: dict[
            tuple[str, str, int, str, int], asyncio.Future[TopAlbumsResponse]
        ] = {}
        self._lastfm_repo = lastfm_repo
        self._preferences_service = preferences_service
        self._client_factory = client_factory
        self._auth_store = auth_store
        self._workload_gate = workload_gate

    async def _resolve_listenbrainz(
        self, user_id: str | None
    ) -> Optional[ListenBrainzRepositoryProtocol]:
        """Per-user ListenBrainz client.

        A known user (user_id present) with a factory always resolves strictly to
        their own connection - never the global repo - so an unlinked user gets
        None. Anonymous/background callers (cache warmers) and unit tests (no
        factory) fall back to the legacy global repo when it is configured.
        """
        if self._client_factory is not None and user_id:
            return await self._client_factory.resolve_listenbrainz(user_id)
        return self._lb_repo if self._lb_repo.is_configured() else None

    async def _resolve_lastfm(
        self, user_id: str | None
    ) -> Optional[LastFmRepositoryProtocol]:
        if self._client_factory is not None and user_id:
            return await self._client_factory.resolve_lastfm(user_id)
        if (
            self._lastfm_repo
            and self._preferences_service
            and self._preferences_service.is_lastfm_enabled()
        ):
            return self._lastfm_repo
        return None

    async def _lastfm_fallback(
        self, kind: str, user_id: str | None, artist_mbid: str, count: int
    ):
        """When the ListenBrainz source yields nothing (popularity disabled/auth-gated
        or breaker tripped upstream), try the same section from Last.fm. Returns the
        response (possibly empty) or None when Last.fm isn't available/failed."""
        try:
            if kind == "similar":
                result = await self.get_similar_artists(
                    artist_mbid, count, source="lastfm", user_id=user_id
                )
            elif kind == "top_songs":
                result = await self.get_top_songs(
                    artist_mbid, count, source="lastfm", user_id=user_id
                )
            else:
                result = await self.get_top_albums(
                    artist_mbid, count, source="lastfm", user_id=user_id
                )
            return result if result.configured else None
        except Exception as e:  # noqa: BLE001
            logger.warning(
                "Last.fm %s fallback failed for %s: %s", kind, artist_mbid[:8], e
            )
        return None

    def _resolve_source(
        self, source: Literal["listenbrainz", "lastfm"] | None
    ) -> Literal["listenbrainz", "lastfm"]:
        if source in ("listenbrainz", "lastfm"):
            resolved: Literal["listenbrainz", "lastfm"] = source
        elif self._preferences_service:
            preferred = self._preferences_service.get_primary_music_source().source
            resolved = (
                preferred if preferred in ("listenbrainz", "lastfm") else "listenbrainz"
            )
        else:
            resolved = "listenbrainz"
        return resolved

    def _get_discovery_ttl(self, in_library: bool) -> int:
        if self._preferences_service:
            try:
                advanced_settings = self._preferences_service.get_advanced_settings()
                return (
                    advanced_settings.cache_ttl_artist_discovery_library
                    if in_library
                    else advanced_settings.cache_ttl_artist_discovery_non_library
                )
            except AttributeError:
                logger.debug(
                    "Artist discovery advanced settings unavailable", exc_info=True
                )
        return (
            DISCOVERY_CACHE_TTL_LIBRARY
            if in_library
            else DISCOVERY_CACHE_TTL_NON_LIBRARY
        )

    def _get_empty_discovery_ttl(self) -> int:
        return DISCOVERY_EMPTY_CACHE_TTL

    def _build_cache_key(
        self,
        category: Literal["similar", "top_songs", "top_albums"],
        artist_mbid: str,
        count: int,
        source: str,
    ) -> str:
        prefix = {
            "similar": f"{ARTIST_DISCOVERY_PREFIX}similar:",
            "top_songs": ARTIST_DISCOVERY_TOP_SONGS_PREFIX,
            "top_albums": ARTIST_DISCOVERY_TOP_ALBUMS_PREFIX,
        }[category]
        return f"{prefix}{artist_mbid}:{count}:{source}"

    async def get_similar_artists(
        self,
        artist_mbid: str,
        count: int = 15,
        source: Literal["listenbrainz", "lastfm"] | None = None,
        user_id: str | None = None,
    ) -> SimilarArtistsResponse:
        effective_source = self._resolve_source(source)
        cache_key = self._build_cache_key(
            "similar", artist_mbid, count, effective_source
        )
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        # A2 part 4 (B5): coalesce concurrent cold renders onto one leader
        # chain - shielded follower wait, set_result/set_exception on both
        # paths, finally pop. Keyed identically to the cache entry.
        stampede_key: tuple[str, str, int, str] = (
            "similar",
            artist_mbid,
            count,
            effective_source,
        )
        current_task = asyncio.current_task()
        existing = self._similar_in_flight.get(stampede_key)
        if existing is not None:
            future, owner = existing
            if owner is current_task:
                # A2: recursive re-entry from inside this leader (the Last.fm
                # fallback calls back into get_similar_artists with a
                # different source that _resolve_source may fold back onto the
                # same key). Bypass the map and compute directly - awaiting
                # our own pending future here would self-deadlock.
                return await self._load_similar_artists(
                    artist_mbid, count, effective_source, user_id, cache_key
                )
            return await asyncio.shield(future)

        loop = asyncio.get_running_loop()
        future: asyncio.Future[SimilarArtistsResponse] = loop.create_future()
        self._similar_in_flight[stampede_key] = (future, current_task)
        try:
            result = await self._load_similar_artists(
                artist_mbid, count, effective_source, user_id, cache_key
            )
            if not future.done():
                future.set_result(result)
            return result
        except BaseException as exc:
            if not future.done():
                future.set_exception(exc)
                future.exception()
            raise
        finally:
            self._similar_in_flight.pop(stampede_key, None)

    async def _load_similar_artists(
        self,
        artist_mbid: str,
        count: int,
        effective_source: Literal["listenbrainz", "lastfm"],
        user_id: str | None,
        cache_key: str,
    ) -> SimilarArtistsResponse:
        lb_unavailable = False
        if effective_source == "lastfm":
            lastfm_repo = await self._resolve_lastfm(user_id)
            try:
                result = await self._get_similar_artists_lastfm(
                    lastfm_repo, artist_mbid, count
                )
            except Exception as e:  # noqa: BLE001
                logger.warning(
                    "Failed to get Last.fm similar artists for %s: %s",
                    artist_mbid[:8],
                    e,
                )
                result = SimilarArtistsResponse(similar_artists=[], source="lastfm")
        else:
            lb_repo = await self._resolve_listenbrainz(user_id)
            if lb_repo is None:
                return SimilarArtistsResponse(configured=False)
            try:
                similar = await lb_repo.get_similar_artists(
                    artist_mbid, max_similar=count
                )
                library_artist_mbids = await self._library_db.get_all_artist_mbids()

                artists = [
                    SimilarArtist(
                        musicbrainz_id=a.artist_mbid,
                        name=a.artist_name,
                        listen_count=a.listen_count,
                        in_library=a.artist_mbid in library_artist_mbids,
                    )
                    for a in similar[:count]
                ]
                result = SimilarArtistsResponse(similar_artists=artists)
            except CircuitOpenError:
                logger.warning("Circuit open for similar artists %s", artist_mbid[:8])
                result = SimilarArtistsResponse(similar_artists=[])
                lb_unavailable = True
            except Exception as e:  # noqa: BLE001
                logger.warning(
                    "Failed to get similar artists for %s: %s(%s)",
                    artist_mbid[:8],
                    type(e).__name__,
                    e,
                )
                result = SimilarArtistsResponse(similar_artists=[])

        # LB similar/popularity is intermittently disabled or breaker-tripped upstream
        # (2026-07); fall back to Last.fm so the section still fills
        if effective_source == "listenbrainz" and not result.similar_artists:
            fb = await self._lastfm_fallback("similar", user_id, artist_mbid, count)
            if fb is not None and fb.similar_artists:
                result, lb_unavailable = fb, False

        result.similar_artists = _dedupe_similar_artists(result.similar_artists)

        # Degraded empty must use short TTL, not healthy-empty TTL (NEW-CPU-02)
        _ctx = try_get_degradation_context()
        _degraded = (
            (_ctx is not None and _ctx.has_degradation())
            or lb_popularity_degraded()
            or lb_unavailable
        )
        if _degraded and not result.similar_artists:
            await self._cache.set(cache_key, result, ttl_seconds=CIRCUIT_OPEN_CACHE_TTL)
            return result
        in_library = await self._is_library_artist(artist_mbid)
        ttl = (
            self._get_discovery_ttl(in_library)
            if result.similar_artists
            else self._get_empty_discovery_ttl()
        )
        await self._cache.set(cache_key, result, ttl_seconds=ttl)
        return result

    async def get_top_songs(
        self,
        artist_mbid: str,
        count: int = 10,
        source: Literal["listenbrainz", "lastfm"] | None = None,
        user_id: str | None = None,
    ) -> TopSongsResponse:
        source_context = capture_mb_source_context()
        effective_source = self._resolve_source(source)
        cache_key = self._build_cache_key(
            "top_songs", artist_mbid, count, effective_source
        )
        cached = await self._cache.get(cache_key)
        if is_mb_source_current(source_context) and cached is not None:
            return cached
        if not is_mb_source_current(source_context):
            source_context = capture_mb_source_context()

        lb_unavailable = False
        if effective_source == "lastfm":
            lastfm_repo = await self._resolve_lastfm(user_id)
            try:
                result = await self._get_top_songs_lastfm(
                    lastfm_repo, artist_mbid, count
                )
            except Exception as e:  # noqa: BLE001
                logger.warning(
                    "Failed to get Last.fm top songs for %s: %s", artist_mbid[:8], e
                )
                result = TopSongsResponse(songs=[], source="lastfm")
        else:
            lb_repo = await self._resolve_listenbrainz(user_id)
            if lb_repo is None:
                return TopSongsResponse(configured=False)
            try:
                recordings = await lb_repo.get_artist_top_recordings(
                    artist_mbid, count=count
                )

                rg_map = await self._resolve_recording_release_groups(
                    lb_repo, recordings
                )

                # B3.2: positions are pure in-process dict reads off
                # MB_RELEASE_REC_PREFIX (no wire call either way), so gathering
                # them is hygiene-only; zip keeps positions mapped by index so
                # output order is identical to the serial loop.
                ranked = recordings[:count]

                async def _position(rec):
                    if rec.release_mbid and rec.recording_mbid:
                        return await self._mb_repo.get_recording_position_on_release(
                            rec.release_mbid, rec.recording_mbid
                        )
                    return None

                positions = await asyncio.gather(*(_position(r) for r in ranked))

                songs = []
                for r, pos in zip(ranked, positions):
                    disc_number, track_number = pos if pos else (None, None)
                    songs.append(
                        TopSong(
                            recording_mbid=r.recording_mbid,
                            title=r.track_name,
                            artist_name=r.artist_name,
                            release_group_mbid=rg_map.get(r.release_mbid)
                            if r.release_mbid
                            else None,
                            original_release_mbid=r.release_mbid,
                            release_name=r.release_name,
                            listen_count=r.listen_count,
                            disc_number=disc_number,
                            track_number=track_number,
                        )
                    )
                result = TopSongsResponse(songs=songs)
            except CircuitOpenError:
                logger.warning("Circuit open for top songs %s", artist_mbid[:8])
                result = TopSongsResponse(songs=[])
                lb_unavailable = True
            except Exception as e:  # noqa: BLE001
                logger.warning(
                    "Failed to get top songs for %s: %s(%s)",
                    artist_mbid[:8],
                    type(e).__name__,
                    e,
                )
                result = TopSongsResponse(songs=[])

        # LB popularity (top-recordings) is disabled/auth-gated upstream (2026-07);
        # fall back to Last.fm so the section still fills
        if effective_source == "listenbrainz" and not result.songs:
            fb = await self._lastfm_fallback("top_songs", user_id, artist_mbid, count)
            if fb is not None and fb.songs:
                result, lb_unavailable = fb, False

        # Degraded empty must use short TTL, not healthy-empty TTL (NEW-CPU-02)
        _ctx = try_get_degradation_context()
        _degraded = (
            (_ctx is not None and _ctx.has_degradation())
            or lb_popularity_degraded()
            or lb_unavailable
        )
        if _degraded and not result.songs:
            await mb_publish_if_current(
                source_context,
                lambda: self._cache.set(
                    cache_key, result, ttl_seconds=CIRCUIT_OPEN_CACHE_TTL
                ),
            )
            return result
        in_library = await self._is_library_artist(artist_mbid)
        ttl = (
            self._get_discovery_ttl(in_library)
            if result.songs
            else self._get_empty_discovery_ttl()
        )
        await mb_publish_if_current(
            source_context,
            lambda: self._cache.set(cache_key, result, ttl_seconds=ttl),
        )
        return result

    async def get_top_albums(
        self,
        artist_mbid: str,
        count: int = 10,
        source: Literal["listenbrainz", "lastfm"] | None = None,
        user_id: str | None = None,
    ) -> TopAlbumsResponse:
        source_context = capture_mb_source_context()
        effective_source = self._resolve_source(source)
        cache_key = self._build_cache_key(
            "top_albums", artist_mbid, count, effective_source
        )
        cached = await self._cache.get(cache_key)
        if is_mb_source_current(source_context) and cached is not None:
            return cached
        if not is_mb_source_current(source_context):
            source_context = capture_mb_source_context()

        # A2 part 4 (B5): same stampede map as /similar above, with the
        # MusicBrainz source generation preventing a post-switch follower from
        # joining an old MB-backed fallback.
        stampede_key: tuple[str, str, int, str, int] = (
            "top_albums",
            artist_mbid,
            count,
            effective_source,
            source_context.generation,
        )
        current_task = asyncio.current_task()
        existing = self._top_albums_in_flight.get(stampede_key)
        if existing is not None:
            future, owner = existing
            if owner is current_task:
                return await self._load_top_albums(
                    artist_mbid,
                    count,
                    effective_source,
                    user_id,
                    cache_key,
                    source_context=source_context,
                )
            return await asyncio.shield(future)

        loop = asyncio.get_running_loop()
        future: asyncio.Future[TopAlbumsResponse] = loop.create_future()
        self._top_albums_in_flight[stampede_key] = (future, current_task)
        try:
            result = await self._load_top_albums(
                artist_mbid,
                count,
                effective_source,
                user_id,
                cache_key,
                source_context=source_context,
            )
            if not future.done():
                future.set_result(result)
            return result
        except BaseException as exc:
            if not future.done():
                future.set_exception(exc)
                future.exception()
            raise
        finally:
            self._top_albums_in_flight.pop(stampede_key, None)

    async def _load_top_albums(
        self,
        artist_mbid: str,
        count: int,
        effective_source: Literal["listenbrainz", "lastfm"],
        user_id: str | None,
        cache_key: str,
        *,
        source_context: MbSourceContext | None = None,
    ) -> TopAlbumsResponse:
        source_context = source_context or capture_mb_source_context()
        lb_unavailable = False
        if effective_source == "lastfm":
            lastfm_repo = await self._resolve_lastfm(user_id)
            try:
                result = await self._get_top_albums_lastfm(
                    lastfm_repo, artist_mbid, count
                )
            except Exception as e:  # noqa: BLE001
                logger.warning(
                    "Failed to get Last.fm top albums for %s: %s", artist_mbid[:8], e
                )
                result = TopAlbumsResponse(albums=[], source="lastfm")
        else:
            lb_repo = await self._resolve_listenbrainz(user_id)
            if lb_repo is None:
                return TopAlbumsResponse(configured=False)
            try:
                release_groups = await lb_repo.get_artist_top_release_groups(
                    artist_mbid, count=count
                )
                if not release_groups:
                    fallback_albums = (
                        await self._get_top_albums_from_recordings_fallback(
                            lb_repo, artist_mbid, count
                        )
                    )
                    result = TopAlbumsResponse(albums=fallback_albums)
                else:
                    try:
                        (
                            library_album_mbids,
                            requested_album_mbids,
                        ) = await asyncio.gather(
                            self._library_repo.get_library_mbids(),
                            self._library_repo.get_requested_mbids(),
                        )
                    except Exception as e:  # noqa: BLE001
                        logger.warning(
                            "Failed to load Lidarr album MBIDs for %s: %s(%s)",
                            artist_mbid[:8],
                            type(e).__name__,
                            e,
                        )
                        library_album_mbids, requested_album_mbids = set(), set()

                    library_album_mbids = {
                        mbid.lower()
                        for mbid in library_album_mbids
                        if isinstance(mbid, str)
                    }
                    requested_album_mbids = {
                        mbid.lower()
                        for mbid in requested_album_mbids
                        if isinstance(mbid, str)
                    }

                    albums = [
                        TopAlbum(
                            release_group_mbid=rg.release_group_mbid,
                            title=rg.release_group_name,
                            artist_name=rg.artist_name,
                            listen_count=rg.listen_count,
                            in_library=rg.release_group_mbid.lower()
                            in library_album_mbids
                            if rg.release_group_mbid
                            else False,
                            requested=rg.release_group_mbid.lower()
                            in requested_album_mbids
                            if rg.release_group_mbid
                            else False,
                        )
                        for rg in release_groups
                    ]
                    result = TopAlbumsResponse(albums=albums)
            except CircuitOpenError:
                logger.warning("Circuit open for top albums %s", artist_mbid[:8])
                result = TopAlbumsResponse(albums=[])
                lb_unavailable = True
            except Exception as e:  # noqa: BLE001
                logger.warning(
                    "Failed to get top albums for %s: %s(%s)",
                    artist_mbid[:8],
                    type(e).__name__,
                    e,
                )
                try:
                    fallback_albums = (
                        await self._get_top_albums_from_recordings_fallback(
                            lb_repo, artist_mbid, count
                        )
                    )
                    result = TopAlbumsResponse(albums=fallback_albums)
                except Exception as fallback_error:  # noqa: BLE001
                    logger.warning(
                        "Top albums fallback from recordings failed for %s: %s(%s)",
                        artist_mbid[:8],
                        type(fallback_error).__name__,
                        fallback_error,
                    )
                    result = TopAlbumsResponse(albums=[])

        # LB popularity (top-release-groups) is disabled/auth-gated upstream (2026-07);
        # fall back to Last.fm so the section still fills
        if effective_source == "listenbrainz" and not result.albums:
            fb = await self._lastfm_fallback("top_albums", user_id, artist_mbid, count)
            if fb is not None and fb.albums:
                result, lb_unavailable = fb, False

        # Degraded empty must use short TTL, not healthy-empty TTL (NEW-CPU-02)
        _ctx = try_get_degradation_context()
        _degraded = (
            (_ctx is not None and _ctx.has_degradation())
            or lb_popularity_degraded()
            or lb_unavailable
        )
        if _degraded and not result.albums:
            await mb_publish_if_current(
                source_context,
                lambda: self._cache.set(
                    cache_key, result, ttl_seconds=CIRCUIT_OPEN_CACHE_TTL
                ),
            )
            return result
        in_library = await self._is_library_artist(artist_mbid)
        empty_ttl = (
            60
            if effective_source == "listenbrainz"
            else self._get_empty_discovery_ttl()
        )
        ttl = self._get_discovery_ttl(in_library) if result.albums else empty_ttl
        await mb_publish_if_current(
            source_context,
            lambda: self._cache.set(cache_key, result, ttl_seconds=ttl),
        )
        return result

    async def _get_top_albums_from_recordings_fallback(
        self,
        lb_repo: ListenBrainzRepositoryProtocol,
        artist_mbid: str,
        count: int,
    ) -> list[TopAlbum]:
        recordings = await lb_repo.get_artist_top_recordings(
            artist_mbid,
            count=max(count * 8, 80),
        )
        if not recordings:
            return []

        try:
            library_album_mbids, requested_album_mbids = await asyncio.gather(
                self._library_repo.get_library_mbids(),
                self._library_repo.get_requested_mbids(),
            )
        except Exception as e:  # noqa: BLE001
            logger.warning(
                "Fallback Lidarr album MBID load failed for %s: %s(%s)",
                artist_mbid[:8],
                type(e).__name__,
                e,
            )
            library_album_mbids, requested_album_mbids = set(), set()

        library_album_mbids = {
            mbid.lower() for mbid in library_album_mbids if isinstance(mbid, str)
        }
        requested_album_mbids = {
            mbid.lower() for mbid in requested_album_mbids if isinstance(mbid, str)
        }

        rg_map = await self._resolve_recording_release_groups(lb_repo, recordings)

        aggregated: dict[str, dict[str, str | int | None]] = {}
        for recording in recordings:
            release_title = (recording.release_name or "").strip()
            raw_release_mbid = (
                recording.release_mbid.strip().lower()
                if recording.release_mbid and recording.release_mbid.strip()
                else None
            )
            resolved_release_group_mbid = (
                rg_map.get(raw_release_mbid, raw_release_mbid)
                if raw_release_mbid
                else None
            )

            key = resolved_release_group_mbid or (
                f"name:{release_title.lower()}" if release_title else None
            )
            if not key:
                continue

            if key not in aggregated:
                aggregated[key] = {
                    "title": release_title or "Unknown",
                    "artist_name": recording.artist_name,
                    "listen_count": 0,
                    "release_group_mbid": resolved_release_group_mbid,
                }

            aggregated[key]["listen_count"] = int(
                aggregated[key]["listen_count"]
            ) + int(recording.listen_count)

        sorted_albums = sorted(
            aggregated.values(),
            key=lambda album: int(album["listen_count"]),
            reverse=True,
        )[:count]

        return [
            TopAlbum(
                release_group_mbid=album["release_group_mbid"]
                if isinstance(album["release_group_mbid"], str)
                else None,
                title=str(album["title"]),
                artist_name=str(album["artist_name"]),
                listen_count=int(album["listen_count"]),
                in_library=(
                    isinstance(album["release_group_mbid"], str)
                    and album["release_group_mbid"] in library_album_mbids
                ),
                requested=(
                    isinstance(album["release_group_mbid"], str)
                    and album["release_group_mbid"] in requested_album_mbids
                ),
            )
            for album in sorted_albums
        ]

    async def _is_library_artist(self, artist_mbid: str) -> bool:
        try:
            library_artist_mbids = await self._library_db.get_all_artist_mbids()
            return artist_mbid in library_artist_mbids
        except Exception:  # noqa: BLE001
            return False

    async def _resolve_precache_user_id(self) -> str | None:
        """First admin's id, used as the credential source for the global precache."""
        if self._auth_store is None:
            return None
        try:
            admin = await self._auth_store.get_first_admin()
        except Exception:  # noqa: BLE001
            return None
        return admin.id if admin else None

    async def precache_artist_discovery(
        self,
        artist_mbids: list[str],
        delay: float = 0.5,
        status_service: Any = None,
        mbid_to_name: dict[str, str] | None = None,
        generation: int = 0,
    ) -> int:
        global _discovery_precache_running
        if _discovery_precache_running:
            return 0
        if monotonic() < _precache_paused_until:
            logger.debug(
                "Discovery precache skipped: paused after repeated upstream failures"
            )
            return 0

        _discovery_precache_running = True
        try:
            return await self._do_precache_artist_discovery(
                artist_mbids,
                delay=delay,
                status_service=status_service,
                mbid_to_name=mbid_to_name,
                generation=generation,
            )
        finally:
            _discovery_precache_running = False

    async def _do_precache_artist_discovery(
        self,
        artist_mbids: list[str],
        delay: float = 0.5,
        status_service: Any = None,
        mbid_to_name: dict[str, str] | None = None,
        generation: int = 0,
    ) -> int:
        started = monotonic()
        # Precache warms a GLOBAL cache (keyed by mbid+source, not per-user), so it
        # only needs one valid set of credentials. Use the first admin's per-user
        # connection - the same identity the startup backfill seeds.
        user_id = await self._resolve_precache_user_id()
        sources: list[Literal["listenbrainz", "lastfm"]] = []
        if await self._resolve_listenbrainz(user_id) is not None:
            sources.append("listenbrainz")
        if await self._resolve_lastfm(user_id) is not None:
            sources.append("lastfm")
        if not sources:
            logger.debug("Skipping discovery pre-cache: no configured source")
            return 0

        cached_count = 0
        source_fetches = 0
        advanced = (
            self._preferences_service.get_advanced_settings()
            if self._preferences_service
            else None
        )
        discovery_concurrency = (
            getattr(advanced, "artist_discovery_precache_concurrency", 5)
            if advanced
            else 5
        )
        sem = asyncio.Semaphore(discovery_concurrency)
        counter_lock = asyncio.Lock()
        progress_counter = 0
        counted_workers: set[int] = set()

        async def process_artist(idx: int, mbid: str) -> bool:
            nonlocal cached_count, source_fetches, progress_counter
            if monotonic() < _precache_paused_until:
                return False
            # Isolated degradation context per artist task, no sibling leakage
            ctx = init_degradation_context()
            degraded = False
            has_data = False
            configured_absent = False
            had_fetch = False
            gathered_results: list[Any] = []
            try:
                async with sem:
                    if monotonic() < _precache_paused_until:
                        return False
                    for source in sources:
                        if self._workload_gate is not None:
                            await self._workload_gate.wait_until_available()
                        similar_key = self._build_cache_key(
                            "similar", mbid, DEFAULT_SIMILAR_COUNT, source
                        )
                        songs_key = self._build_cache_key(
                            "top_songs", mbid, DEFAULT_TOP_SONGS_COUNT, source
                        )
                        albums_key = self._build_cache_key(
                            "top_albums", mbid, DEFAULT_TOP_ALBUMS_COUNT, source
                        )

                        has_all = (
                            await self._cache.get(similar_key) is not None
                            and await self._cache.get(songs_key) is not None
                            and await self._cache.get(albums_key) is not None
                        )
                        if has_all:
                            continue
                        had_fetch = True
                        results = await asyncio.gather(
                            self.get_similar_artists(
                                mbid,
                                count=DEFAULT_SIMILAR_COUNT,
                                source=source,
                                user_id=user_id,
                            ),
                            self.get_top_songs(
                                mbid,
                                count=DEFAULT_TOP_SONGS_COUNT,
                                source=source,
                                user_id=user_id,
                            ),
                            self.get_top_albums(
                                mbid,
                                count=DEFAULT_TOP_ALBUMS_COUNT,
                                source=source,
                                user_id=user_id,
                            ),
                            return_exceptions=True,
                        )
                        gathered_results.extend(results)
                        # Configured absence is per-section source selection, not response attr.
                        # For precache, sources already reflects configured Last.fm/ListenBrainz,
                        # so if we fetched, the source was configured; has_all already handled.
                        # Keep configured_absent False for real precache units.
                        configured_absent = False
                        for r in results:
                            if isinstance(r, Exception):
                                continue
                            sa = getattr(r, "similar_artists", None)
                            if isinstance(sa, list) and len(sa) > 0:
                                has_data = True
                            sg = getattr(r, "songs", None)
                            if isinstance(sg, list) and len(sg) > 0:
                                has_data = True
                            al = getattr(r, "albums", None)
                            if isinstance(al, list) and len(al) > 0:
                                has_data = True
                        if ctx.has_degradation() or lb_popularity_degraded():
                            degraded = True
                if not had_fetch:
                    # Already cached, treat as success (no degradation)
                    outcome: _PrecacheOutcome = "healthy_data"
                else:
                    outcome = _classify_precache_outcome(
                        gathered_results, degraded, has_data, configured_absent
                    )
                # Metrics and success/failure handling
                if outcome == "healthy_empty":
                    _precache_metrics.increment("precache_healthy_empty")
                elif outcome == "degraded":
                    _precache_metrics.increment("precache_degraded")
                # Fallback data is usable but primary degradation observable
                if degraded and has_data and outcome == "healthy_data":
                    _precache_metrics.increment("precache_degraded")
                if outcome in ("healthy_data", "healthy_empty", "configured_absent"):
                    if delay > 0:
                        await asyncio.sleep(delay)
                    async with counter_lock:
                        if outcome != "configured_absent":
                            cached_count += 1
                        progress_counter += 1
                        local_progress = progress_counter
                        counted_workers.add(idx)
                    if status_service:
                        artist_name = (mbid_to_name or {}).get(mbid, mbid[:8])
                        await status_service.update_progress(
                            local_progress,
                            current_item=artist_name,
                            generation=generation,
                        )
                    _record_precache_unit_success()
                    return True
                # degraded or failed
                if delay > 0:
                    await asyncio.sleep(delay)
                async with counter_lock:
                    progress_counter += 1
                    local_progress = progress_counter
                    counted_workers.add(idx)
                if status_service:
                    artist_name = (mbid_to_name or {}).get(mbid, mbid[:8])
                    await status_service.update_progress(
                        local_progress, current_item=artist_name, generation=generation
                    )
                _record_precache_unit_failure()
                if outcome == "degraded":
                    logger.debug("Discovery precache degraded for %s", mbid[:8])
                return False
            except Exception as e:  # noqa: BLE001
                _record_precache_unit_failure()
                logger.warning("Failed to precache discovery for %s: %s", mbid[:8], e)
                async with counter_lock:
                    progress_counter += 1
                    local_progress = progress_counter
                    counted_workers.add(idx)
                if status_service:
                    artist_name = (mbid_to_name or {}).get(mbid, mbid[:8])
                    await status_service.update_progress(
                        local_progress, current_item=artist_name, generation=generation
                    )
                return False
            finally:
                clear_degradation_context()

        async def process_artist_with_timeout(idx: int, mbid: str) -> bool:
            nonlocal progress_counter
            try:
                return await asyncio.wait_for(
                    process_artist(idx, mbid), timeout=_DISCOVERY_WORKER_TIMEOUT
                )
            except asyncio.TimeoutError:
                _record_precache_unit_failure()
                logger.warning(
                    "Discovery timed out for %s after %ds",
                    mbid[:8],
                    _DISCOVERY_WORKER_TIMEOUT,
                )
                async with counter_lock:
                    if idx not in counted_workers:
                        progress_counter += 1
                        counted_workers.add(idx)
                    local_progress = progress_counter
                if status_service:
                    artist_name = (mbid_to_name or {}).get(mbid, mbid[:8])
                    await status_service.update_progress(
                        local_progress,
                        current_item=f"{artist_name} (timed out)",
                        generation=generation,
                    )
                return False

        chunk = max(discovery_concurrency * 4, 20)
        for i in range(0, len(artist_mbids), chunk):
            if monotonic() < _precache_paused_until:
                break
            if status_service and status_service.is_cancelled():
                break
            batch = artist_mbids[i : i + chunk]
            batch_tasks = [
                asyncio.create_task(process_artist_with_timeout(i + j, mbid))
                for j, mbid in enumerate(batch)
            ]
            if batch_tasks:
                await asyncio.gather(*batch_tasks, return_exceptions=True)

        logger.info(
            "Artist discovery precache complete: artists=%d source_fetches=%d duration=%.2fs",
            cached_count,
            source_fetches,
            monotonic() - started,
        )
        return cached_count

    async def _resolve_release_groups(self, release_ids: list[str]) -> dict[str, str]:
        if not release_ids:
            return {}

        unique_ids = list(dict.fromkeys(release_ids))
        tasks = [
            self._mb_repo.get_release_group_id_from_release(rid) for rid in unique_ids
        ]
        results = await asyncio.gather(*tasks, return_exceptions=True)

        rg_map = {}
        errors = 0
        for rid, rg_id in zip(unique_ids, results):
            if isinstance(rg_id, Exception):
                errors += 1
                logger.warning(f"Resolution exception for {rid}: {rg_id}")
            elif isinstance(rg_id, str) and rg_id:
                rg_map[rid] = rg_id
            else:
                errors += 1
                logger.warning(f"Resolution returned None/empty for {rid}")

        return rg_map

    async def _resolve_recording_release_groups(
        self,
        lb_repo: ListenBrainzRepositoryProtocol,
        recordings: list,
    ) -> dict[str, str]:
        """Map release MBIDs to release groups, batching through ListenBrainz first."""
        recording_ids = [
            recording.recording_mbid
            for recording in recordings
            if recording.recording_mbid
        ]
        try:
            recording_to_rg = await lb_repo.get_recording_release_groups_batch(
                recording_ids
            )
            if not isinstance(recording_to_rg, dict):
                recording_to_rg = {}
        except Exception:  # noqa: BLE001 - MusicBrainz remains the fallback
            recording_to_rg = {}

        release_to_rg = {
            recording.release_mbid: recording_to_rg[recording.recording_mbid]
            for recording in recordings
            if recording.release_mbid
            and recording.recording_mbid
            and recording.recording_mbid in recording_to_rg
        }
        unresolved_release_ids = [
            recording.release_mbid
            for recording in recordings
            if recording.release_mbid and recording.release_mbid not in release_to_rg
        ]
        if unresolved_release_ids:
            release_to_rg.update(
                await self._resolve_release_groups(unresolved_release_ids)
            )
        return release_to_rg

    async def _get_similar_artists_lastfm(
        self,
        lastfm_repo: Optional[LastFmRepositoryProtocol],
        artist_mbid: str,
        count: int,
    ) -> SimilarArtistsResponse:
        if lastfm_repo is None:
            return SimilarArtistsResponse(
                similar_artists=[], source="lastfm", configured=False
            )

        try:
            similar = await lastfm_repo.get_similar_artists(
                artist="", mbid=artist_mbid, limit=count
            )
            library_artist_mbids = await self._library_db.get_all_artist_mbids()

            artists = [
                SimilarArtist(
                    musicbrainz_id=a.mbid or "",
                    name=a.name,
                    listen_count=0,
                    in_library=bool(a.mbid and a.mbid in library_artist_mbids),
                )
                for a in similar[:count]
                if a.mbid
            ]
            return SimilarArtistsResponse(similar_artists=artists, source="lastfm")
        except Exception as e:
            logger.warning(
                "Last.fm similar artists API error for %s: %s", artist_mbid[:8], e
            )
            raise

    async def _get_top_songs_lastfm(
        self,
        lastfm_repo: Optional[LastFmRepositoryProtocol],
        artist_mbid: str,
        count: int,
    ) -> TopSongsResponse:
        if lastfm_repo is None:
            return TopSongsResponse(songs=[], source="lastfm", configured=False)

        try:
            tracks = await lastfm_repo.get_artist_top_tracks(
                artist="", mbid=artist_mbid, limit=count
            )
            trimmed = tracks[:count]

            songs = [
                TopSong(
                    recording_mbid=t.mbid,
                    title=t.name,
                    artist_name=t.artist_name,
                    release_group_mbid=None,
                    original_release_mbid=None,
                    release_name=None,
                    listen_count=t.playcount,
                )
                for t in trimmed
            ]
            return TopSongsResponse(songs=songs, source="lastfm")
        except Exception as e:
            logger.warning("Last.fm top songs API error for %s: %s", artist_mbid[:8], e)
            raise

    async def _get_top_albums_lastfm(
        self,
        lastfm_repo: Optional[LastFmRepositoryProtocol],
        artist_mbid: str,
        count: int,
    ) -> TopAlbumsResponse:
        if lastfm_repo is None:
            return TopAlbumsResponse(albums=[], source="lastfm", configured=False)

        try:
            lfm_albums = await lastfm_repo.get_artist_top_albums(
                artist="", mbid=artist_mbid, limit=count
            )

            library_album_mbids, requested_album_mbids = await asyncio.gather(
                self._library_repo.get_library_mbids(),
                self._library_repo.get_requested_mbids(),
            )

            trimmed = lfm_albums[:count]
            try:
                # QW1 Part B: synchronous leg of the user-facing top-albums
                # response; USER_INITIATED avoids the 2 s inactivity gate that
                # this same page load keeps resetting (BACKGROUND_SYNC/
                # PREFETCH_VISIBLE both route into the gated branch).
                release_groups = await self._mb_repo.get_release_groups_by_artist(
                    artist_mbid, limit=100, priority=RequestPriority.USER_INITIATED
                )
            except Exception as exc:  # noqa: BLE001 - optional canonicalization must degrade
                logger.warning(
                    "Could not canonicalize Last.fm albums for %s: %s",
                    artist_mbid[:8],
                    type(exc).__name__,
                )
                release_groups = []
            release_group_ids = {
                str(group.get("id", "")).strip().lower()
                for group in release_groups
                if group.get("id")
            }
            release_groups_by_title: dict[str, str | None] = {}
            for group in release_groups:
                release_group_mbid = str(group.get("id", "")).strip().lower()
                title_key = self._normalized_album_title(group.get("title"))
                if not release_group_mbid or not title_key:
                    continue
                previous = release_groups_by_title.get(title_key)
                if previous is None and title_key not in release_groups_by_title:
                    release_groups_by_title[title_key] = release_group_mbid
                elif previous != release_group_mbid:
                    release_groups_by_title[title_key] = None

            library_album_mbids = {
                mbid.strip().lower()
                for mbid in library_album_mbids
                if isinstance(mbid, str) and mbid.strip()
            }
            requested_album_mbids = {
                mbid.strip().lower()
                for mbid in requested_album_mbids
                if isinstance(mbid, str) and mbid.strip()
            }

            albums = []
            seen_release_groups: set[str] = set()
            for a in trimmed:
                raw_mbid = a.mbid.strip().lower() if a.mbid and a.mbid.strip() else None
                title_match = None
                for title_key in self._album_title_candidates(a.name):
                    if title_key not in release_groups_by_title:
                        continue
                    title_match = release_groups_by_title[title_key]
                    break
                release_group_mbid = (
                    raw_mbid
                    if raw_mbid in release_group_ids
                    else title_match or raw_mbid
                )
                if release_group_mbid and release_group_mbid in seen_release_groups:
                    continue
                if release_group_mbid:
                    seen_release_groups.add(release_group_mbid)
                albums.append(
                    TopAlbum(
                        release_group_mbid=release_group_mbid,
                        title=a.name,
                        artist_name=a.artist_name,
                        listen_count=a.playcount,
                        in_library=(
                            release_group_mbid in library_album_mbids
                            if release_group_mbid
                            else False
                        ),
                        requested=(
                            release_group_mbid in requested_album_mbids
                            if release_group_mbid
                            else False
                        ),
                    )
                )
            return TopAlbumsResponse(albums=albums, source="lastfm")
        except Exception as e:
            logger.warning(
                "Last.fm top albums API error for %s: %s", artist_mbid[:8], e
            )
            raise

    @staticmethod
    def _normalized_album_title(title: object) -> str:
        if not isinstance(title, str):
            return ""
        return " ".join(title.casefold().split())

    @classmethod
    def _album_title_candidates(cls, title: object) -> list[str]:
        normalized = cls._normalized_album_title(title)
        if not normalized:
            return []
        candidates = [normalized]
        for suffix in (
            " (deluxe)",
            " (deluxe edition)",
            " [deluxe]",
            " [deluxe edition]",
        ):
            if normalized.endswith(suffix):
                candidates.append(normalized[: -len(suffix)].rstrip())
                break
        return candidates
