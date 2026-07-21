import asyncio
import logging
import random
import uuid
from typing import Any, TYPE_CHECKING

from api.v1.schemas.discover import (
    DiscoverQueueItemLight,
    DiscoverQueueResponse,
    DiscoverIgnoredRelease,
    QueueSettings,
)
from infrastructure.persistence import LibraryDB, MBIDStore
from repositories.protocols import (
    ListenBrainzRepositoryProtocol,
    JellyfinRepositoryProtocol,
    MusicBrainzRepositoryProtocol,
    LastFmRepositoryProtocol,
)
from repositories.listenbrainz_models import ListenBrainzArtist
from services.home.integration_helpers import resolve_source_value
from services.per_user_client_factory import PerUserClientFactory
from infrastructure.persistence.user_listening_prefs_store import (
    UserListeningPrefsStore,
)
from services.discover.integration_helpers import IntegrationHelpers
from services.discover.mbid_resolution_service import MbidResolutionService
from services.discover.queue_strategies import (
    build_similar_artist_pools,
    discover_by_genres,
    get_artist_deep_cuts,
    get_trending_filler,
    interleave_at_positions,
    round_robin_dedup_select,
)

logger = logging.getLogger(__name__)

if TYPE_CHECKING:
    from services.native.library_ownership_service import LibraryOwnershipService

VARIOUS_ARTISTS_MBID = "89ad4ac3-39f7-470e-963a-56509c546377"


class DiscoverQueueService:
    def __init__(
        self,
        listenbrainz_repo: ListenBrainzRepositoryProtocol,
        jellyfin_repo: JellyfinRepositoryProtocol,
        musicbrainz_repo: MusicBrainzRepositoryProtocol,
        integration: IntegrationHelpers,
        mbid_resolution: MbidResolutionService,
        library_db: LibraryDB | None = None,
        mbid_store: MBIDStore | None = None,
        lastfm_repo: LastFmRepositoryProtocol | None = None,
        client_factory: PerUserClientFactory | None = None,
        listening_prefs_store: UserListeningPrefsStore | None = None,
        ownership_service: "LibraryOwnershipService | None" = None,
    ) -> None:
        self._lb_repo = listenbrainz_repo
        self._jf_repo = jellyfin_repo
        self._mb_repo = musicbrainz_repo
        self._integration = integration
        self._mbid = mbid_resolution
        self._library_db = library_db
        self._mbid_store = mbid_store
        self._lfm_repo = lastfm_repo
        self._client_factory = client_factory
        self._prefs_store = listening_prefs_store
        self._ownership = ownership_service

    async def _resolve_user_music(self, user_id: str, source: str | None):
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
            primary_source = (await self._prefs_store.get(user_id)).primary_music_source
        lb_enabled = lb_client is not None
        lfm_enabled = lfm_client is not None
        resolved = resolve_source_value(source, primary_source, lb_enabled, lfm_enabled)
        return (
            lb_client,
            lfm_client,
            lb_username,
            lfm_username,
            lb_enabled,
            lfm_enabled,
            resolved,
        )

    async def build_queue(
        self, user_id: str, count: int | None = None
    ) -> DiscoverQueueResponse:
        qs = self._integration.get_queue_settings()
        if count is None:
            count = qs.queue_size
        # queue strategies follow the user's primary source
        (
            lb_client,
            _lfm_client,
            username,
            lfm_username,
            lb_enabled,
            lfm_enabled,
            resolved_source,
        ) = await self._resolve_user_music(user_id, None)
        jf_enabled = self._integration.is_jellyfin_enabled()
        library_configured = self._integration.is_library_configured()

        ignored_mbids: set[str] = set()
        if self._mbid_store:
            try:
                ignored_mbids = await self._mbid_store.get_ignored_release_mbids(
                    user_id
                )
            except Exception:  # noqa: BLE001
                logger.warning("Failed to load ignored release MBIDs from cache")

        library_album_mbids: set[str] = set()
        listened_release_group_mbids = (
            await self._mbid.get_user_listened_release_group_mbids(
                lb_enabled,
                username,
                resolved_source,
            )
        )

        has_services = lb_enabled or jf_enabled or (lfm_enabled and lfm_username)
        if has_services:
            items = await self._build_personalized_queue(
                count,
                lb_enabled,
                username,
                jf_enabled,
                ignored_mbids,
                library_album_mbids,
                listened_release_group_mbids,
                resolved_source=resolved_source,
                lfm_enabled=lfm_enabled,
                lfm_username=lfm_username,
                lb_client=lb_client,
            )
        else:
            items = await self._build_anonymous_queue(
                count,
                ignored_mbids,
                library_album_mbids,
                resolved_source=resolved_source,
            )

        candidate_owned = await self._mbid.get_library_album_mbids(
            library_configured,
            [item.release_group_mbid for item in items],
        )
        if self._ownership is not None:
            from services.native.library_ownership_service import (
                AlbumOwnershipCandidate,
            )

            projections = await self._ownership.project_albums(
                [
                    AlbumOwnershipCandidate(
                        release_group_mbid=item.release_group_mbid,
                        title=item.album_name,
                        album_artist=item.artist_name,
                    )
                    for item in items
                ]
            )
            for item, projection in zip(items, projections):
                item.in_library = projection.owned
                if projection.owned:
                    candidate_owned.add(item.release_group_mbid.casefold())

        items = [
            item
            for item in items
            if item.release_group_mbid.casefold() not in candidate_owned
        ]

        return DiscoverQueueResponse(
            items=items,
            queue_id=str(uuid.uuid4()),
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
        # LB top-artists rely on the client's own identity, so use the per-user client.
        seed_lb_repo = lb_client or self._lb_repo

        if (
            resolved_source == "lastfm"
            and lfm_enabled
            and lfm_username
            and self._lfm_repo
        ):
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

        if resolved_source != "lastfm" and len(seeds) < 3 and lb_enabled and username:
            # Prefer recent listening, fall back to broader windows for quiet-but-real history.
            for range_ in ("this_week", "this_month", "this_year", "all_time"):
                if len(seeds) >= 3:
                    break
                try:
                    artists = await seed_lb_repo.get_user_top_artists(
                        count=10, range_=range_
                    )
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
                            seeds.append(
                                ListenBrainzArtist(
                                    artist_name=item.artist_name or item.name,
                                    listen_count=item.play_count,
                                    artist_mbids=[mbid],
                                )
                            )
                            seen_mbids.add(mbid)
                except Exception as e:  # noqa: BLE001
                    logger.warning(f"Failed to get Jellyfin seed artists: {e}")
                    continue

        return seeds

    async def validate_queue_mbids(self, mbids: list[str]) -> list[str]:
        library_mbids: set[str] = set()
        if self._library_db:
            try:
                library_mbids = await self._library_db.existing_library_mbids(mbids)
            except Exception:  # noqa: BLE001
                logger.warning(
                    "Failed to load album MBIDs from library cache for validation"
                )
        if not library_mbids:
            try:
                library_configured = self._integration.is_library_configured()
                if library_configured:
                    library_mbids = await self._mbid.get_library_album_mbids(
                        True, mbids
                    )
            except Exception:  # noqa: BLE001
                logger.warning("Failed to load album MBIDs from Lidarr for validation")
        if not library_mbids:
            return mbids
        lowered_library = {lm.lower() for lm in library_mbids}
        return [m for m in mbids if m.lower() in lowered_library]

    async def ignore_release(
        self,
        user_id: str,
        release_group_mbid: str,
        artist_mbid: str,
        release_name: str,
        artist_name: str,
    ) -> None:
        if self._mbid_store:
            await self._mbid_store.add_ignored_release(
                user_id, release_group_mbid, artist_mbid, release_name, artist_name
            )

    async def get_ignored_releases(self, user_id: str) -> list[DiscoverIgnoredRelease]:
        if self._mbid_store:
            rows = await self._mbid_store.get_ignored_releases(user_id)
            return [DiscoverIgnoredRelease(**row) for row in rows]
        return []

    async def _build_lb_similar_seed_pools(
        self,
        seeds: list[ListenBrainzArtist],
        excluded_mbids: set[str],
        qs: QueueSettings,
    ) -> list[list[DiscoverQueueItemLight]]:
        return await build_similar_artist_pools(
            seeds,
            excluded_mbids,
            qs.similar_artists_limit,
            qs.albums_per_similar,
            lb_repo=self._lb_repo,
            mbid_svc=self._mbid,
        )

    async def _strategy_lb_genre_discovery(
        self,
        username: str,
        excluded_mbids: set[str],
    ) -> list[DiscoverQueueItemLight]:
        try:
            genre_activity = await self._lb_repo.get_user_genre_activity(username)
        except Exception:  # noqa: BLE001
            logger.warning("Failed to fetch user genre activity from ListenBrainz")
            return []

        if not genre_activity:
            return []

        genres = [g.genre for g in genre_activity if getattr(g, "genre", None)]
        return await discover_by_genres(
            genres,
            excluded_mbids,
            mb_repo=self._mb_repo,
            mbid_svc=self._mbid,
        )

    async def _strategy_lb_fresh_releases(
        self,
        username: str,
        excluded_mbids: set[str],
    ) -> list[DiscoverQueueItemLight]:
        try:
            fresh_releases = await self._lb_repo.get_user_fresh_releases(username)
        except Exception:  # noqa: BLE001
            logger.warning("Failed to fetch fresh releases from ListenBrainz")
            return []

        if not fresh_releases:
            return []

        items: list[DiscoverQueueItemLight] = []
        seen: set[str] = set()
        for release in fresh_releases:
            if not isinstance(release, dict):
                continue
            rg_mbid = self._mbid.normalize_mbid(release.get("release_group_mbid"))
            if not rg_mbid:
                continue
            if rg_mbid in excluded_mbids or rg_mbid in seen:
                continue

            artist_mbids = release.get("artist_mbids")
            first_artist_mbid = ""
            if isinstance(artist_mbids, list) and artist_mbids:
                first_artist_mbid = self._mbid.normalize_mbid(artist_mbids[0]) or ""

            album_name = (
                release.get("release_name") or release.get("title") or "Unknown"
            )
            artist_name = (
                release.get("artist_credit_name")
                or release.get("artist_name")
                or "Unknown"
            )
            items.append(
                self._mbid.make_queue_item(
                    release_group_mbid=rg_mbid,
                    album_name=album_name,
                    artist_name=artist_name,
                    artist_mbid=first_artist_mbid,
                    reason="New release for you",
                )
            )
            seen.add(rg_mbid)
        return items

    async def _strategy_lb_loved_artists(
        self,
        username: str,
        excluded_mbids: set[str],
        albums_per_artist: int,
    ) -> list[DiscoverQueueItemLight]:
        try:
            loved = await self._lb_repo.get_user_loved_recordings(
                username=username,
                count=50,
            )
        except Exception:  # noqa: BLE001
            logger.warning("Failed to fetch loved recordings from ListenBrainz")
            return []

        artist_mbids: list[str] = []
        seen_artists: set[str] = set()
        for recording in loved:
            mbids = getattr(recording, "artist_mbids", None) or []
            if not mbids:
                continue
            normalized = self._mbid.normalize_mbid(mbids[0])
            if not normalized or normalized in seen_artists:
                continue
            artist_mbids.append(normalized)
            seen_artists.add(normalized)
            if len(artist_mbids) >= 6:
                break

        if not artist_mbids:
            return []

        results = await asyncio.gather(
            *[
                self._lb_repo.get_artist_top_release_groups(
                    artist_mbid, count=albums_per_artist
                )
                for artist_mbid in artist_mbids
            ],
            return_exceptions=True,
        )

        items: list[DiscoverQueueItemLight] = []
        seen_rg_mbids: set[str] = set()
        for artist_mbid, result in zip(artist_mbids, results):
            if isinstance(result, Exception):
                continue
            for rg in result:
                rg_mbid = self._mbid.normalize_mbid(rg.release_group_mbid)
                if not rg_mbid:
                    continue
                if rg_mbid in excluded_mbids or rg_mbid in seen_rg_mbids:
                    continue
                items.append(
                    self._mbid.make_queue_item(
                        release_group_mbid=rg_mbid,
                        album_name=rg.release_group_name,
                        artist_name=rg.artist_name,
                        artist_mbid=artist_mbid,
                        reason="From an artist you love",
                    )
                )
                seen_rg_mbids.add(rg_mbid)
        return items

    async def _strategy_lb_top_artist_deep_cuts(
        self,
        username: str,
        excluded_mbids: set[str],
        listened_mbids: set[str],
        albums_per_artist: int,
    ) -> list[DiscoverQueueItemLight]:
        return await get_artist_deep_cuts(
            username,
            excluded_mbids,
            listened_mbids,
            albums_per_artist,
            lb_repo=self._lb_repo,
            mbid_svc=self._mbid,
        )

    async def _build_personalized_queue(
        self,
        count: int,
        lb_enabled: bool,
        username: str | None,
        jf_enabled: bool,
        ignored_mbids: set[str],
        library_album_mbids: set[str],
        listened_release_group_mbids: set[str],
        resolved_source: str = "listenbrainz",
        lfm_enabled: bool = False,
        lfm_username: str | None = None,
        lb_client: Any = None,
    ) -> list[DiscoverQueueItemLight]:
        seed_artists = await self._get_seed_artists(
            lb_enabled,
            username,
            jf_enabled,
            resolved_source=resolved_source,
            lfm_enabled=lfm_enabled,
            lfm_username=lfm_username,
            lb_client=lb_client,
        )
        if not seed_artists:
            return await self._build_anonymous_queue(
                count,
                ignored_mbids,
                library_album_mbids,
                resolved_source=resolved_source,
            )

        qs = self._integration.get_queue_settings()
        # during a genuine LB popularity outage, source albums from Last.fm even when the
        # user's primary is ListenBrainz (the LB similar-artist seeds above still work)
        from repositories.listenbrainz_repository import lb_popularity_degraded

        use_lastfm = (
            (resolved_source == "lastfm" or lb_popularity_degraded())
            and lfm_enabled
            and self._lfm_repo is not None
        )
        seeds = seed_artists[: qs.seed_artists]
        wildcard_slots = qs.wildcard_slots
        personalized_target = max(count - wildcard_slots, 0)
        seed_target = max(4, (personalized_target // max(len(seeds), 1)) + 3)
        excluded_mbids = ignored_mbids | library_album_mbids
        mbid_resolution_cache: dict[str, str | None] = {}

        candidate_pools: list[list[DiscoverQueueItemLight]] = []
        if use_lastfm:
            candidate_pools = [[] for _ in range(len(seeds))]

            async def _process_seed_lastfm(i: int, seed: Any) -> None:
                seed_mbid = seed.artist_mbids[0] if seed.artist_mbids else None
                if not seed_mbid:
                    return
                try:
                    similar_raw = await self._lfm_repo.get_similar_artists(
                        seed.artist_name,
                        mbid=seed_mbid,
                        limit=qs.similar_artists_limit,
                    )
                    valid_sims = [
                        sim
                        for sim in similar_raw
                        if self._mbid.normalize_mbid(sim.mbid)
                        and self._mbid.normalize_mbid(sim.mbid) != VARIOUS_ARTISTS_MBID
                    ]
                    album_fetch_results = await asyncio.gather(
                        *[
                            self._lfm_repo.get_artist_top_albums(
                                sim.name,
                                mbid=sim.mbid,
                                limit=qs.albums_per_similar,
                            )
                            for sim in valid_sims
                        ],
                        return_exceptions=True,
                    )
                    sim_albums_map: list[tuple[Any, list]] = []
                    for sim, result in zip(valid_sims, album_fetch_results):
                        if isinstance(result, Exception):
                            logger.debug(
                                "Failed to get Last.fm albums for %s: %s",
                                sim.name,
                                result,
                            )
                            continue
                        sim_albums_map.append((sim, result))
                    candidate_pools[i] = await self._mbid.lastfm_albums_to_queue_items(
                        sim_albums_map,
                        exclude=excluded_mbids,
                        target=seed_target,
                        reason=f"Similar to {seed.artist_name}",
                        resolver_cache=mbid_resolution_cache,
                        use_album_artist_name=False,
                    )
                except Exception as e:  # noqa: BLE001
                    logger.debug(
                        f"Failed to get similar artists for seed {seed_mbid[:8]}: {e}"
                    )

            await asyncio.gather(
                *[_process_seed_lastfm(i, seed) for i, seed in enumerate(seeds)]
            )
        else:
            deep_cut_excluded = excluded_mbids | listened_release_group_mbids
            strategy_names = [
                "similar_seeds",
                "genre_discovery",
                "fresh_releases",
                "loved_artists",
                "deep_cuts",
            ]
            strategy_results = await asyncio.gather(
                self._build_lb_similar_seed_pools(seeds, excluded_mbids, qs),
                self._strategy_lb_genre_discovery(username or "", excluded_mbids),
                self._strategy_lb_fresh_releases(username or "", excluded_mbids),
                self._strategy_lb_loved_artists(
                    username or "",
                    excluded_mbids,
                    qs.albums_per_similar,
                ),
                self._strategy_lb_top_artist_deep_cuts(
                    username or "",
                    deep_cut_excluded,
                    listened_release_group_mbids,
                    qs.albums_per_similar,
                ),
                return_exceptions=True,
            )

            similar_seed_pools = strategy_results[0]
            if isinstance(similar_seed_pools, list):
                candidate_pools.extend(similar_seed_pools)
            elif isinstance(similar_seed_pools, Exception):
                logger.warning("Strategy similar_seeds FAILED: %s", similar_seed_pools)

            for idx, strategy_result in enumerate(strategy_results[1:], start=1):
                name = strategy_names[idx]
                if isinstance(strategy_result, Exception):
                    logger.warning("Strategy %s FAILED: %s", name, strategy_result)
                    continue
                if strategy_result:
                    candidate_pools.append(strategy_result)

        personalized = self._round_robin_select(candidate_pools, personalized_target)
        seen_mbids = {item.release_group_mbid.lower() for item in personalized}

        wildcard_count = max(wildcard_slots, count - len(personalized))
        wildcards = await self._get_wildcard_albums(
            wildcard_count,
            ignored_mbids,
            library_album_mbids,
            seen_mbids,
            resolved_source=resolved_source,
        )
        queue_items = self._interleave_wildcards(personalized, wildcards)

        if len(queue_items) < count:
            top_up_seen = {item.release_group_mbid.lower() for item in queue_items}
            top_up = await self._get_wildcard_albums(
                count - len(queue_items),
                ignored_mbids,
                library_album_mbids,
                top_up_seen,
                resolved_source=resolved_source,
            )
            queue_items.extend(top_up)

        return queue_items[:count]

    def _round_robin_select(
        self, pools: list[list[DiscoverQueueItemLight]], count: int
    ) -> list[DiscoverQueueItemLight]:
        return round_robin_dedup_select(pools, count)

    async def _get_wildcard_albums(
        self,
        count: int,
        ignored_mbids: set[str],
        library_album_mbids: set[str],
        seen_mbids: set[str] | None = None,
        resolved_source: str = "listenbrainz",
    ) -> list[DiscoverQueueItemLight]:
        return await get_trending_filler(
            count,
            ignored_mbids,
            library_album_mbids,
            seen_mbids,
            resolved_source,
            lb_repo=self._lb_repo,
            mb_repo=self._mb_repo,
            mbid_svc=self._mbid,
            lfm_repo=self._lfm_repo,
            is_lastfm_enabled=self._integration.is_lastfm_enabled(),
        )

    def _interleave_wildcards(
        self,
        personalized: list[DiscoverQueueItemLight],
        wildcards: list[DiscoverQueueItemLight],
    ) -> list[DiscoverQueueItemLight]:
        return interleave_at_positions(personalized, wildcards)

    async def _build_anonymous_queue(
        self,
        count: int,
        ignored_mbids: set[str],
        library_album_mbids: set[str],
        resolved_source: str = "listenbrainz",
    ) -> list[DiscoverQueueItemLight]:
        items: list[DiscoverQueueItemLight] = []
        use_lastfm = (
            resolved_source == "lastfm"
            and self._integration.is_lastfm_enabled()
            and self._lfm_repo is not None
        )
        exclude = ignored_mbids | library_album_mbids

        try:
            if use_lastfm:
                top_artists = await self._lfm_repo.get_global_top_artists(limit=15)
                random.shuffle(top_artists)
                valid_artists = [
                    a
                    for a in top_artists
                    if self._mbid.normalize_mbid(a.mbid) != VARIOUS_ARTISTS_MBID
                ]
                album_fetch_results = await asyncio.gather(
                    *[
                        self._lfm_repo.get_artist_top_albums(
                            a.name, mbid=a.mbid, limit=3
                        )
                        for a in valid_artists
                    ],
                    return_exceptions=True,
                )
                artist_albums_pairs: list[tuple[Any, list]] = []
                for artist, result in zip(valid_artists, album_fetch_results):
                    if isinstance(result, Exception):
                        continue
                    artist_albums_pairs.append((artist, result))
                items = await self._mbid.lastfm_albums_to_queue_items(
                    artist_albums_pairs,
                    exclude=exclude,
                    target=count,
                    reason="Trending on Last.fm",
                    is_wildcard=True,
                )
            else:
                trending = await self._lb_repo.get_sitewide_top_release_groups(count=50)
                random.shuffle(trending)
                for rg in trending:
                    if len(items) >= count:
                        break
                    rg_mbid = rg.release_group_mbid
                    if not rg_mbid or rg_mbid.lower() in exclude:
                        continue
                    artist_mbid = rg.artist_mbids[0] if rg.artist_mbids else ""
                    if artist_mbid.lower() == VARIOUS_ARTISTS_MBID:
                        continue
                    items.append(
                        DiscoverQueueItemLight(
                            release_group_mbid=rg_mbid,
                            album_name=rg.release_group_name,
                            artist_name=rg.artist_name,
                            artist_mbid=artist_mbid,
                            cover_url=f"/api/v1/covers/release-group/{rg_mbid}?size=500",
                            recommendation_reason="Trending This Week",
                            is_wildcard=True,
                            in_library=False,
                        )
                    )
        except Exception as e:  # noqa: BLE001
            logger.debug(f"Failed to get trending for anonymous queue: {e}")

        if len(items) < count:
            top_up_seen = {item.release_group_mbid.lower() for item in items}
            top_up = await self._get_wildcard_albums(
                count - len(items),
                ignored_mbids,
                library_album_mbids,
                top_up_seen,
                resolved_source=resolved_source,
            )
            items.extend(top_up)

        return items[:count]
