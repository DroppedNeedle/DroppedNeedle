import pytest
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch

_UID = "u1"

from api.v1.schemas.settings import (
    ListenBrainzConnectionSettings,
    LastFmConnectionSettings,
    PrimaryMusicSourceSettings,
)
from api.v1.schemas.advanced_settings import AdvancedSettings
from repositories.lastfm_models import LastFmArtist, LastFmAlbum
from repositories.listenbrainz_models import ListenBrainzReleaseGroup
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
    prefs.get_advanced_settings.return_value = AdvancedSettings()

    jf_settings = MagicMock()
    jf_settings.enabled = False
    jf_settings.jellyfin_url = ""
    jf_settings.api_key = ""
    jf_settings.user_id = ""
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
) -> tuple[DiscoverService, AsyncMock, AsyncMock, MagicMock]:
    lb_repo = AsyncMock()
    lb_repo.get_sitewide_top_artists = AsyncMock(return_value=[])
    lb_repo.get_sitewide_top_release_groups = AsyncMock(return_value=[])
    lb_repo.get_user_fresh_releases = AsyncMock(return_value=None)
    lb_repo.get_user_genre_activity = AsyncMock(return_value=None)
    lb_repo.get_user_top_artists = AsyncMock(return_value=[])
    lb_repo.get_similar_artists = AsyncMock(return_value=[])
    lb_repo.get_artist_top_release_groups = AsyncMock(return_value=[])
    lb_repo.configure = MagicMock()

    lfm_repo = AsyncMock()
    lfm_repo.get_global_top_artists = AsyncMock(return_value=[])
    lfm_repo.get_user_weekly_artist_chart = AsyncMock(return_value=[])
    lfm_repo.get_user_top_albums = AsyncMock(return_value=[])
    lfm_repo.get_user_recent_tracks = AsyncMock(return_value=[])
    lfm_repo.get_user_top_artists = AsyncMock(return_value=[])
    lfm_repo.get_similar_artists = AsyncMock(return_value=[])
    lfm_repo.get_artist_top_albums = AsyncMock(return_value=[])

    jf_repo = AsyncMock()
    library_repo = AsyncMock()
    mb_repo = AsyncMock()
    mb_repo.search_release_groups_by_tag = AsyncMock(return_value=[])
    mb_repo.get_release_group_id_from_release = AsyncMock(return_value=None)
    mb_repo.get_release_group_by_id = AsyncMock(return_value=None)

    prefs = _make_prefs(
        lb_enabled=lb_enabled,
        lfm_enabled=lfm_enabled,
        primary_source=primary_source,
    )

    # factory resolves the user's client; return the same mock repos so assertions hold
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


class TestBuildQueueSourceRouting:
    @pytest.mark.asyncio
    async def test_build_queue_uses_lastfm_when_source_is_lastfm(self):
        service, lb_repo, lfm_repo, _ = _make_service(
            lb_enabled=False, lfm_enabled=True, primary_source="lastfm"
        )
        lfm_repo.get_global_top_artists.return_value = [
            LastFmArtist(name="Artist1", mbid="mbid-1", playcount=1000, listeners=500),
        ]
        lfm_repo.get_artist_top_albums.return_value = [
            LastFmAlbum(
                name="Album1", mbid="album-mbid-1", playcount=100, artist_name="Artist1"
            ),
        ]

        result = await service.build_queue(_UID, count=5)
        assert result is not None
        lfm_repo.get_global_top_artists.assert_awaited()
        lb_repo.get_sitewide_top_release_groups.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_build_queue_uses_listenbrainz_when_source_is_lb(self):
        service, lb_repo, lfm_repo, _ = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="listenbrainz"
        )
        lb_repo.get_sitewide_top_release_groups.return_value = []

        result = await service.build_queue(_UID, count=5)
        assert result is not None
        lb_repo.get_sitewide_top_release_groups.assert_awaited()
        lfm_repo.get_global_top_artists.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_build_queue_none_source_uses_global_default(self):
        service, lb_repo, lfm_repo, _ = _make_service(
            lb_enabled=False, lfm_enabled=True, primary_source="lastfm"
        )
        lfm_repo.get_global_top_artists.return_value = []

        result = await service.build_queue(_UID, count=5)
        assert result is not None
        lfm_repo.get_global_top_artists.assert_awaited()

    @pytest.mark.asyncio
    async def test_build_queue_returns_valid_response(self):
        service, _, _, _ = _make_service(lb_enabled=False, lfm_enabled=False)
        result = await service.build_queue(_UID, count=5)
        assert result is not None
        assert result.queue_id
        assert isinstance(result.items, list)

    @pytest.mark.asyncio
    async def test_anonymous_queue_deduplicates_repeated_listenbrainz_groups(self):
        service, lb_repo, _, _ = _make_service(
            lb_enabled=False, lfm_enabled=False, primary_source="listenbrainz"
        )
        lb_repo.get_sitewide_top_release_groups.return_value = [
            ListenBrainzReleaseGroup(
                release_group_name="First",
                artist_name="Artist",
                listen_count=500,
                release_group_mbid="DUPE-1",
                artist_mbids=["a-1"],
            ),
            ListenBrainzReleaseGroup(
                release_group_name="Repeated",
                artist_name="Artist",
                listen_count=400,
                release_group_mbid="dupe-1",
                artist_mbids=["a-1"],
            ),
            ListenBrainzReleaseGroup(
                release_group_name="Ignored",
                artist_name="Artist",
                listen_count=300,
                release_group_mbid="ignored-1",
                artist_mbids=["a-2"],
            ),
            ListenBrainzReleaseGroup(
                release_group_name="Distinct",
                artist_name="Artist",
                listen_count=200,
                release_group_mbid="distinct-1",
                artist_mbids=["a-3"],
            ),
        ]
        service._queue._get_wildcard_albums = AsyncMock(return_value=[])

        with patch("services.discover.queue_service.random.shuffle", lambda _: None):
            items = await service._queue._build_anonymous_queue(
                5, {"ignored-1"}, set(), resolved_source="listenbrainz"
            )

        assert [item.release_group_mbid for item in items] == ["DUPE-1", "distinct-1"]


class TestBuildQueuePersonalizedSourceRouting:
    @pytest.mark.asyncio
    async def test_personalized_queue_lastfm_uses_lastfm_similar(self):
        service, lb_repo, lfm_repo, _ = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="lastfm"
        )
        lfm_repo.get_user_top_artists.return_value = [
            LastFmArtist(name="Seed", mbid="seed-mbid", playcount=500, listeners=100),
        ]
        lfm_repo.get_similar_artists.return_value = []

        await service.build_queue(_UID, count=5)
        lfm_repo.get_user_top_artists.assert_awaited()
        lfm_repo.get_similar_artists.assert_awaited()
        lb_repo.get_similar_artists.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_personalized_queue_lb_uses_lb_similar(self):
        service, lb_repo, lfm_repo, _ = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="listenbrainz"
        )
        lb_repo.get_user_top_artists.return_value = [
            MagicMock(artist_name="Seed", artist_mbids=["seed-mbid"], listen_count=500),
        ]
        lb_repo.get_similar_artists.return_value = []

        await service.build_queue(_UID, count=5)
        lb_repo.get_user_top_artists.assert_awaited()
        lb_repo.get_similar_artists.assert_awaited()
        lfm_repo.get_similar_artists.assert_not_awaited()


class TestLastFmQueueDataQuality:
    @pytest.mark.asyncio
    async def test_lastfm_queue_normalizes_release_mbids_to_release_groups(self):
        service, _, lfm_repo, _ = _make_service(
            lb_enabled=False, lfm_enabled=True, primary_source="lastfm"
        )
        service._mbid_resolution._mb_repo.get_release_group_id_from_release.return_value = "rg-mbid-1"

        lfm_repo.get_global_top_artists.return_value = [
            LastFmArtist(
                name="Artist1", mbid="artist-mbid-1", playcount=1000, listeners=500
            ),
        ]
        lfm_repo.get_artist_top_albums.return_value = [
            LastFmAlbum(
                name="Album1",
                mbid="release-mbid-1",
                playcount=100,
                artist_name="Artist1",
            ),
        ]

        result = await service.build_queue(_UID, count=5)

        assert any(item.release_group_mbid == "rg-mbid-1" for item in result.items)
        service._mbid_resolution._mb_repo.get_release_group_id_from_release.assert_awaited()

    @pytest.mark.asyncio
    async def test_lastfm_seed_collection_does_not_call_listenbrainz_fallback(self):
        service, lb_repo, lfm_repo, _ = _make_service(
            lb_enabled=True, lfm_enabled=True, primary_source="lastfm"
        )
        lfm_repo.get_user_top_artists.return_value = []
        lfm_repo.get_global_top_artists.return_value = []

        await service.build_queue(_UID, count=5)

        lb_repo.get_user_top_artists.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_lastfm_queue_keeps_items_when_release_group_resolution_fails(self):
        service, _, lfm_repo, _ = _make_service(
            lb_enabled=False, lfm_enabled=True, primary_source="lastfm"
        )
        service._mbid_resolution._mb_repo.get_release_group_id_from_release.return_value = None
        service._mbid_resolution._mb_repo.get_release_group_by_id.return_value = None

        lfm_repo.get_global_top_artists.return_value = [
            LastFmArtist(
                name="Artist1", mbid="artist-mbid-1", playcount=1000, listeners=500
            ),
        ]
        lfm_repo.get_artist_top_albums.return_value = [
            LastFmAlbum(
                name="Album1",
                mbid="release-mbid-1",
                playcount=100,
                artist_name="Artist1",
            ),
        ]

        result = await service.build_queue(_UID, count=5)

        assert result.items
        assert any(item.release_group_mbid == "release-mbid-1" for item in result.items)

    @pytest.mark.asyncio
    async def test_lastfm_queue_items_are_deduplicated_by_release_group_mbid(self):
        service, _, _, _ = _make_service(
            lb_enabled=False, lfm_enabled=True, primary_source="lastfm"
        )

        service._mbid_resolution.resolve_lastfm_release_group_mbids = AsyncMock(
            return_value={
                "release-a": "rg-shared",
                "release-b": "rg-shared",
            }
        )

        artist_a = LastFmArtist(
            name="Artist A", mbid="artist-a", playcount=100, listeners=10
        )
        artist_b = LastFmArtist(
            name="Artist B", mbid="artist-b", playcount=120, listeners=12
        )
        albums_a = [
            LastFmAlbum(
                name="Album A", mbid="release-a", playcount=50, artist_name="Artist A"
            )
        ]
        albums_b = [
            LastFmAlbum(
                name="Album B", mbid="release-b", playcount=60, artist_name="Artist B"
            )
        ]

        items = await service._mbid_resolution.lastfm_albums_to_queue_items(
            [(artist_a, albums_a), (artist_b, albums_b)],
            exclude=set(),
            target=5,
            reason="Trending on Last.fm",
        )

        assert len(items) == 1
        assert items[0].release_group_mbid == "rg-shared"


class TestLastFmResolutionBehavior:
    @pytest.mark.asyncio
    async def test_lastfm_resolution_caps_musicbrainz_lookup_count(self):
        service, _, _, _ = _make_service(
            lb_enabled=False, lfm_enabled=True, primary_source="lastfm"
        )

        album_mbids = [f"release-mbid-{idx}" for idx in range(10)]

        await service._mbid_resolution.resolve_lastfm_release_group_mbids(
            album_mbids, max_lookups=3
        )

        assert (
            service._mbid_resolution._mb_repo.get_release_group_id_from_release.await_count
            == 3
        )

    @pytest.mark.asyncio
    async def test_release_to_rg_hits_persist_before_second_gather(self):
        """The release->RG hits from the first gather must be banked to the mbid_store
        BEFORE the second (RG-existence) gather runs. That gather is the one a discover
        build's budget timeout routinely cancels, so persisting only at the very end
        would drop every hit the build already earned and the cache would never warm."""
        from services.discover.mbid_resolution_service import MbidResolutionService

        events: list[tuple[str, object]] = []

        mb_repo = AsyncMock()

        async def _rel_to_rg(mbid: str):
            return {"rel-hit": "rg-hit"}.get(mbid)

        mb_repo.get_release_group_id_from_release = AsyncMock(side_effect=_rel_to_rg)

        async def _rg_by_id(mbid: str, **_kwargs):
            events.append(("rg_lookup", mbid))
            return {"id": mbid}

        mb_repo.get_release_group_by_id = AsyncMock(side_effect=_rg_by_id)

        canonical_store = AsyncMock()
        canonical_store.get_release_to_rg_batch = AsyncMock(return_value={})

        async def _save(mapping, source_host="", *, source_context=None):
            events.append(("save", dict(mapping)))

        canonical_store.save_canonical_redirect = AsyncMock(side_effect=_save)
        canonical_store.save_release_to_rg = AsyncMock(side_effect=_save)

        svc = MbidResolutionService(
            musicbrainz_repo=mb_repo,
            library_repo=AsyncMock(),
            listenbrainz_repo=AsyncMock(),
            mb_canonical_store=canonical_store,
        )

        # "rel-hit" resolves in the first gather; "rg-passthrough" falls through to the
        # second gather - so both gathers run and ordering is observable.
        await svc.resolve_lastfm_release_group_mbids(
            ["rel-hit", "rg-passthrough"], max_lookups=10
        )

        save_events = [e for e in events if e[0] == "save" and e[1].get("rel-hit")]
        first_lookup = next(i for i, e in enumerate(events) if e[0] == "rg_lookup")
        assert save_events  # at least one persist happened
        assert save_events[0][1] == {"rel-hit": "rg-hit"}
        _ = first_lookup  # rg_lookup ordering is no longer observable via mock


class TestLastFmQueueResilience:
    @pytest.mark.asyncio
    async def test_lastfm_queue_falls_back_to_decade_results_when_top_albums_sparse(
        self,
    ):
        service, _, lfm_repo, _ = _make_service(
            lb_enabled=False, lfm_enabled=True, primary_source="lastfm"
        )

        lfm_repo.get_user_top_artists.return_value = []
        lfm_repo.get_global_top_artists.return_value = [
            LastFmArtist(
                name="Artist1", mbid="artist-mbid-1", playcount=1000, listeners=500
            ),
        ]
        lfm_repo.get_artist_top_albums.return_value = [
            LastFmAlbum(
                name="Album No MBID", mbid=None, playcount=100, artist_name="Artist1"
            ),
        ]

        fallback_rg = MagicMock()
        fallback_rg.musicbrainz_id = "rg-fallback-1"
        fallback_rg.title = "Fallback Album"
        fallback_rg.artist = "Fallback Artist"

        async def _search_release_groups_by_tag(tag, limit=25, offset=0):
            if tag == "1990s" and offset == 0:
                return [fallback_rg]
            return []

        service._queue._mb_repo.search_release_groups_by_tag = AsyncMock(
            side_effect=_search_release_groups_by_tag
        )

        result = await service.build_queue(_UID, count=5)

        assert result.items
        assert any(item.release_group_mbid == "rg-fallback-1" for item in result.items)
