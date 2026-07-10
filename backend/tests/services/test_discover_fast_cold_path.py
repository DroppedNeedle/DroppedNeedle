"""Fast-first-paint, zone isolation and restart persistence for the Discover build.

Covers the slow-Discover fixes:
- the cold path never blocks on the full build: it returns the cheap library-derived
  zones within the quick budget, flags ``refreshing`` and completes the rest in a
  background warm that fills the cache;
- one hanging upstream zone can't delay unrelated zones (per-zone timeouts);
- a built page persists to the disk metadata cache and reloads instantly after a
  simulated restart (new service instance, empty memory cache).
"""

import asyncio
import time
from datetime import datetime, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from api.v1.schemas.discover import DiscoverResponse
from api.v1.schemas.home import HomeAlbum, HomeArtist, HomeGenre, HomeSection
from infrastructure.cache.memory_cache import InMemoryCache
from repositories.listenbrainz_models import ListenBrainzArtist
from services.discover.homepage_service import DiscoverHomepageService
from services.discover.response_codec import (
    decode_discover_response,
    encode_discover_response,
)


def _cache_key(user_id: str) -> str:
    return f"discover_response:{user_id}:True:False"


def _anniversary_albums() -> list[dict]:
    year = datetime.now(timezone.utc).year - 20
    return [
        {"mbid": f"a{i}", "title": f"Old Gold {i}", "artist_name": "A", "artist_mbid": None, "year": year}
        for i in range(3)
    ]


def _make_service(
    memory_cache=None,
    disk_cache=None,
    lb_repo=None,
    library_db=None,
) -> DiscoverHomepageService:
    lb = lb_repo or AsyncMock()
    integration = MagicMock()
    integration.get_discover_cache_key.side_effect = (
        lambda uid, lb_enabled=False, lfm_enabled=False: _cache_key(uid)
    )
    integration.get_integration_status.return_value = None
    integration.is_jellyfin_enabled.return_value = False
    integration.is_library_configured.return_value = True
    integration.get_discover_picks_settings.return_value = (True, 5)

    mbid = MagicMock()
    mbid.get_library_artist_mbids = AsyncMock(return_value=set())
    mbid.get_library_album_mbids = AsyncMock(return_value=set())
    mbid.get_user_listened_release_group_mbids = AsyncMock(return_value=set())
    mbid.normalize_mbid.side_effect = lambda m: m.lower() if m else None

    library_repo = AsyncMock()
    library_repo.get_artists_from_library = AsyncMock(return_value=[])
    library_repo.get_library = AsyncMock(return_value=[])

    factory = MagicMock()
    factory.resolve_listenbrainz = AsyncMock(return_value=lb)
    factory.resolve_lastfm = AsyncMock(return_value=None)
    factory.resolve_listenbrainz_username = AsyncMock(return_value="lbuser")
    factory.resolve_lastfm_username = AsyncMock(return_value=None)

    prefs_store = MagicMock()
    prefs_store.get = AsyncMock(
        return_value=SimpleNamespace(primary_music_source="listenbrainz")
    )

    return DiscoverHomepageService(
        listenbrainz_repo=lb,
        jellyfin_repo=AsyncMock(),
        library_repo=library_repo,
        musicbrainz_repo=AsyncMock(),
        integration=integration,
        mbid_resolution=mbid,
        memory_cache=memory_cache,
        lastfm_repo=None,
        client_factory=factory,
        listening_prefs_store=prefs_store,
        library_db=library_db,
        disk_cache=disk_cache,
    )


def _full_response() -> DiscoverResponse:
    return DiscoverResponse(
        globally_trending=HomeSection(
            title="Globally Trending",
            type="artists",
            items=[HomeArtist(mbid="m1", name="Artist One", listen_count=5)],
            source="listenbrainz",
        ),
        daily_mixes=[
            HomeSection(
                title="Daily Mix 1 - Rock",
                type="albums",
                items=[HomeAlbum(name="Album", mbid="rg1", artist_name="A")],
                source="listenbrainz",
            )
        ],
        genre_list=HomeSection(
            title="Browse by Genre",
            type="genres",
            items=[HomeGenre(name="Rock", listen_count=10)],
            source="listenbrainz",
        ),
    )


class TestFastColdPath:
    @pytest.mark.asyncio
    async def test_cold_request_returns_fast_and_background_build_fills_cache(self):
        uid = "cold-user-1"
        cache = InMemoryCache()
        library_db = MagicMock()
        library_db.get_albums = AsyncMock(return_value=_anniversary_albums())
        svc = _make_service(memory_cache=cache, library_db=library_db)

        build_started = asyncio.Event()

        async def slow_build(user_id: str) -> DiscoverResponse:
            build_started.set()
            await asyncio.sleep(1.0)  # a slow external-dependent full build
            return _full_response()

        svc.build_discover_data = slow_build

        start = time.monotonic()
        resp = await svc.get_discover_data(uid)
        elapsed = time.monotonic() - start

        # never blocks on the full build; cheap local zones paint immediately
        assert elapsed < 2.0
        assert resp.refreshing is True
        assert resp.anniversaries is not None and resp.anniversaries.items
        assert resp.globally_trending is None  # not ready yet -> empty zone

        # the triggered background build completes and fills the cache
        await asyncio.wait_for(build_started.wait(), timeout=2)
        for _ in range(60):
            if await cache.get(_cache_key(uid)) is not None:
                break
            await asyncio.sleep(0.1)
        cached = await cache.get(_cache_key(uid))
        assert isinstance(cached, DiscoverResponse)

        followup = await svc.get_discover_data(uid)
        assert followup.refreshing is False  # settled
        assert followup.globally_trending is not None
        assert followup.globally_trending.items[0].name == "Artist One"

    @pytest.mark.asyncio
    async def test_empty_quick_zones_still_returns_fast_shell(self):
        uid = "cold-user-2"
        svc = _make_service(memory_cache=InMemoryCache())
        svc.build_discover_data = AsyncMock(return_value=DiscoverResponse())

        resp = await svc.get_discover_data(uid)

        assert resp.refreshing is True
        assert resp.anniversaries is None
        assert resp.because_you_listen_to == []


class TestZoneIsolation:
    @pytest.mark.asyncio
    async def test_one_hanging_zone_does_not_delay_or_kill_others(self, monkeypatch):
        import services.discover.homepage_service as hs

        monkeypatch.setattr(hs, "DISCOVER_TASK_TIMEOUT_SECONDS", 0.5)

        lb = AsyncMock()
        lb.get_user_top_artists = AsyncMock(return_value=[])
        lb.get_sitewide_top_artists = AsyncMock(
            return_value=[
                ListenBrainzArtist(
                    artist_name=f"Trend {i}", listen_count=10 - i, artist_mbids=[f"t{i}"]
                )
                for i in range(5)
            ]
        )

        async def hang():
            await asyncio.sleep(30)

        lb.get_user_fresh_releases = AsyncMock(side_effect=hang)
        lb.get_user_genre_activity = AsyncMock(return_value=[])
        lb.get_similar_users = AsyncMock(return_value=[])
        lb.get_recommendation_playlists = AsyncMock(return_value=[])
        lb.get_sitewide_top_release_groups = AsyncMock(return_value=[])

        svc = _make_service(memory_cache=InMemoryCache(), lb_repo=lb)

        start = time.monotonic()
        response = await svc.build_discover_data("iso-user")
        elapsed = time.monotonic() - start

        # the hanging zone was cut at its own budget without stalling the build
        assert elapsed < 5.0
        assert response.fresh_releases is None
        # unrelated zones still built
        assert response.globally_trending is not None
        assert len(response.globally_trending.items) == 5


class TestPersistentCache:
    @pytest.mark.asyncio
    async def test_reload_after_simulated_restart(self, tmp_path):
        from infrastructure.cache.disk_cache import DiskMetadataCache

        uid = "persist-user-1"
        disk = DiskMetadataCache(base_path=tmp_path)

        svc1 = _make_service(memory_cache=InMemoryCache(), disk_cache=disk)
        svc1.build_discover_data = AsyncMock(return_value=_full_response())
        await svc1.warm_cache(uid)

        persisted = await disk.get_discover(_cache_key(uid))
        assert isinstance(persisted, dict) and persisted.get("response")

        # "restart": a brand-new service instance with an empty memory cache
        svc2 = _make_service(memory_cache=InMemoryCache(), disk_cache=disk)
        resp = await svc2.get_discover_data(uid)

        # yesterday's discover is served instantly, fully typed, without a rebuild
        assert resp.refreshing is False  # fresh enough -> no background churn
        assert resp.globally_trending is not None
        assert isinstance(resp.globally_trending.items[0], HomeArtist)
        assert resp.daily_mixes and isinstance(resp.daily_mixes[0].items[0], HomeAlbum)
        assert resp.genre_list and isinstance(resp.genre_list.items[0], HomeGenre)

    @pytest.mark.asyncio
    async def test_stale_persisted_copy_triggers_background_refresh(self, tmp_path, monkeypatch):
        from infrastructure.cache.disk_cache import DiskMetadataCache

        uid = "persist-user-2"
        disk = DiskMetadataCache(base_path=tmp_path)

        svc1 = _make_service(memory_cache=InMemoryCache(), disk_cache=disk)
        svc1.build_discover_data = AsyncMock(return_value=_full_response())
        await svc1.warm_cache(uid)

        # age the persisted copy past the SWR freshness window (still inside its TTL)
        payload = await disk.get_discover(_cache_key(uid))
        payload["built_at"] = time.time() - 3600
        await disk.set_discover(_cache_key(uid), payload, ttl_seconds=3600)

        svc2 = _make_service(memory_cache=InMemoryCache(), disk_cache=disk)
        svc2._trigger_warm = MagicMock()
        resp = await svc2.get_discover_data(uid)

        # stale-but-usable: serve it AND refresh in background
        assert resp.globally_trending is not None
        assert resp.refreshing is True
        svc2._trigger_warm.assert_called_once_with(uid)


class TestResponseCodec:
    def test_round_trip_preserves_sections_and_item_types(self):
        from api.v1.schemas.discover import (
            BecauseYouListenTo,
            TopPickItem,
            TopPicksSection,
        )

        original = _full_response()
        original.because_you_listen_to = [
            BecauseYouListenTo(
                seed_artist="Seed",
                seed_artist_mbid="s1",
                listen_count=7,
                section=HomeSection(
                    title="Because You Listen To Seed",
                    type="artists",
                    items=[HomeArtist(mbid="sim1", name="Similar One")],
                    source="listenbrainz",
                ),
            )
        ]
        original.top_picks = TopPicksSection(
            items=[
                TopPickItem(
                    album=HomeAlbum(name="Pick", mbid="p1", artist_name="PA"),
                    match_pct=87,
                    reasons=["similar to Seed"],
                )
            ],
            source="listenbrainz",
        )
        original.genre_artists = {"Rock": "m1"}

        decoded = decode_discover_response(encode_discover_response(original))

        assert decoded is not None
        assert isinstance(decoded.globally_trending.items[0], HomeArtist)
        assert decoded.globally_trending.items[0].name == "Artist One"
        assert isinstance(decoded.daily_mixes[0].items[0], HomeAlbum)
        assert isinstance(decoded.genre_list.items[0], HomeGenre)
        because = decoded.because_you_listen_to[0]
        assert because.seed_artist == "Seed" and because.listen_count == 7
        assert isinstance(because.section.items[0], HomeArtist)
        assert decoded.top_picks.items[0].match_pct == 87
        assert decoded.top_picks.items[0].album.name == "Pick"
        assert decoded.genre_artists == {"Rock": "m1"}

    def test_corrupt_payload_decodes_to_none(self):
        assert decode_discover_response(None) is None
        assert decode_discover_response("junk") is None
        assert decode_discover_response({"because_you_listen_to": 42}) is None
