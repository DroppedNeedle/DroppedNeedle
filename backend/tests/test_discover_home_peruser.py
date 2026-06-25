"""Phase 5 (AuthMultiUser) per-user discovery/home isolation tests (AMU-8).

Service-level 3-user style coverage: two linked users never share cache entries or
data, an unlinked user gets sitewide-only home + a connect prompt with recently-played
still sourced from local play_history (D6), the shared LB/Last.fm singleton is never
credential-mutated, and DiscoverQueueManager state is keyed per (user_id, source).
"""

import time
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from api.v1.schemas.settings import HomeSettings
from services.home_service import HomeService
from services.discover_queue_manager import DiscoverQueueManager, QueueBuildStatus


class _FakeCache:
    """Minimal in-memory cache to assert per-user key isolation."""

    def __init__(self) -> None:
        self.store: dict[str, object] = {}

    async def get(self, key: str):
        return self.store.get(key)

    async def set(self, key: str, value, ttl=None) -> None:
        self.store[key] = value


def _conn(**attrs) -> MagicMock:
    m = MagicMock()
    m.enabled = False
    for key, value in attrs.items():
        setattr(m, key, value)
    return m


def _global_prefs() -> MagicMock:
    prefs = MagicMock()
    prefs.get_listenbrainz_connection.return_value = _conn(username="", user_token="")
    prefs.get_jellyfin_connection.return_value = _conn(jellyfin_url="", api_key="")
    prefs.get_download_client_settings.return_value = _conn(url="")
    yt = _conn(api_enabled=False)
    yt.has_valid_api_key = MagicMock(return_value=False)
    prefs.get_youtube_connection.return_value = yt
    prefs.get_navidrome_connection.return_value = _conn(navidrome_url="", username="", password="")
    prefs.get_plex_connection.return_value = _conn(plex_url="", plex_token="", music_library_ids=[])
    prefs.is_lastfm_enabled.return_value = False
    prefs.get_lastfm_connection.return_value = _conn(api_key="", shared_secret="", session_key="", username="")
    prefs.get_home_settings.return_value = HomeSettings(show_whats_hot=True)
    prefs.get_primary_music_source.return_value = SimpleNamespace(source="listenbrainz")
    return prefs


def _factory(*, lb_linked: bool, lfm_linked: bool, lb_username="lbuser", lfm_username="lfmuser") -> MagicMock:
    f = MagicMock()
    lb_repo = None
    if lb_linked:
        lb_repo = AsyncMock()
        lb_repo.get_user_loved_recordings = AsyncMock(return_value=[])
        lb_repo.get_user_genre_activity = AsyncMock(return_value=None)
        lb_repo.get_user_top_release_groups = AsyncMock(return_value=[])
        lb_repo.get_recommendation_playlists = AsyncMock(return_value=[])
    lfm_repo = None
    if lfm_linked:
        lfm_repo = AsyncMock()
        lfm_repo.get_user_top_albums = AsyncMock(return_value=[])
        lfm_repo.get_user_loved_tracks = AsyncMock(return_value=[])
    f.resolve_listenbrainz = AsyncMock(return_value=lb_repo)
    f.resolve_lastfm = AsyncMock(return_value=lfm_repo)
    f.resolve_listenbrainz_username = AsyncMock(return_value=lb_username if lb_linked else None)
    f.resolve_lastfm_username = AsyncMock(return_value=lfm_username if lfm_linked else None)
    return f


def _play_record(track: str, artist: str = "Artist") -> SimpleNamespace:
    return SimpleNamespace(
        track_name=track,
        artist_name=artist,
        album_name="Album",
        recording_mbid="rec-1",
        release_group_mbid="rg-1",
        duration_ms=180000,
        source="local",
        played_at="2026-06-20T12:00:00+00:00",
    )


def _home_service(factory: MagicMock, *, cache: _FakeCache | None = None, play_records=None):
    lb_repo = AsyncMock()
    lb_repo.get_sitewide_top_artists = AsyncMock(return_value=[])
    lb_repo.get_sitewide_top_release_groups = AsyncMock(return_value=[])
    lb_repo.configure = MagicMock()  # must never be called: shared singleton

    lfm_repo = AsyncMock()
    lfm_repo.get_global_top_artists = AsyncMock(return_value=[])

    library_repo = AsyncMock()
    library_repo.get_library = AsyncMock(return_value=[])
    library_repo.get_artists_from_library = AsyncMock(return_value=[])
    library_repo.get_recently_imported = AsyncMock(return_value=[])

    prefs_store = MagicMock()
    prefs_store.get = AsyncMock(return_value=SimpleNamespace(primary_music_source="listenbrainz"))

    play_store = MagicMock()
    play_store.recent = AsyncMock(return_value=play_records or [])

    service = HomeService(
        listenbrainz_repo=lb_repo,
        jellyfin_repo=AsyncMock(),
        library_repo=library_repo,
        musicbrainz_repo=AsyncMock(),
        preferences_service=_global_prefs(),
        memory_cache=cache,
        lastfm_repo=lfm_repo,
        client_factory=factory,
        listening_prefs_store=prefs_store,
        play_history_store=play_store,
    )
    return service, lb_repo


@pytest.mark.asyncio
async def test_recently_played_is_per_user_from_play_history():
    svc_a, _ = _home_service(_factory(lb_linked=True, lfm_linked=False), play_records=[_play_record("Song A")])
    svc_b, _ = _home_service(_factory(lb_linked=False, lfm_linked=False), play_records=[_play_record("Song B")])

    resp_a = await svc_a.get_home_data("user-a", "listenbrainz")
    resp_b = await svc_b.get_home_data("user-b", "listenbrainz")

    assert resp_a.recently_played is not None
    assert resp_a.recently_played.items[0].name == "Song A"
    assert resp_b.recently_played is not None
    assert resp_b.recently_played.items[0].name == "Song B"


@pytest.mark.asyncio
async def test_unlinked_user_omits_personalized_and_shows_connect_prompt():
    svc, _ = _home_service(_factory(lb_linked=False, lfm_linked=False), play_records=[_play_record("Local Song")])

    resp = await svc.get_home_data("unlinked-user", "listenbrainz")

    # D1: identity-gated sections are omitted for an unlinked user
    assert resp.your_top_albums is None
    assert resp.favorite_artists is None
    assert resp.weekly_exploration is None
    # D6: recently-played still comes from local play_history
    assert resp.recently_played is not None
    assert resp.recently_played.items[0].name == "Local Song"
    prompt_services = {p.service for p in resp.service_prompts}
    assert "listenbrainz" in prompt_services
    assert "lastfm" in prompt_services


@pytest.mark.asyncio
async def test_two_linked_users_write_distinct_cache_entries():
    cache = _FakeCache()
    svc_a, _ = _home_service(_factory(lb_linked=True, lfm_linked=False), cache=cache, play_records=[_play_record("A")])
    svc_b, _ = _home_service(_factory(lb_linked=True, lfm_linked=False), cache=cache, play_records=[_play_record("B")])

    await svc_a.get_home_data("user-a", "listenbrainz")
    await svc_b.get_home_data("user-b", "listenbrainz")

    keys = list(cache.store.keys())
    assert any("user-a" in k for k in keys)
    assert any("user-b" in k for k in keys)
    assert not any("user-a" in k and "user-b" in k for k in keys)


@pytest.mark.asyncio
async def test_personalized_path_never_mutates_singleton_credentials():
    # interleaving two users must never call the shared LB singleton's configure()
    svc_a, lb_singleton_a = _home_service(_factory(lb_linked=True, lfm_linked=False))
    svc_b, lb_singleton_b = _home_service(_factory(lb_linked=True, lfm_linked=False))

    await svc_a.get_home_data("user-a", "listenbrainz")
    await svc_b.get_home_data("user-b", "listenbrainz")
    await svc_a.get_home_data("user-a", "listenbrainz")

    lb_singleton_a.configure.assert_not_called()
    lb_singleton_b.configure.assert_not_called()


def _queue_manager() -> DiscoverQueueManager:
    prefs = MagicMock()
    prefs.get_advanced_settings.return_value = SimpleNamespace(discover_queue_ttl=3600)
    return DiscoverQueueManager(discover_service=MagicMock(), preferences_service=prefs)


@pytest.mark.asyncio
async def test_queue_state_is_isolated_per_user():
    qm = _queue_manager()
    state_a = qm._get_state("user-a", "listenbrainz")
    state_a.status = QueueBuildStatus.READY
    state_a.queue = SimpleNamespace(queue_id="queue-a", items=[1, 2, 3])
    state_a.built_at = time.time()

    assert qm.get_status("user-b", "listenbrainz").status == "idle"
    assert qm.get_queue("user-b", "listenbrainz") is None

    # consuming A's queue must not drain B's
    consumed = await qm.consume_queue("user-a", "listenbrainz")
    assert consumed is not None
    assert consumed.queue_id == "queue-a"
    assert await qm.consume_queue("user-b", "listenbrainz") is None


def test_queue_invalidate_is_scoped_to_one_user():
    qm = _queue_manager()
    qm._get_state("user-a", "listenbrainz").status = QueueBuildStatus.READY
    qm._get_state("user-b", "listenbrainz").status = QueueBuildStatus.READY

    qm.invalidate("user-a")

    assert qm._get_state("user-a", "listenbrainz").status == QueueBuildStatus.IDLE
    assert qm._get_state("user-b", "listenbrainz").status == QueueBuildStatus.READY


def test_discover_cache_key_includes_enable_flags():
    # regression: connect/disconnect must bust the discover cache, so the key tracks
    # lb/lfm enable flags like Home does
    from services.discover.integration_helpers import IntegrationHelpers

    helpers = IntegrationHelpers(MagicMock())
    linked = helpers.get_discover_cache_key("u1", "listenbrainz", lb_enabled=True, lfm_enabled=False)
    unlinked = helpers.get_discover_cache_key("u1", "listenbrainz", lb_enabled=False, lfm_enabled=False)
    assert linked != unlinked


def test_discover_service_prompts_are_per_user():
    # regression: a linked user must not see the Discover connect prompt; it used to
    # read global config and show for everyone
    from services.discover.homepage_service import DiscoverHomepageService

    svc = DiscoverHomepageService.__new__(DiscoverHomepageService)
    svc._integration = MagicMock()
    svc._integration.is_jellyfin_enabled.return_value = True
    svc._integration.is_download_client_configured.return_value = True

    linked = svc._build_service_prompts(lb_enabled=True, lfm_enabled=True)
    assert [p.service for p in linked] == []

    unlinked = svc._build_service_prompts(lb_enabled=False, lfm_enabled=False)
    assert {p.service for p in unlinked} == {"listenbrainz", "lastfm"}


@pytest.mark.asyncio
async def test_seed_artists_fall_back_to_broader_ranges():
    # a quiet week/month but real long-term history should still yield seeds, so the
    # personalized sections populate
    from services.discover.homepage_service import DiscoverHomepageService
    from repositories.listenbrainz_models import ListenBrainzArtist

    svc = DiscoverHomepageService.__new__(DiscoverHomepageService)
    svc._lfm_repo = None

    seed = ListenBrainzArtist(artist_name="A", listen_count=5, artist_mbids=["mbid-1"])

    async def top_artists(count=10, range_="this_month"):
        return [seed] if range_ == "all_time" else []

    client = MagicMock()
    client.get_user_top_artists = AsyncMock(side_effect=top_artists)

    seeds = await svc._get_seed_artists(
        lb_enabled=True, username="u", jf_enabled=False,
        resolved_source="listenbrainz", lb_client=client,
    )
    assert [s.artist_mbids[0] for s in seeds] == ["mbid-1"]


def _swr_service(cache_key: str, cached_response):
    """A DiscoverHomepageService primed with a cached response, collaborators mocked,
    for exercising the stale-while-revalidate path in get_discover_data."""
    from services.discover.homepage_service import DiscoverHomepageService

    svc = DiscoverHomepageService.__new__(DiscoverHomepageService)
    svc._resolve_user_music = AsyncMock(
        return_value=(None, None, None, None, True, False, "listenbrainz")
    )
    svc._building_keys = set()
    cache = _FakeCache()
    cache.store[cache_key] = cached_response
    svc._memory_cache = cache
    svc._integration = MagicMock()
    svc._integration.get_discover_cache_key.return_value = cache_key
    svc._integration.get_home_settings.return_value = SimpleNamespace(show_globally_trending=True)
    svc._trigger_warm = MagicMock()
    return svc


@pytest.mark.asyncio
async def test_stale_cache_serves_immediately_and_revalidates_in_background():
    import time as _time

    from api.v1.schemas.discover import DiscoverResponse
    from services.discover.homepage_service import STALE_REVALIDATE_SECONDS

    key = "discover_response:u1:listenbrainz:True:False"
    svc = _swr_service(key, DiscoverResponse(refreshing=False))
    # built long ago -> serve the cached copy but kick off a background rebuild
    svc._built_at = {key: _time.time() - STALE_REVALIDATE_SECONDS - 10}

    resp = await svc.get_discover_data("u1", "listenbrainz")

    assert resp.refreshing is True  # frontend shows the "updating" indicator + polls
    svc._trigger_warm.assert_called_once_with("u1", "listenbrainz")


@pytest.mark.asyncio
async def test_fresh_cache_served_without_rebuild():
    import time as _time

    from api.v1.schemas.discover import DiscoverResponse

    key = "discover_response:u1:listenbrainz:True:False"
    svc = _swr_service(key, DiscoverResponse(refreshing=False))
    # just built -> serve as-is, no rebuild, not refreshing
    svc._built_at = {key: _time.time()}

    resp = await svc.get_discover_data("u1", "listenbrainz")

    assert resp.refreshing is False
    svc._trigger_warm.assert_not_called()


@pytest.mark.asyncio
async def test_manual_refresh_targets_viewed_source_and_marks_it_stale():
    from services.discover.homepage_service import DiscoverHomepageService

    svc = DiscoverHomepageService.__new__(DiscoverHomepageService)
    # primary source is listenbrainz, but the user is viewing (and refreshing) lastfm
    svc._resolve_user_music = AsyncMock(
        return_value=(None, None, None, None, False, True, "lastfm")
    )
    svc._building_keys = set()
    key = "discover_response:u1:lastfm:False:True"
    svc._integration = MagicMock()
    svc._integration.get_discover_cache_key.return_value = key
    svc._built_at = {key: 999999999.0}  # currently "fresh"
    svc._trigger_warm = MagicMock()

    await svc.refresh_discover_data("u1", "lastfm")

    # resolves the VIEWED source, not the primary
    svc._resolve_user_music.assert_awaited_once_with("u1", "lastfm")
    # marks that source stale so the next GET reliably revalidates (shows the spinner)
    assert key not in svc._built_at
    svc._trigger_warm.assert_called_once_with("u1", "lastfm")


def _miss_service(built_at: dict):
    from services.discover.homepage_service import DiscoverHomepageService

    svc = DiscoverHomepageService.__new__(DiscoverHomepageService)
    svc._resolve_user_music = AsyncMock(
        return_value=(None, None, None, None, True, False, "listenbrainz")
    )
    svc._building_keys = set()
    svc._memory_cache = _FakeCache()  # no cached response -> cache miss
    svc._integration = MagicMock()
    svc._integration.get_discover_cache_key.return_value = "discover_response:u1:listenbrainz:True:False"
    svc._integration.get_integration_status.return_value = MagicMock()
    svc._build_service_prompts = MagicMock(return_value=[])
    svc._trigger_warm = MagicMock()
    svc._built_at = built_at
    return svc


@pytest.mark.asyncio
async def test_first_visit_cache_miss_triggers_build():
    svc = _miss_service(built_at={})  # never built
    resp = await svc.get_discover_data("u1", "listenbrainz")
    assert resp.refreshing is True
    svc._trigger_warm.assert_called_once_with("u1", "listenbrainz")


@pytest.mark.asyncio
async def test_empty_build_backs_off_instead_of_rebuilding_every_poll():
    import time as _time

    # a build was just attempted but produced nothing (empty user): the next poll must
    # NOT rebuild, and must settle (refreshing=false) instead of polling forever
    svc = _miss_service(built_at={"discover_response:u1:listenbrainz:True:False": _time.time()})
    resp = await svc.get_discover_data("u1", "listenbrainz")
    assert resp.refreshing is False
    svc._trigger_warm.assert_not_called()
