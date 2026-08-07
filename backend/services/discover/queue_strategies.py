"""Reusable queue-building strategy functions.

Extracted from DiscoverQueueService for reuse by Popular Radio,
Playlist-Seeded Discovery, and Daily Mix builders.
"""

import asyncio
import logging
import random

from api.v1.schemas.discover import DiscoverQueueItemLight
from api.v1.schemas.home import HomeAlbum
from repositories.listenbrainz_models import ListenBrainzArtist
from repositories.protocols import (
    LastFmRepositoryProtocol,
    ListenBrainzRepositoryProtocol,
    MusicBrainzRepositoryProtocol,
)
from services.discover.mbid_resolution_service import MbidResolutionService

logger = logging.getLogger(__name__)

VARIOUS_ARTISTS_MBID = "89ad4ac3-39f7-470e-963a-56509c546377"


def queue_item_to_home_album(item: DiscoverQueueItemLight) -> HomeAlbum:
    """Convert a DiscoverQueueItemLight to a HomeAlbum."""
    return HomeAlbum(
        name=item.album_name,
        mbid=item.release_group_mbid,
        artist_name=item.artist_name,
        artist_mbid=item.artist_mbid,
        image_url=f"/api/v1/covers/release-group/{item.release_group_mbid}?size=500",
    )


async def build_similar_artist_pools(
    seeds: list[ListenBrainzArtist],
    excluded_mbids: set[str],
    similar_limit: int,
    albums_per: int,
    *,
    lb_repo: ListenBrainzRepositoryProtocol,
    mbid_svc: MbidResolutionService,
) -> list[list[DiscoverQueueItemLight]]:
    """Build one pool of candidate queue items per seed artist via LB similar-artist lookups."""
    pools: list[list[DiscoverQueueItemLight]] = [[] for _ in range(len(seeds))]

    async def _process_seed(i: int, seed: ListenBrainzArtist) -> None:
        seed_mbid = seed.artist_mbids[0] if seed.artist_mbids else None
        if not seed_mbid:
            return

        pool_seen: set[str] = set()
        try:
            similar = await asyncio.wait_for(
                lb_repo.get_similar_artists(
                    seed_mbid,
                    max_similar=similar_limit,
                ),
                timeout=30,
            )
            for sim_artist in similar:
                sim_mbid = mbid_svc.normalize_mbid(sim_artist.artist_mbid)
                if not sim_mbid or sim_mbid == VARIOUS_ARTISTS_MBID:
                    continue

                try:
                    release_groups = await asyncio.wait_for(
                        lb_repo.get_artist_top_release_groups(
                            sim_mbid,
                            count=albums_per,
                        ),
                        timeout=30,
                    )
                except asyncio.TimeoutError:
                    logger.warning("Timeout getting releases for similar artist %s", sim_mbid[:8])
                    continue
                except Exception as e:  # noqa: BLE001
                    logger.debug(f"Failed to get releases for similar artist: {e}")
                    continue

                for rg in release_groups:
                    rg_mbid = mbid_svc.normalize_mbid(rg.release_group_mbid)
                    if not rg_mbid:
                        continue
                    if rg_mbid in excluded_mbids or rg_mbid in pool_seen:
                        continue
                    pools[i].append(
                        mbid_svc.make_queue_item(
                            release_group_mbid=rg_mbid,
                            album_name=rg.release_group_name,
                            artist_name=rg.artist_name,
                            artist_mbid=sim_mbid,
                            reason=f"Similar to {seed.artist_name}",
                        )
                    )
                    pool_seen.add(rg_mbid)
        except asyncio.TimeoutError:
            logger.warning("Timeout getting similar artists for seed %s", seed_mbid[:8])
        except Exception as e:  # noqa: BLE001
            logger.debug(f"Failed to get similar artists for seed {seed_mbid[:8]}: {e}")

    await asyncio.gather(*[_process_seed(i, seed) for i, seed in enumerate(seeds)])
    return pools


async def build_similar_artist_pools_lastfm(
    seed_mbids: list[str],
    excluded_mbids: set[str],
    similar_limit: int,
    albums_per: int,
    *,
    lfm_repo: LastFmRepositoryProtocol,
    mbid_svc: MbidResolutionService,
) -> list[list[DiscoverQueueItemLight]]:
    """Build candidate pools per seed artist via Last.fm similar-artist lookups."""
    pools: list[list[DiscoverQueueItemLight]] = [[] for _ in range(len(seed_mbids))]

    async def _process_seed(i: int, seed_mbid: str) -> None:
        pool_seen: set[str] = set()
        try:
            similar = await asyncio.wait_for(
                lfm_repo.get_similar_artists(
                    "", mbid=seed_mbid, limit=similar_limit,
                ),
                timeout=30,
            )
            for sim_artist in similar:
                sim_mbid = mbid_svc.normalize_mbid(sim_artist.mbid)
                if not sim_mbid or sim_mbid == VARIOUS_ARTISTS_MBID:
                    continue

                try:
                    top_albums = await asyncio.wait_for(
                        lfm_repo.get_artist_top_albums(
                            sim_artist.name, mbid=sim_artist.mbid, limit=albums_per,
                        ),
                        timeout=30,
                    )
                except asyncio.TimeoutError:
                    logger.warning("Timeout getting Last.fm top albums for %s", sim_artist.name)
                    continue
                except Exception as e:  # noqa: BLE001
                    logger.debug(f"Failed to get top albums for Last.fm similar artist: {e}")
                    continue

                artist_albums_pair = [(sim_artist, top_albums)]
                items = await mbid_svc.lastfm_albums_to_queue_items(
                    artist_albums_pair,
                    exclude=excluded_mbids | pool_seen,
                    target=albums_per,
                    reason="Similar to seed (via Last.fm)",
                )
                for item in items:
                    pools[i].append(item)
                    pool_seen.add(item.release_group_mbid.lower())
        except asyncio.TimeoutError:
            logger.warning("Timeout getting Last.fm similar artists for seed %s", seed_mbid[:8])
        except Exception as e:  # noqa: BLE001
            logger.debug(f"Failed to get Last.fm similar artists for seed {seed_mbid[:8]}: {e}")

    await asyncio.gather(*[_process_seed(i, mbid) for i, mbid in enumerate(seed_mbids)])
    return pools


async def discover_by_genres(
    genres: list[str],
    excluded_mbids: set[str],
    *,
    mb_repo: MusicBrainzRepositoryProtocol,
    mbid_svc: MbidResolutionService,
    per_genre_limit: int = 4,
) -> list[DiscoverQueueItemLight]:
    """Discover albums by genre tags via MusicBrainz tag search.

    The caller is responsible for resolving genre names (e.g. from a
    ListenBrainz user profile) and passing them in directly.  This keeps
    the function reusable for Playlist-Seeded Discovery, Popular Radio,
    and any other caller that already has a genre list.
    """
    if not genres:
        return []

    top_genres = genres[:per_genre_limit]

    search_results = await asyncio.gather(
        *[
            mb_repo.search_release_groups_by_tag(tag=genre, limit=8)
            for genre in top_genres
        ],
        return_exceptions=True,
    )

    items: list[DiscoverQueueItemLight] = []
    seen: set[str] = set()
    for genre, result in zip(top_genres, search_results):
        if isinstance(result, Exception):
            continue
        for release in result:
            rg_mbid = mbid_svc.normalize_mbid(getattr(release, "musicbrainz_id", None))
            if not rg_mbid:
                continue
            if rg_mbid in excluded_mbids or rg_mbid in seen:
                continue
            items.append(
                mbid_svc.make_queue_item(
                    release_group_mbid=rg_mbid,
                    album_name=getattr(release, "title", "Unknown"),
                    artist_name=getattr(release, "artist", "Unknown") or "Unknown",
                    artist_mbid=getattr(release, "artist_id", "") or rg_mbid,
                    reason=f"Because you listen to {genre}",
                )
            )
            seen.add(rg_mbid)
    return items


async def get_artist_deep_cuts(
    username: str,
    excluded_mbids: set[str],
    listened_mbids: set[str],
    albums_per_artist: int,
    *,
    lb_repo: ListenBrainzRepositoryProtocol,
    mbid_svc: MbidResolutionService,
) -> list[DiscoverQueueItemLight]:
    """Find lesser-known releases from the user's top-played artists."""
    try:
        top_release_groups = await lb_repo.get_user_top_release_groups(
            username=username,
            range_="this_month",
            count=25,
        )
    except Exception:  # noqa: BLE001
        logger.warning("Failed to fetch top release groups from ListenBrainz for deep cuts")
        return []

    if not top_release_groups:
        return []

    current_top_mbids = {
        rg.release_group_mbid.lower()
        for rg in top_release_groups
        if getattr(rg, "release_group_mbid", None)
    }

    artist_seed_names: dict[str, str] = {}
    for rg in top_release_groups:
        rg_artist_mbids = getattr(rg, "artist_mbids", None) or []
        if not rg_artist_mbids:
            continue
        artist_mbid = mbid_svc.normalize_mbid(rg_artist_mbids[0])
        if not artist_mbid or artist_mbid in artist_seed_names:
            continue
        artist_seed_names[artist_mbid] = getattr(rg, "artist_name", "")
        if len(artist_seed_names) >= 6:
            break

    if not artist_seed_names:
        return []

    artist_mbid_list = list(artist_seed_names.keys())
    results = await asyncio.gather(
        *[
            lb_repo.get_artist_top_release_groups(
                a_mbid,
                count=max(albums_per_artist + 2, 4),
            )
            for a_mbid in artist_mbid_list
        ],
        return_exceptions=True,
    )

    items: list[DiscoverQueueItemLight] = []
    seen_rg_mbids: set[str] = set()
    for a_mbid, result in zip(artist_mbid_list, results):
        if isinstance(result, Exception):
            continue
        for rg in result:
            rg_mbid = mbid_svc.normalize_mbid(rg.release_group_mbid)
            if not rg_mbid:
                continue
            if rg_mbid in current_top_mbids or rg_mbid in listened_mbids:
                continue
            if rg_mbid in excluded_mbids or rg_mbid in seen_rg_mbids:
                continue

            source_artist_name = artist_seed_names.get(a_mbid) or rg.artist_name
            items.append(
                mbid_svc.make_queue_item(
                    release_group_mbid=rg_mbid,
                    album_name=rg.release_group_name,
                    artist_name=rg.artist_name,
                    artist_mbid=a_mbid,
                    reason=f"More from {source_artist_name}",
                )
            )
            seen_rg_mbids.add(rg_mbid)
    return items


def round_robin_dedup_select(
    pools: list[list[DiscoverQueueItemLight]],
    count: int,
    max_per_artist: int = 2,
) -> list[DiscoverQueueItemLight]:
    """Select items round-robin across pools, deduplicating by MBID and capping per artist."""
    selected: list[DiscoverQueueItemLight] = []
    seen_mbids: set[str] = set()
    artist_counts: dict[str, int] = {}

    shuffled = [list(pool) for pool in pools]
    for pool in shuffled:
        random.shuffle(pool)

    pool_indices = [0] * len(shuffled)

    for _ in range(count * 3):
        if len(selected) >= count:
            break
        for pool_idx in range(len(shuffled)):
            if len(selected) >= count:
                break
            pool = shuffled[pool_idx]
            idx = pool_indices[pool_idx]
            while idx < len(pool):
                item = pool[idx]
                idx += 1
                pool_indices[pool_idx] = idx
                mbid_lower = item.release_group_mbid.lower()
                artist_key = item.artist_mbid.lower() if item.artist_mbid else ""
                if mbid_lower in seen_mbids:
                    continue
                if artist_key and artist_counts.get(artist_key, 0) >= max_per_artist:
                    continue
                selected.append(item)
                seen_mbids.add(mbid_lower)
                if artist_key:
                    artist_counts[artist_key] = artist_counts.get(artist_key, 0) + 1
                break

    return selected


async def get_trending_filler(
    count: int,
    ignored_mbids: set[str],
    library_mbids: set[str],
    seen_mbids: set[str] | None,
    source: str,
    *,
    lb_repo: ListenBrainzRepositoryProtocol,
    mb_repo: MusicBrainzRepositoryProtocol,
    mbid_svc: MbidResolutionService,
    lfm_repo: LastFmRepositoryProtocol | None = None,
    is_lastfm_enabled: bool = False,
) -> list[DiscoverQueueItemLight]:
    """Fetch trending/wildcard albums from LB or Last.fm as queue filler."""
    if count <= 0:
        return []
    exclude = ignored_mbids | library_mbids | (seen_mbids or set())
    from repositories.listenbrainz_repository import lb_popularity_degraded

    use_lastfm = (
        (source == "lastfm" or lb_popularity_degraded())
        and is_lastfm_enabled
        and lfm_repo is not None
    )
    target = max(count * 2, 6)

    try:
        wildcards: list[DiscoverQueueItemLight] = []
        if use_lastfm:
            top_artists = await lfm_repo.get_global_top_artists(limit=15)  # type: ignore[union-attr]
            random.shuffle(top_artists)
            valid_artists = [
                a
                for a in top_artists[:10]
                if mbid_svc.normalize_mbid(a.mbid) != VARIOUS_ARTISTS_MBID
            ]
            album_fetch_results = await asyncio.gather(
                *[
                    lfm_repo.get_artist_top_albums(  # type: ignore[union-attr]
                        a.name, mbid=a.mbid, limit=3
                    )
                    for a in valid_artists
                ],
                return_exceptions=True,
            )
            artist_albums_pairs: list[tuple[object, list[object]]] = []
            for artist, result in zip(valid_artists, album_fetch_results):
                if isinstance(result, Exception):
                    continue
                artist_albums_pairs.append((artist, result))
            wildcards = await mbid_svc.lastfm_albums_to_queue_items(
                artist_albums_pairs,
                exclude=exclude,
                target=target,
                reason="Trending on Last.fm",
                is_wildcard=True,
            )
        else:
            rgs = await lb_repo.get_sitewide_top_release_groups(count=25)
            random.shuffle(rgs)
            for rg in rgs:
                if len(wildcards) >= target:
                    break
                rg_mbid = rg.release_group_mbid
                if not rg_mbid or rg_mbid.lower() in exclude:
                    continue
                artist_mbid = rg.artist_mbids[0] if rg.artist_mbids else ""
                if artist_mbid.lower() == VARIOUS_ARTISTS_MBID:
                    continue
                wildcards.append(DiscoverQueueItemLight(
                    release_group_mbid=rg_mbid,
                    album_name=rg.release_group_name,
                    artist_name=rg.artist_name,
                    artist_mbid=artist_mbid,
                    cover_url=f"/api/v1/covers/release-group/{rg_mbid}?size=500",
                    recommendation_reason="Trending This Week",
                    is_wildcard=True,
                    in_library=False,
                ))
                exclude.add(rg_mbid.lower())
    except Exception as e:  # noqa: BLE001
        logger.debug(f"Failed to get wildcard albums: {e}")
        wildcards = []

    if not wildcards:
        # MusicBrainz decade tag-search fallback. Ungated (previously only ran on the
        # use_lastfm path): during a ListenBrainz-popularity outage a user with Last.fm
        # NOT linked gets an empty sitewide-top primary tier AND no fallback, so the queue
        # renders only its personalised slots (~5). This tier needs no LB popularity
        # endpoint and no per-album release->RG resolution - each tag is one direct RG
        # query - so it fills wildcard/top-up slots whenever MusicBrainz is reachable.
        decade_tags = ["2020s", "2010s", "2000s", "1990s", "1980s", "1970s"]
        for decade in decade_tags:
            if len(wildcards) >= target:
                break
            try:
                decade_releases = await mb_repo.search_release_groups_by_tag(
                    tag=decade,
                    limit=25,
                    offset=0,
                )
            except Exception:  # noqa: BLE001
                logger.warning("Failed to search release groups for decade tag %s", decade)
                continue
            for release in decade_releases:
                if len(wildcards) >= target:
                    break
                rg_mbid = mbid_svc.normalize_mbid(getattr(release, "musicbrainz_id", None))
                if not rg_mbid or rg_mbid.lower() in exclude:
                    continue
                wildcards.append(DiscoverQueueItemLight(
                    release_group_mbid=rg_mbid,
                    album_name=getattr(release, "title", "Unknown"),
                    artist_name=getattr(release, "artist", "Unknown") or "Unknown",
                    artist_mbid="",
                    cover_url=f"/api/v1/covers/release-group/{rg_mbid}?size=500",
                    recommendation_reason="Trending",
                    is_wildcard=True,
                    in_library=False,
                ))
                exclude.add(rg_mbid.lower())

    if not wildcards:
        logger.warning("Failed to populate any wildcard albums for discover queue")

    return wildcards[:count]


def interleave_at_positions(
    base: list[DiscoverQueueItemLight],
    insertions: list[DiscoverQueueItemLight],
    positions: list[int] | None = None,
) -> list[DiscoverQueueItemLight]:
    """Insert items from *insertions* into *base* at the given positions, appending any remainder.

    Because ``list.insert`` shifts subsequent indices, each position is
    relative to the *growing* list (after prior insertions), not the
    original ``base`` list.
    """
    if positions is None:
        positions = [2, 7]
    result = list(base)
    for i, wc in enumerate(insertions):
        pos = positions[i] if i < len(positions) else len(result)
        pos = min(pos, len(result))
        result.insert(pos, wc)
    return result
