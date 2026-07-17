import pytest
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

_UID = "u1"

from api.v1.schemas.settings import (
    ListenBrainzConnectionSettings,
    LastFmConnectionSettings,
    PrimaryMusicSourceSettings,
)
from api.v1.schemas.search import SearchResult
from repositories.listenbrainz_models import (
    ListenBrainzArtist,
    ListenBrainzReleaseGroup,
    ListenBrainzGenreActivity,
    ListenBrainzFeedbackRecording,
)
from services.discover_service import DiscoverService


def _make_lb_settings(
    enabled: bool = True, username: str = "lbuser"
) -> ListenBrainzConnectionSettings:
    return ListenBrainzConnectionSettings(
        user_token="tok",
        username=username,
        enabled=enabled,
    )


def _make_lfm_settings(
    enabled: bool = True, username: str = "lfmuser"
) -> LastFmConnectionSettings:
    return LastFmConnectionSettings(
        api_key="key",
        shared_secret="secret",
        session_key="sk",
        username=username,
        enabled=enabled,
    )


def _make_prefs(
    lb_enabled: bool = True,
    lfm_enabled: bool = True,
    primary_source: str = "listenbrainz",
) -> MagicMock:
    prefs = MagicMock()
    prefs.get_listenbrainz_connection.return_value = _make_lb_settings(
        enabled=lb_enabled
    )
    prefs.get_lastfm_connection.return_value = _make_lfm_settings(enabled=lfm_enabled)
    prefs.is_lastfm_enabled.return_value = lfm_enabled
    prefs.get_primary_music_source.return_value = PrimaryMusicSourceSettings(
        source=primary_source
    )

    jf_settings = MagicMock()
    jf_settings.enabled = False
    jf_settings.jellyfin_url = ""
    jf_settings.api_key = ""
    prefs.get_jellyfin_connection.return_value = jf_settings

    download_client = MagicMock()
    download_client.enabled = False
    download_client.url = ""
    prefs.get_download_client_settings.return_value = download_client
    # is_download_client_configured() now delegates to is_download_source_ready() (slskd OR
    # Usenet); mirror the disabled download client so the onboarding prompt shows.
    prefs.is_download_source_ready.return_value = False

    yt = MagicMock()
    yt.enabled = False
    yt.api_key = ""
    prefs.get_youtube_connection.return_value = yt

    lf = MagicMock()
    lf.enabled = False
    lf.music_path = ""
    prefs.get_local_files_connection.return_value = lf

    adv = MagicMock()
    adv.discover_queue_size = 10
    adv.discover_queue_ttl = 3600
    adv.discover_queue_seed_artists = 3
    adv.discover_queue_wildcard_slots = 2
    adv.discover_queue_similar_artists_limit = 15
    adv.discover_queue_albums_per_similar = 3
    adv.discover_queue_enrich_ttl = 3600
    adv.discover_queue_lastfm_mbid_max_lookups = 10
    prefs.get_advanced_settings.return_value = adv

    return prefs


def _make_service(
    lb_enabled: bool = True,
    lfm_enabled: bool = True,
    primary_source: str = "listenbrainz",
) -> tuple[DiscoverService, AsyncMock, AsyncMock, MagicMock]:
    lb_repo = AsyncMock()
    lb_repo.get_sitewide_top_artists = AsyncMock(return_value=[])
    lb_repo.get_user_fresh_releases = AsyncMock(return_value=None)
    lb_repo.get_user_genre_activity = AsyncMock(return_value=None)
    lb_repo.configure = MagicMock()

    lfm_repo = AsyncMock()
    lfm_repo.get_global_top_artists = AsyncMock(return_value=[])
    lfm_repo.get_user_weekly_artist_chart = AsyncMock(return_value=[])
    lfm_repo.get_user_top_albums = AsyncMock(return_value=[])
    lfm_repo.get_user_recent_tracks = AsyncMock(return_value=[])

    jf_repo = AsyncMock()
    library_repo = AsyncMock()
    mb_repo = AsyncMock()
    prefs = _make_prefs(
        lb_enabled=lb_enabled,
        lfm_enabled=lfm_enabled,
        primary_source=primary_source,
    )

    # discover resolves the user's client via the factory; reuse the same mock repos so existing assertions hold
    factory = MagicMock()
    factory.resolve_listenbrainz = AsyncMock(
        return_value=lb_repo if lb_enabled else None
    )
    factory.resolve_lastfm = AsyncMock(return_value=lfm_repo if lfm_enabled else None)
    factory.resolve_listenbrainz_username = AsyncMock(
        return_value="lbuser" if lb_enabled else None
    )
    factory.resolve_lastfm_username = AsyncMock(
        return_value="lfmuser" if lfm_enabled else None
    )
    factory.is_listenbrainz_linked = AsyncMock(return_value=lb_enabled)
    factory.is_lastfm_linked = AsyncMock(return_value=lfm_enabled)
    prefs_store = MagicMock()
    prefs_store.get = AsyncMock(
        return_value=SimpleNamespace(primary_music_source=primary_source)
    )

    service = DiscoverService(
        listenbrainz_repo=lb_repo,
        jellyfin_repo=jf_repo,
        library_repo=library_repo,
        musicbrainz_repo=mb_repo,
        preferences_service=prefs,
        lastfm_repo=lfm_repo,
        client_factory=factory,
        listening_prefs_store=prefs_store,
    )
    return service, lb_repo, lfm_repo, prefs


class TestResolveSource:
    def test_explicit_listenbrainz(self):
        service, _, _, _ = _make_service(primary_source="lastfm")
        assert service.resolve_source("listenbrainz") == "listenbrainz"

    def test_explicit_lastfm(self):
        service, _, _, _ = _make_service(primary_source="listenbrainz")
        assert service.resolve_source("lastfm") == "lastfm"

    def test_none_uses_global_setting(self):
        service, _, _, _ = _make_service(primary_source="lastfm")
        assert service.resolve_source(None) == "lastfm"

    def test_invalid_uses_global_setting(self):
        service, _, _, _ = _make_service(primary_source="listenbrainz")
        assert service.resolve_source("invalid") == "listenbrainz"


class TestLastFmSectionsAlwaysFetched:
    @pytest.mark.asyncio
    async def test_lastfm_tasks_fetched_when_source_is_listenbrainz(self):
        # last.fm weekly/recent tasks fetch regardless of primary source
        service, lb_repo, lfm_repo, _ = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="listenbrainz"
        )
        await service.build_discover_data(_UID)
        lfm_repo.get_user_weekly_artist_chart.assert_awaited_once()
        lfm_repo.get_user_weekly_album_chart.assert_awaited_once()
        lfm_repo.get_user_recent_tracks.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_lastfm_tasks_fetched_when_source_is_lastfm(self):
        service, _, lfm_repo, _ = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="lastfm"
        )
        await service.build_discover_data(_UID)
        lfm_repo.get_user_weekly_artist_chart.assert_awaited_once()
        lfm_repo.get_user_weekly_album_chart.assert_awaited_once()
        lfm_repo.get_user_recent_tracks.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_lastfm_tasks_not_fetched_when_lfm_disabled(self):
        service, _, lfm_repo, _ = _make_service(
            lb_enabled=True, lfm_enabled=False, primary_source="listenbrainz"
        )
        await service.build_discover_data(_UID)
        lfm_repo.get_user_weekly_artist_chart.assert_not_awaited()
        lfm_repo.get_user_weekly_album_chart.assert_not_awaited()
        lfm_repo.get_user_recent_tracks.assert_not_awaited()


class TestGloballyTrendingSourceSelection:
    @pytest.mark.asyncio
    async def test_listenbrainz_trending_when_source_is_lb(self):
        service, lb_repo, lfm_repo, _ = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="listenbrainz"
        )
        await service.build_discover_data(_UID)
        lb_repo.get_sitewide_top_artists.assert_awaited_once()
        lfm_repo.get_global_top_artists.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_lastfm_trending_when_source_is_lastfm(self):
        service, lb_repo, lfm_repo, _ = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="lastfm"
        )
        await service.build_discover_data(_UID)
        lfm_repo.get_global_top_artists.assert_awaited_once()
        lb_repo.get_sitewide_top_artists.assert_not_awaited()


class TestCacheKeyUserAware:
    def test_different_users_produce_different_keys(self):
        service, _, _, _ = _make_service()
        key_a = service._integration.get_discover_cache_key("user-a", True, False)
        key_b = service._integration.get_discover_cache_key("user-b", True, False)
        assert key_a != key_b

    def test_enable_flags_bust_the_key(self):
        # connecting/disconnecting a service must change the key so the user
        # never sees stale unlinked content
        service, _, _, _ = _make_service()
        key_linked = service._integration.get_discover_cache_key(_UID, True, True)
        key_unlinked = service._integration.get_discover_cache_key(_UID, False, False)
        assert key_linked != key_unlinked


class TestBuildServicePrompts:
    def _make_all_enabled(self) -> DiscoverService:
        prefs = _make_prefs(lb_enabled=True, lfm_enabled=True)
        jf = MagicMock()
        jf.enabled = True
        jf.jellyfin_url = "http://jf"
        jf.api_key = "jf-key"
        prefs.get_jellyfin_connection.return_value = jf
        download_client = MagicMock()
        download_client.enabled = True
        download_client.url = "http://slskd"
        prefs.get_download_client_settings.return_value = download_client
        prefs.is_download_source_ready.return_value = True
        service = DiscoverService(
            listenbrainz_repo=AsyncMock(),
            jellyfin_repo=AsyncMock(),
            library_repo=AsyncMock(),
            musicbrainz_repo=AsyncMock(),
            preferences_service=prefs,
            lastfm_repo=AsyncMock(),
        )
        return service

    def test_lastfm_prompt_shown_when_disabled(self):
        # LB/last.fm prompts are per-user: driven by the passed flags, not service config
        service = self._make_all_enabled()
        prompts = service._homepage._build_service_prompts(
            lb_enabled=True, lfm_enabled=False
        )
        services = [p.service for p in prompts]
        assert "lastfm" in services

    def test_lastfm_prompt_hidden_when_enabled(self):
        service = self._make_all_enabled()
        prompts = service._homepage._build_service_prompts(
            lb_enabled=True, lfm_enabled=True
        )
        services = [p.service for p in prompts]
        assert "lastfm" not in services

    def test_no_prompts_when_all_enabled(self):
        service = self._make_all_enabled()
        prompts = service._homepage._build_service_prompts(
            lb_enabled=True, lfm_enabled=True
        )
        assert prompts == []

    def test_all_prompts_when_nothing_enabled(self):
        service, _, _, _ = _make_service(lb_enabled=False, lfm_enabled=False)
        prompts = service._homepage._build_service_prompts(
            lb_enabled=False, lfm_enabled=False
        )
        services = {p.service for p in prompts}
        assert services == {"download-client", "listenbrainz", "jellyfin", "lastfm"}

    def test_lb_prompt_mentions_lastfm(self):
        service = self._make_all_enabled()
        prompts = service._homepage._build_service_prompts(
            lb_enabled=False, lfm_enabled=True
        )
        lb_prompt = next(p for p in prompts if p.service == "listenbrainz")
        assert "last.fm" in lb_prompt.description.lower()


class TestBuildArtistsYouMightLikeLastFm:
    def test_handles_lastfm_similar_artist_shape(self):
        # LastFmSimilarArtist uses mbid/name, not LB's artist_mbid/artist_name
        from repositories.lastfm_models import LastFmSimilarArtist

        service, _, _, _ = _make_service(primary_source="lastfm")
        seed = MagicMock(artist_name="Radiohead", artist_mbids=["seed-mbid"])
        similar = [
            LastFmSimilarArtist(name="Muse", mbid="muse-mbid", match=0.9),
            LastFmSimilarArtist(name="Coldplay", mbid="coldplay-mbid", match=0.7),
        ]
        results = {"similar_0": similar}
        seen: set[str] = set()
        section = service._homepage._build_artists_you_might_like(
            [seed], results, set(), seen, resolved_source="lastfm"
        )
        assert section is not None
        assert len(section.items) == 2
        assert section.items[0].name == "Muse"
        assert section.items[0].mbid == "muse-mbid"
        assert section.items[1].name == "Coldplay"
        assert section.source == "lastfm"

    def test_handles_listenbrainz_similar_artist_shape(self):
        # LB similar-artist objects use artist_mbid/artist_name (the other shape)
        service, _, _, _ = _make_service(primary_source="listenbrainz")
        seed = MagicMock(artist_name="Radiohead", artist_mbids=["seed-mbid"])
        lb_similar = MagicMock(
            artist_mbid="muse-mbid",
            artist_name="Muse",
            listen_count=500,
        )
        lb_similar.mbid = None
        lb_similar.name = None
        lb_similar.playcount = None
        results = {"similar_0": [lb_similar]}
        seen: set[str] = set()
        section = service._homepage._build_artists_you_might_like(
            [seed], results, set(), seen, resolved_source="listenbrainz"
        )
        assert section is not None
        assert section.items[0].name == "Muse"
        assert section.items[0].mbid == "muse-mbid"
        assert section.source == "listenbrainz"

    def test_skips_artists_without_mbid(self):
        from repositories.lastfm_models import LastFmSimilarArtist

        service, _, _, _ = _make_service()
        seed = MagicMock(artist_name="Seed", artist_mbids=["seed-mbid"])
        similar = [
            LastFmSimilarArtist(name="NoMBID", mbid=None, match=0.5),
        ]
        results = {"similar_0": similar}
        seen: set[str] = set()
        section = service._homepage._build_artists_you_might_like(
            [seed], results, set(), seen, resolved_source="lastfm"
        )
        assert section is None


class TestDiscoverQueuePersonalization:
    @pytest.mark.asyncio
    async def test_queue_uses_non_wildcard_strategies_when_seed_similar_empty(self):
        service, lb_repo, _, _ = _make_service(lb_enabled=True, lfm_enabled=False)
        mb_repo = service._queue._mb_repo

        lb_repo.get_user_top_artists = AsyncMock(
            return_value=[
                ListenBrainzArtist("Seed A", 100, ["seed-a"]),
                ListenBrainzArtist("Seed B", 90, ["seed-b"]),
                ListenBrainzArtist("Seed C", 80, ["seed-c"]),
            ]
        )
        lb_repo.get_similar_artists = AsyncMock(return_value=[])
        lb_repo.get_user_genre_activity = AsyncMock(
            return_value=[ListenBrainzGenreActivity("post-rock", 150)]
        )
        mb_repo.search_release_groups_by_tag = AsyncMock(
            return_value=[
                SearchResult(
                    type="album",
                    title="Genre Album 1",
                    artist="Genre Artist",
                    musicbrainz_id="genre-rg-1",
                ),
                SearchResult(
                    type="album",
                    title="Genre Album 2",
                    artist="Genre Artist",
                    musicbrainz_id="genre-rg-2",
                ),
            ]
        )
        lb_repo.get_user_fresh_releases = AsyncMock(
            return_value=[
                {
                    "release_group_mbid": "fresh-rg-1",
                    "release_name": "Fresh Album",
                    "artist_credit_name": "Fresh Artist",
                    "artist_mbids": ["fresh-artist-1"],
                }
            ]
        )
        lb_repo.get_user_loved_recordings = AsyncMock(
            return_value=[
                ListenBrainzFeedbackRecording(
                    track_name="Loved Track",
                    artist_name="Loved Artist",
                    artist_mbids=["loved-artist-1"],
                )
            ]
        )

        async def top_release_groups_side_effect(
            username: str | None = None,
            range_: str = "this_month",
            count: int = 25,
            offset: int = 0,
        ) -> list[ListenBrainzReleaseGroup]:
            if range_ == "all_time":
                return []
            return [
                ListenBrainzReleaseGroup(
                    release_group_name="Current Top",
                    artist_name="Top Artist",
                    listen_count=10,
                    release_group_mbid="top-current-rg",
                    artist_mbids=["top-artist-1"],
                )
            ]

        lb_repo.get_user_top_release_groups = AsyncMock(
            side_effect=top_release_groups_side_effect
        )

        async def artist_release_groups_side_effect(
            artist_mbid: str,
            count: int = 10,
        ) -> list[ListenBrainzReleaseGroup]:
            if artist_mbid == "loved-artist-1":
                return [
                    ListenBrainzReleaseGroup(
                        release_group_name="Loved Artist Album",
                        artist_name="Loved Artist",
                        listen_count=50,
                        release_group_mbid="loved-rg-1",
                    )
                ]
            if artist_mbid == "top-artist-1":
                return [
                    ListenBrainzReleaseGroup(
                        release_group_name="Deep Cut Album",
                        artist_name="Top Artist",
                        listen_count=20,
                        release_group_mbid="deep-cut-rg-1",
                    )
                ]
            return []

        lb_repo.get_artist_top_release_groups = AsyncMock(
            side_effect=artist_release_groups_side_effect
        )

        lb_repo.get_sitewide_top_release_groups = AsyncMock(
            return_value=[
                ListenBrainzReleaseGroup(
                    release_group_name=f"Wildcard {i}",
                    artist_name=f"Wildcard Artist {i}",
                    listen_count=100 - i,
                    release_group_mbid=f"wild-rg-{i}",
                    artist_mbids=[f"wild-artist-{i}"],
                )
                for i in range(20)
            ]
        )

        queue = await service.build_queue(_UID, count=10)

        assert len(queue.items) == 10
        assert any(not item.is_wildcard for item in queue.items)
        assert any(
            "Because you listen to" in item.recommendation_reason
            or item.recommendation_reason == "New release for you"
            or item.recommendation_reason == "From an artist you love"
            for item in queue.items
        )

    @pytest.mark.asyncio
    async def test_queue_all_wildcards_when_all_personalized_strategies_empty(self):
        service, lb_repo, _, _ = _make_service(lb_enabled=True, lfm_enabled=False)
        mb_repo = service._queue._mb_repo

        lb_repo.get_user_top_artists = AsyncMock(
            return_value=[
                ListenBrainzArtist("Seed A", 100, ["seed-a"]),
                ListenBrainzArtist("Seed B", 90, ["seed-b"]),
                ListenBrainzArtist("Seed C", 80, ["seed-c"]),
            ]
        )
        lb_repo.get_similar_artists = AsyncMock(return_value=[])
        lb_repo.get_user_genre_activity = AsyncMock(return_value=[])
        mb_repo.search_release_groups_by_tag = AsyncMock(return_value=[])
        lb_repo.get_user_fresh_releases = AsyncMock(return_value=[])
        lb_repo.get_user_loved_recordings = AsyncMock(return_value=[])

        async def no_top_release_groups(
            username: str | None = None,
            range_: str = "this_month",
            count: int = 25,
            offset: int = 0,
        ) -> list[ListenBrainzReleaseGroup]:
            return []

        lb_repo.get_user_top_release_groups = AsyncMock(
            side_effect=no_top_release_groups
        )
        lb_repo.get_artist_top_release_groups = AsyncMock(return_value=[])
        lb_repo.get_sitewide_top_release_groups = AsyncMock(
            return_value=[
                ListenBrainzReleaseGroup(
                    release_group_name=f"Wildcard {i}",
                    artist_name=f"Wildcard Artist {i}",
                    listen_count=100 - i,
                    release_group_mbid=f"wild-rg-{i}",
                    artist_mbids=[f"wild-artist-{i}"],
                )
                for i in range(20)
            ]
        )

        queue = await service.build_queue(_UID, count=10)

        assert len(queue.items) == 10
        assert all(item.is_wildcard for item in queue.items)

    @pytest.mark.asyncio
    async def test_deep_cuts_excludes_listened_release_groups(self):
        # only the deep-cuts strategy filters out already-listened release groups
        service, lb_repo, _, _ = _make_service(lb_enabled=True, lfm_enabled=False)
        mb_repo = service._queue._mb_repo

        lb_repo.get_user_top_artists = AsyncMock(
            return_value=[
                ListenBrainzArtist("Seed A", 100, ["seed-a"]),
            ]
        )
        lb_repo.get_similar_artists = AsyncMock(return_value=[])
        lb_repo.get_user_genre_activity = AsyncMock(return_value=[])
        mb_repo.search_release_groups_by_tag = AsyncMock(return_value=[])
        lb_repo.get_user_loved_recordings = AsyncMock(return_value=[])
        lb_repo.get_user_fresh_releases = AsyncMock(return_value=[])

        async def top_release_groups_side_effect(
            username: str | None = None,
            range_: str = "this_month",
            count: int = 25,
            offset: int = 0,
        ) -> list[ListenBrainzReleaseGroup]:
            if range_ == "all_time":
                return [
                    ListenBrainzReleaseGroup(
                        release_group_name="Already Heard",
                        artist_name="Deep Artist",
                        listen_count=120,
                        release_group_mbid="listened-rg-1",
                        artist_mbids=["deep-artist-1"],
                    )
                ]
            if range_ == "this_month":
                return [
                    ListenBrainzReleaseGroup(
                        release_group_name="Current Fav",
                        artist_name="Deep Artist",
                        listen_count=50,
                        release_group_mbid="current-rg-1",
                        artist_mbids=["deep-artist-1"],
                    )
                ]
            return []

        lb_repo.get_user_top_release_groups = AsyncMock(
            side_effect=top_release_groups_side_effect
        )

        async def artist_release_groups_side_effect(
            artist_mbid: str,
            count: int = 10,
        ) -> list[ListenBrainzReleaseGroup]:
            if artist_mbid == "deep-artist-1":
                return [
                    ListenBrainzReleaseGroup(
                        release_group_name="Already Heard",
                        artist_name="Deep Artist",
                        listen_count=120,
                        release_group_mbid="listened-rg-1",
                    ),
                    ListenBrainzReleaseGroup(
                        release_group_name="Unheard Deep Cut",
                        artist_name="Deep Artist",
                        listen_count=5,
                        release_group_mbid="deep-cut-new",
                    ),
                ]
            return []

        lb_repo.get_artist_top_release_groups = AsyncMock(
            side_effect=artist_release_groups_side_effect
        )
        lb_repo.get_sitewide_top_release_groups = AsyncMock(
            return_value=[
                ListenBrainzReleaseGroup(
                    release_group_name=f"Wildcard {i}",
                    artist_name=f"Wildcard Artist {i}",
                    listen_count=100 - i,
                    release_group_mbid=f"wild-rg-{i}",
                    artist_mbids=[f"wild-artist-{i}"],
                )
                for i in range(20)
            ]
        )

        queue = await service.build_queue(_UID, count=10)
        mbids = {item.release_group_mbid.lower() for item in queue.items}

        assert (
            "listened-rg-1" not in mbids
        ), "Deep cuts should exclude listened release groups"
        assert "deep-cut-new" in mbids, "Unheard deep cuts should be included"


class TestDiscoverPerformanceHotpaths:
    @pytest.mark.asyncio
    async def test_missing_essentials_does_not_use_per_artist_sleep(self, monkeypatch):
        service, lb_repo, _, _ = _make_service(lb_enabled=True, lfm_enabled=False)

        sleep_mock = AsyncMock()
        monkeypatch.setattr(
            "services.discover.homepage_service.asyncio.sleep", sleep_mock
        )

        library_albums = [
            MagicMock(artist_mbid="artist-1", musicbrainz_id="in-lib-1"),
            MagicMock(artist_mbid="artist-1", musicbrainz_id="in-lib-2"),
            MagicMock(artist_mbid="artist-1", musicbrainz_id="in-lib-3"),
        ]
        lb_repo.get_artist_top_release_groups = AsyncMock(
            return_value=[
                MagicMock(
                    release_group_mbid="new-rg-1",
                    release_group_name="Missing Album",
                    artist_name="Artist 1",
                    listen_count=25,
                )
            ]
        )

        section = await service._homepage._build_missing_essentials(
            {
                "library_artists": [MagicMock(musicbrainz_id="artist-1")],
                "library_albums": library_albums,
            },
            library_mbids=set(),
        )

        sleep_mock.assert_not_awaited()
        lb_repo.get_artist_top_release_groups.assert_awaited_once_with(
            "artist-1", count=10
        )
        assert section is not None
        assert section.items[0].mbid == "new-rg-1"

    @pytest.mark.asyncio
    async def test_popular_in_genres_lastfm_handles_partial_failures(self):
        service, _, lfm_repo, _ = _make_service(lb_enabled=False, lfm_enabled=True)
        from types import SimpleNamespace

        top_artists = [
            SimpleNamespace(name="A", mbid="a-mbid"),
            SimpleNamespace(name="B", mbid="b-mbid"),
            SimpleNamespace(name="C", mbid="c-mbid"),
        ]

        info_1 = SimpleNamespace(
            tags=[SimpleNamespace(name="Rock"), SimpleNamespace(name="Indie")]
        )
        info_3 = SimpleNamespace(tags=[SimpleNamespace(name="Shoegaze")])
        lfm_repo.get_artist_info = AsyncMock(
            side_effect=[info_1, Exception("boom"), info_3]
        )

        async def _tag_side_effect(genre_name: str, limit: int = 10):
            if genre_name.lower() == "rock":
                return [
                    SimpleNamespace(
                        mbid="artist-rock", name="Rock Artist", playcount=42
                    )
                ]
            if genre_name.lower() == "indie":
                raise Exception("tag fail")
            return [
                SimpleNamespace(mbid="artist-shoe", name="Shoe Artist", playcount=30)
            ]

        lfm_repo.get_tag_top_artists = AsyncMock(side_effect=_tag_side_effect)

        section = await service._homepage._build_popular_in_genres_lastfm(
            {"lfm_user_top_artists_for_genres": top_artists},
            library_mbids=set(),
            seen_artist_mbids=set(),
        )

        assert section is not None
        assert {item.mbid for item in section.items} == {"artist-rock", "artist-shoe"}
        assert section.source == "lastfm"


class TestGloballyTrendingAlwaysBuilt:
    # section visibility is per-user at read time (section_catalog.apply_section_prefs);
    # the build always fetches trending so the shared cache entry stays complete

    @pytest.mark.asyncio
    async def test_trending_always_dispatched(self):
        service, lb_repo, _, _ = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="listenbrainz"
        )
        await service.build_discover_data(_UID)
        lb_repo.get_sitewide_top_artists.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_disabled_section_filtered_at_read_time(self):
        from services.section_catalog import apply_section_prefs

        service, _, _, _ = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="listenbrainz"
        )
        response = await service.build_discover_data(_UID)
        filtered = apply_section_prefs(response, "discover", {"globally_trending"})
        assert filtered.globally_trending is None
        assert filtered.service_prompts == response.service_prompts


class TestDailyMixesWiring:
    # build_discover_data() must populate response.daily_mixes via post_tasks wiring

    @pytest.mark.asyncio
    async def test_daily_mixes_populated_via_post_tasks(self, monkeypatch):
        from api.v1.schemas.home import HomeSection

        service, _lb, _lfm, _prefs = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="listenbrainz"
        )
        stub_sections = [
            HomeSection(
                title="Your Rock Mix", type="albums", items=[], source="listenbrainz"
            ),
            HomeSection(
                title="Your Pop Mix", type="albums", items=[], source="listenbrainz"
            ),
        ]

        async def fake_daily_mix(
            user_id, resolved_source, library_mbids=None, lfm_enabled=False, **_kwargs
        ):
            assert resolved_source == "listenbrainz"
            return stub_sections

        monkeypatch.setattr(
            service._homepage, "_build_daily_mix_sections", fake_daily_mix
        )

        response = await service.build_discover_data(_UID)

        assert response.daily_mixes == stub_sections

    @pytest.mark.asyncio
    async def test_daily_mixes_empty_when_builder_returns_none(self, monkeypatch):
        service, _lb, _lfm, _prefs = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="listenbrainz"
        )

        async def fake_daily_mix(
            user_id, resolved_source, library_mbids=None, lfm_enabled=False, **_kwargs
        ):
            return None

        monkeypatch.setattr(
            service._homepage, "_build_daily_mix_sections", fake_daily_mix
        )

        response = await service.build_discover_data(_UID)

        assert response.daily_mixes == []

    @pytest.mark.asyncio
    async def test_daily_mix_keeps_local_only_familiar_album(self, monkeypatch):
        service, lb_repo, _lfm, _prefs = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="listenbrainz"
        )
        lb_repo.get_artist_top_release_groups.return_value = []
        service._homepage._genre_index = AsyncMock()
        service._homepage._genre_index.get_albums_by_genre.return_value = [
            {
                "local_id": "local-album-1",
                "mbid": None,
                "title": "Local Only",
                "artist_name": "Local Artist",
            }
        ]
        monkeypatch.setattr(
            service._homepage,
            "_similar_artist_album_pools",
            AsyncMock(return_value=[]),
        )

        section = await service._homepage._build_single_daily_mix(
            0,
            "ambient",
            ["60000000-0000-4000-8000-000000000001"],
            "listenbrainz",
            set(),
        )

        assert section is not None
        assert [(item.local_id, item.mbid) for item in section.items] == [
            ("local-album-1", None)
        ]
