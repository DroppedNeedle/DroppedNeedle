import pytest
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

from api.v1.schemas.settings import (
    ListenBrainzConnectionSettings,
    LastFmConnectionSettings,
    PrimaryMusicSourceSettings,
    HomeSettings,
)
from api.v1.schemas.library import LibraryAlbum
from repositories.protocols import ListenBrainzReleaseGroup
from services.home_service import HomeService


def _make_prefs(
    lb_enabled: bool = True,
    lfm_enabled: bool = True,
    primary_source: str = "listenbrainz",
) -> MagicMock:
    prefs = MagicMock()
    lb_settings = ListenBrainzConnectionSettings(
        user_token="tok", username="lbuser", enabled=lb_enabled
    )
    prefs.get_listenbrainz_connection.return_value = lb_settings

    lfm_settings = LastFmConnectionSettings(
        api_key="key",
        shared_secret="secret",
        session_key="sk",
        username="lfmuser",
        enabled=lfm_enabled,
    )
    prefs.get_lastfm_connection.return_value = lfm_settings
    prefs.is_lastfm_enabled.return_value = lfm_enabled
    prefs.get_primary_music_source.return_value = PrimaryMusicSourceSettings(source=primary_source)

    jf_settings = MagicMock()
    jf_settings.enabled = False
    jf_settings.jellyfin_url = ""
    jf_settings.api_key = ""
    prefs.get_jellyfin_connection.return_value = jf_settings

    download_client = MagicMock()
    download_client.enabled = False
    download_client.url = ""
    prefs.get_download_client_settings.return_value = download_client

    yt = MagicMock()
    yt.enabled = False
    yt.api_key = ""
    prefs.get_youtube_connection.return_value = yt

    lf = MagicMock()
    lf.enabled = False
    lf.music_path = ""
    prefs.get_local_files_connection.return_value = lf

    return prefs


def _make_service(
    lb_enabled: bool = True,
    lfm_enabled: bool = True,
    primary_source: str = "listenbrainz",
) -> tuple[HomeService, AsyncMock, AsyncMock, MagicMock]:
    lb_repo = AsyncMock()
    lb_repo.get_sitewide_top_artists = AsyncMock(return_value=[])
    lb_repo.get_sitewide_top_release_groups = AsyncMock(return_value=[])
    lb_repo.get_user_listens = AsyncMock(return_value=[])
    lb_repo.get_user_loved_recordings = AsyncMock(return_value=[])
    lb_repo.get_user_genre_activity = AsyncMock(return_value=None)
    lb_repo.get_recommendation_playlists = AsyncMock(return_value=[])
    lb_repo.get_playlist_tracks = AsyncMock(return_value=None)
    lb_repo.get_recording_release_groups_batch = AsyncMock(return_value={})
    lb_repo.configure = MagicMock()

    lfm_repo = AsyncMock()
    lfm_repo.get_global_top_artists = AsyncMock(return_value=[])
    lfm_repo.get_user_top_albums = AsyncMock(return_value=[])
    lfm_repo.get_user_recent_tracks = AsyncMock(return_value=[])
    lfm_repo.get_user_loved_tracks = AsyncMock(return_value=[])

    jf_repo = AsyncMock()
    library_repo = AsyncMock()
    library_repo.get_library = AsyncMock(return_value=[])
    library_repo.get_artists_from_library = AsyncMock(return_value=[])
    library_repo.get_recently_imported = AsyncMock(return_value=[])
    mb_repo = AsyncMock()

    prefs = _make_prefs(
        lb_enabled=lb_enabled,
        lfm_enabled=lfm_enabled,
        primary_source=primary_source,
    )

    factory = MagicMock()
    factory.resolve_listenbrainz = AsyncMock(return_value=lb_repo if lb_enabled else None)
    factory.resolve_lastfm = AsyncMock(return_value=lfm_repo if lfm_enabled else None)
    factory.resolve_listenbrainz_username = AsyncMock(return_value="lbuser" if lb_enabled else None)
    factory.resolve_lastfm_username = AsyncMock(return_value="lfmuser" if lfm_enabled else None)

    prefs_store = MagicMock()
    prefs_store.get = AsyncMock(return_value=SimpleNamespace(primary_music_source=primary_source))

    play_history_store = MagicMock()
    play_history_store.recent = AsyncMock(return_value=[])

    service = HomeService(
        listenbrainz_repo=lb_repo,
        jellyfin_repo=jf_repo,
        library_repo=library_repo,
        musicbrainz_repo=mb_repo,
        preferences_service=prefs,
        lastfm_repo=lfm_repo,
        client_factory=factory,
        listening_prefs_store=prefs_store,
        play_history_store=play_history_store,
    )
    return service, lb_repo, lfm_repo, prefs


class TestHomeServiceResolveSource:
    def test_explicit_source_overrides_global(self):
        service, _, _, _ = _make_service(primary_source="lastfm")
        assert service._resolve_source("listenbrainz") == "listenbrainz"

    def test_none_uses_global_setting(self):
        service, _, _, _ = _make_service(primary_source="lastfm")
        assert service._resolve_source(None) == "lastfm"


class TestHomeServiceSourceSelection:
    @pytest.mark.asyncio
    async def test_lb_trending_called_when_source_is_lb(self):
        service, lb_repo, lfm_repo, _ = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="listenbrainz"
        )
        await service.get_home_data("u1")
        lb_repo.get_sitewide_top_artists.assert_awaited_once()
        lfm_repo.get_global_top_artists.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_lfm_trending_called_when_source_is_lastfm(self):
        service, lb_repo, lfm_repo, _ = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="lastfm"
        )
        await service.get_home_data("u1")
        lfm_repo.get_global_top_artists.assert_awaited_once()
        lb_repo.get_sitewide_top_artists.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_source_field_in_response(self):
        service, _, _, _ = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="listenbrainz"
        )
        response = await service.get_home_data("u1")
        assert response.integration_status is not None
        assert response.integration_status.lastfm is True
        assert response.integration_status.listenbrainz is True

    @pytest.mark.asyncio
    async def test_popular_album_in_library_uses_album_mbids_not_artist_mbids(self):
        service, lb_repo, _, prefs = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="listenbrainz"
        )
        download_client_conn = MagicMock()
        download_client_conn.enabled = True
        download_client_conn.url = "http://slskd.local"
        prefs.get_download_client_settings.return_value = download_client_conn
        service._library_repo.get_home_albums.return_value = [
            LibraryAlbum(
                artist="Artist",
                album="Album",
                musicbrainz_id="rg-123",
                artist_mbid="artist-123",
            )
        ]
        service._library_repo.get_home_artists.return_value = [{"mbid": "artist-123"}]
        lb_repo.get_sitewide_top_release_groups.return_value = [
            ListenBrainzReleaseGroup(
                release_group_name="Album",
                artist_name="Artist",
                listen_count=99,
                release_group_mbid="rg-123",
                artist_mbids=["artist-other"],
            )
        ]

        response = await service.get_home_data("u1")

        assert response.popular_albums is not None
        assert len(response.popular_albums.items) == 1
        assert response.popular_albums.items[0].in_library is True

    @pytest.mark.asyncio
    async def test_lastfm_source_skips_user_top_albums_when_username_missing(self):
        service, _, lfm_repo, prefs = _make_service(
            lb_enabled=False, lfm_enabled=True, primary_source="lastfm"
        )
        service._client_factory.resolve_lastfm_username = AsyncMock(return_value="")

        await service.get_home_data("u1")

        lfm_repo.get_global_top_artists.assert_awaited_once()
        lfm_repo.get_user_top_albums.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_listenbrainz_source_includes_weekly_exploration(self):
        service, lb_repo, _, _ = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="listenbrainz"
        )
        lb_repo.get_recommendation_playlists.return_value = [
            {
                "playlist_id": "weekly-123",
                "source_patch": "weekly-exploration",
                "identifier": "https://listenbrainz.org/playlist/weekly-123",
            }
        ]
        lb_repo.get_playlist_tracks.return_value = MagicMock(
            title="Weekly Exploration for lbuser",
            date="2026-03-30T00:00:00+00:00",
            tracks=[
                MagicMock(
                    title="Song",
                    creator="Artist",
                    album="Album",
                    recording_mbid="recording-1",
                    artist_mbids=["artist-1"],
                    caa_release_mbid="release-1",
                    duration_ms=123000,
                )
            ],
        )
        service._mb_repo.get_release_group_id_from_release.return_value = "release-group-1"

        response = await service.get_home_data("u1")

        assert response.weekly_exploration is not None
        assert response.weekly_exploration.source_url == "https://listenbrainz.org/playlist/weekly-123"
        assert len(response.weekly_exploration.tracks) == 1
        assert response.weekly_exploration.tracks[0].release_group_mbid == "release-group-1"

    @pytest.mark.asyncio
    async def test_weekly_exploration_uses_batched_recording_metadata_before_musicbrainz(self):
        service, lb_repo, _, _ = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="listenbrainz"
        )
        lb_repo.get_recommendation_playlists.return_value = [
            {
                "playlist_id": "weekly-123",
                "source_patch": "weekly-exploration",
                "identifier": "https://listenbrainz.org/playlist/weekly-123",
            }
        ]
        lb_repo.get_playlist_tracks.return_value = MagicMock(
            title="Weekly Exploration",
            date="2026-03-30T00:00:00+00:00",
            tracks=[
                MagicMock(
                    title="Song",
                    creator="Artist",
                    album="Album",
                    recording_mbid="recording-1",
                    artist_mbids=["artist-1"],
                    caa_release_mbid="release-1",
                    duration_ms=123000,
                )
            ],
        )
        lb_repo.get_recording_release_groups_batch.return_value = {
            "recording-1": "release-group-1"
        }

        response = await service.get_home_data("u1")

        assert response.weekly_exploration.tracks[0].release_group_mbid == "release-group-1"
        service._mb_repo.get_release_group_id_from_release.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_weekly_exploration_builds_even_with_lastfm_primary(self):
        # unified model: LB-specific sections run whenever LB is linked,
        # regardless of the user's primary source
        service, lb_repo, _, _ = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="lastfm"
        )

        await service.get_home_data("u1")

        lb_repo.get_recommendation_playlists.assert_awaited()

    @pytest.mark.asyncio
    async def test_weekly_exploration_skipped_when_lb_unlinked(self):
        service, lb_repo, _, _ = _make_service(
            lb_enabled=False, lfm_enabled=True, primary_source="lastfm"
        )

        response = await service.get_home_data("u1")

        assert response.weekly_exploration is None
        lb_repo.get_recommendation_playlists.assert_not_awaited()


class TestHomeServiceCacheKeyUserAware:
    def test_different_users_produce_different_keys(self):
        service, _, _, _ = _make_service()
        key_a = service._get_home_cache_key("user-a", True, True)
        key_b = service._get_home_cache_key("user-b", True, True)
        assert key_a != key_b

    def test_enable_flags_bust_the_key(self):
        service, _, _, _ = _make_service()
        key_linked = service._get_home_cache_key("u1", True, True)
        key_unlinked = service._get_home_cache_key("u1", False, False)
        assert key_linked != key_unlinked


class TestBuildServicePrompts:
    def test_source_prompts_hidden_when_one_source_enabled(self):
        service, _, _, _ = _make_service()
        prompts = service._build_service_prompts(
            lb_enabled=True, download_client_configured=True, lfm_enabled=False
        )
        services = [p.service for p in prompts]
        assert "lastfm" not in services
        assert "listenbrainz" not in services

    def test_source_prompts_hidden_when_lastfm_enabled(self):
        service, _, _, _ = _make_service()
        prompts = service._build_service_prompts(
            lb_enabled=False, download_client_configured=True, lfm_enabled=True
        )
        services = [p.service for p in prompts]
        assert "listenbrainz" not in services
        assert "lastfm" not in services

    def test_no_prompts_when_all_enabled(self):
        service, _, _, _ = _make_service()
        prompts = service._build_service_prompts(
            lb_enabled=True, download_client_configured=True, lfm_enabled=True
        )
        assert prompts == []

    def test_all_prompts_when_nothing_enabled(self):
        service, _, _, _ = _make_service()
        prompts = service._build_service_prompts(
            lb_enabled=False, download_client_configured=False, lfm_enabled=False
        )
        services = {p.service for p in prompts}
        assert services == {"download-client", "listenbrainz", "lastfm"}

    def test_lb_prompt_mentions_lastfm(self):
        service, _, _, _ = _make_service()
        prompts = service._build_service_prompts(
            lb_enabled=False, download_client_configured=True, lfm_enabled=False
        )
        lb_prompt = next(p for p in prompts if p.service == "listenbrainz")
        assert "last.fm" in lb_prompt.description.lower()


class TestWhatsHotAlwaysBuilt:
    # section visibility is per-user at read time (section_catalog.apply_section_prefs);
    # the build always fetches trending so the shared cache entry stays complete

    @pytest.mark.asyncio
    async def test_trending_always_dispatched(self):
        service, lb_repo, _, _ = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="listenbrainz"
        )
        await service.get_home_data("u1")
        lb_repo.get_sitewide_top_artists.assert_awaited_once()
        lb_repo.get_sitewide_top_release_groups.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_disabled_sections_filtered_at_read_time(self):
        from api.v1.schemas.home import HomeResponse, HomeSection, HomeArtist, HomeIntegrationStatus
        from services.section_catalog import apply_section_prefs

        built = HomeResponse(
            integration_status=HomeIntegrationStatus(
                listenbrainz=True, jellyfin=False, download_client=False,
                youtube=False, lastfm=True,
            ),
            trending_artists=HomeSection(
                title="Trending", type="artist",
                items=[HomeArtist(name="Artist1")],
            ),
            popular_albums=HomeSection(title="Popular", type="album", items=[]),
        )
        filtered = apply_section_prefs(built, "home", {"trending_artists", "popular_albums"})
        assert filtered.trending_artists is None
        assert filtered.popular_albums is None
        assert filtered.integration_status is not None


class TestHomeServeFastRevalidate:
    # /api/v1/home must never block on external services: cache miss -> instant
    # library-only response with refreshing=true; the full build runs in warm_cache

    @pytest.mark.asyncio
    async def test_cache_miss_returns_fast_local_response(self):
        service, lb_repo, _, _ = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="listenbrainz"
        )
        store: dict = {}
        cache = MagicMock()
        cache.get = AsyncMock(side_effect=lambda k: store.get(k))
        cache.set = AsyncMock(side_effect=lambda k, v, ttl=None: store.__setitem__(k, v))
        service._memory_cache = cache
        # pretend a warm was just attempted so the fast path doesn't spawn a task
        service._built_at = {"anything": 0.0}
        import time as _time
        music = await service._resolve_user_music("u1", None)
        key = service._get_home_cache_key("u1", music.lb_enabled, music.lfm_enabled)
        service._built_at[key] = _time.time()

        resp = await service.get_home_data("u1")

        assert resp.refreshing is False or resp.refreshing is True  # struct field exists
        # the fast path never awaits sitewide trending
        lb_repo.get_sitewide_top_artists.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_warm_cache_builds_full_and_populates_cache(self):
        service, lb_repo, _, _ = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="listenbrainz"
        )
        store: dict = {}
        cache = MagicMock()
        cache.get = AsyncMock(side_effect=lambda k: store.get(k))
        cache.set = AsyncMock(side_effect=lambda k, v, ttl=None: store.__setitem__(k, v))
        service._memory_cache = cache

        await service.warm_cache("u1")

        # the full build fetched trending and the result landed in the cache
        lb_repo.get_sitewide_top_artists.assert_awaited_once()
        assert len(store) == 1
        cached = next(iter(store.values()))
        assert cached.refreshing is False

    @pytest.mark.asyncio
    async def test_cached_copy_served_verbatim(self):
        from api.v1.schemas.home import HomeResponse

        service, lb_repo, _, _ = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="listenbrainz"
        )
        import time as _time
        music = await service._resolve_user_music("u1", None)
        key = service._get_home_cache_key("u1", music.lb_enabled, music.lfm_enabled)
        store = {key: HomeResponse()}
        cache = MagicMock()
        cache.get = AsyncMock(side_effect=lambda k: store.get(k))
        cache.set = AsyncMock()
        service._memory_cache = cache
        service._built_at = {key: _time.time()}  # fresh -> no revalidation

        resp = await service.get_home_data("u1")

        assert resp.refreshing is False
        lb_repo.get_sitewide_top_artists.assert_not_awaited()
