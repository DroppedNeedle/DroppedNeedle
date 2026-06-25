"""Tests for now-playing and session service methods."""
from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock, PropertyMock

import pytest

from repositories.plex_models import PlexSession
from repositories.navidrome_models import SubsonicNowPlayingEntry
from repositories.jellyfin_models import JellyfinSession
from services.plex_library_service import PlexLibraryService
from services.navidrome_library_service import NavidromeLibraryService
from services.jellyfin_library_service import JellyfinLibraryService


def _make_plex_service() -> tuple[PlexLibraryService, MagicMock]:
    repo = MagicMock()
    repo.get_sessions = AsyncMock(return_value=[])
    repo.get_albums = AsyncMock(return_value=([], 0))
    repo.get_artists = AsyncMock(return_value=[])
    repo.get_album_metadata = AsyncMock()
    repo.get_album_tracks = AsyncMock(return_value=[])
    repo.get_recently_added = AsyncMock(return_value=[])
    repo.get_recently_viewed = AsyncMock(return_value=[])
    repo.get_genres = AsyncMock(return_value=[])
    repo.get_track_count = AsyncMock(return_value=0)
    repo.get_artist_count = AsyncMock(return_value=0)
    repo.search = AsyncMock(return_value={"albums": [], "tracks": [], "artists": []})
    type(repo).stats_ttl = PropertyMock(return_value=600)

    prefs = MagicMock()
    conn = MagicMock()
    conn.enabled = True
    conn.plex_url = "http://plex:32400"
    conn.plex_token = "tok"
    conn.music_library_ids = ["1"]
    prefs.get_plex_connection_raw.return_value = conn

    svc = PlexLibraryService(plex_repo=repo, preferences_service=prefs)
    return svc, repo


def _plex_session(**overrides) -> PlexSession:
    defaults = dict(
        session_id="sess1",
        user_name="alice",
        track_title="Song A",
        artist_name="Artist A",
        album_name="Album A",
        album_thumb="/library/metadata/200/thumb",
        player_device="iPhone",
        player_platform="iOS",
        player_state="playing",
        is_direct_play=True,
        progress_ms=60000,
        duration_ms=180000,
        audio_codec="flac",
        audio_channels=2,
        bitrate=1411,
    )
    defaults.update(overrides)
    return PlexSession(**defaults)


def _make_navidrome_service() -> tuple[NavidromeLibraryService, MagicMock]:
    repo = MagicMock()
    repo.get_now_playing = AsyncMock(return_value=[])
    repo.get_albums = AsyncMock(return_value=[])
    repo.get_album_info = AsyncMock()
    repo.get_album_tracks = AsyncMock(return_value=[])
    repo.get_starred = AsyncMock()
    repo.get_artists = AsyncMock(return_value=[])
    repo.get_artist = AsyncMock()
    repo.get_artist_info = AsyncMock()
    repo.search = AsyncMock()
    repo.get_genres = AsyncMock(return_value=[])
    repo.now_playing = AsyncMock(return_value=True)
    repo.get_playlists = AsyncMock(return_value=[])
    repo.get_playlist = AsyncMock()
    repo.get_random_songs = AsyncMock(return_value=[])

    prefs = MagicMock()
    prefs.get_navidrome_connection_raw.return_value = MagicMock(enabled=True)

    svc = NavidromeLibraryService(navidrome_repo=repo, preferences_service=prefs)
    return svc, repo


def _navidrome_entry(**overrides) -> SubsonicNowPlayingEntry:
    defaults = dict(
        id="np1",
        title="Song N",
        artist="Artist N",
        album="Album N",
        albumId="al1",
        artistId="ar1",
        coverArt="cov1",
        duration=240,
        bitRate=320,
        suffix="mp3",
        username="bob",
        minutesAgo=2,
        playerId=1,
        playerName="Firefox",
    )
    defaults.update(overrides)
    return SubsonicNowPlayingEntry(**defaults)


def _make_jellyfin_service() -> tuple[JellyfinLibraryService, MagicMock]:
    repo = MagicMock()
    repo.get_sessions = AsyncMock(return_value=[])
    repo.get_recently_played = AsyncMock(return_value=[])
    repo.get_favorites = AsyncMock(return_value=[])
    repo.get_albums = AsyncMock(return_value=[])
    repo.get_artists = AsyncMock(return_value=[])
    repo.get_album = AsyncMock()
    repo.get_album_tracks = AsyncMock(return_value=[])
    repo.search = AsyncMock()
    repo.get_genres = AsyncMock(return_value=[])
    repo.get_recently_added = AsyncMock(return_value=[])
    repo.get_most_played_artists = AsyncMock(return_value=[])
    repo.get_most_played_albums = AsyncMock(return_value=[])
    repo.get_playlists = AsyncMock(return_value=[])
    repo.get_playlist = AsyncMock()
    repo.get_image_url = MagicMock(return_value="https://jellyfin/Items/img/Primary")

    prefs = MagicMock()
    prefs.get_jellyfin_connection_raw.return_value = MagicMock(enabled=True)

    svc = JellyfinLibraryService(jellyfin_repo=repo, preferences_service=prefs)
    return svc, repo


def _jellyfin_session(**overrides) -> JellyfinSession:
    defaults = dict(
        session_id="jsess1",
        user_name="carol",
        device_name="Chrome",
        client_name="Jellyfin Web",
        now_playing_name="Song J",
        now_playing_artist="Artist J",
        now_playing_album="Album J",
        now_playing_album_id="jalb1",
        now_playing_item_id="jitem1",
        now_playing_image_tag="tag1",
        position_ticks=600_000_000,
        runtime_ticks=3_000_000_000,
        is_paused=False,
        is_muted=False,
        play_method="DirectPlay",
        audio_codec="aac",
        bitrate=256,
    )
    defaults.update(overrides)
    return JellyfinSession(**defaults)


class TestPlexGetSessions:
    @pytest.mark.asyncio
    async def test_returns_mapped_sessions(self):
        svc, repo = _make_plex_service()
        repo.get_sessions.return_value = [_plex_session()]

        result = await svc.get_sessions()

        assert len(result.sessions) == 1
        s = result.sessions[0]
        assert s.session_id == "sess1"
        assert s.user_name == "alice"
        assert s.track_title == "Song A"
        assert s.artist_name == "Artist A"
        assert s.cover_url == "/api/v1/plex/thumb//library/metadata/200/thumb"
        assert s.player_state == "playing"
        assert s.progress_ms == 60000
        assert s.duration_ms == 180000

    @pytest.mark.asyncio
    async def test_empty_sessions(self):
        svc, repo = _make_plex_service()
        repo.get_sessions.return_value = []

        result = await svc.get_sessions()

        assert result.sessions == []

    @pytest.mark.asyncio
    async def test_error_returns_empty(self):
        svc, repo = _make_plex_service()
        repo.get_sessions.side_effect = RuntimeError("Connection refused")

        result = await svc.get_sessions()

        assert result.sessions == []

    @pytest.mark.asyncio
    async def test_no_cover_when_no_album_thumb(self):
        svc, repo = _make_plex_service()
        repo.get_sessions.return_value = [_plex_session(album_thumb="")]

        result = await svc.get_sessions()

        assert result.sessions[0].cover_url == ""


class TestNavidromeGetNowPlaying:
    @pytest.mark.asyncio
    async def test_returns_mapped_entries(self):
        svc, repo = _make_navidrome_service()
        repo.get_now_playing.return_value = [_navidrome_entry()]

        result = await svc.get_now_playing()

        assert len(result.entries) == 1
        e = result.entries[0]
        assert e.user_name == "bob"
        assert e.track_name == "Song N"
        assert e.artist_name == "Artist N"
        assert e.album_name == "Album N"
        assert e.cover_art_id == "cov1"
        assert e.duration_seconds == 240
        assert e.minutes_ago == 2
        assert e.player_name == "Firefox"

    @pytest.mark.asyncio
    async def test_empty_entries(self):
        svc, repo = _make_navidrome_service()
        repo.get_now_playing.return_value = []

        result = await svc.get_now_playing()

        assert result.entries == []

    @pytest.mark.asyncio
    async def test_error_returns_empty(self):
        svc, repo = _make_navidrome_service()
        repo.get_now_playing.side_effect = RuntimeError("timeout")

        result = await svc.get_now_playing()

        assert result.entries == []

    @pytest.mark.asyncio
    async def test_multiple_entries(self):
        svc, repo = _make_navidrome_service()
        repo.get_now_playing.return_value = [
            _navidrome_entry(username="bob"),
            _navidrome_entry(username="charlie", title="Song X"),
        ]

        result = await svc.get_now_playing()

        assert len(result.entries) == 2
        assert result.entries[0].user_name == "bob"
        assert result.entries[1].user_name == "charlie"


class TestJellyfinGetSessions:
    @pytest.mark.asyncio
    async def test_returns_mapped_sessions(self):
        svc, repo = _make_jellyfin_service()
        repo.get_sessions.return_value = [_jellyfin_session()]

        result = await svc.get_sessions()

        assert len(result.sessions) == 1
        s = result.sessions[0]
        assert s.session_id == "jsess1"
        assert s.user_name == "carol"
        assert s.track_name == "Song J"
        assert s.artist_name == "Artist J"
        assert s.device_name == "Chrome"
        assert s.cover_url == "/api/v1/jellyfin/image/jitem1"
        assert s.is_paused is False
        assert s.play_method == "DirectPlay"

    @pytest.mark.asyncio
    async def test_ticks_to_seconds_conversion(self):
        svc, repo = _make_jellyfin_service()
        repo.get_sessions.return_value = [_jellyfin_session(
            position_ticks=600_000_000,
            runtime_ticks=3_000_000_000,
        )]

        result = await svc.get_sessions()

        s = result.sessions[0]
        assert s.position_seconds == pytest.approx(60.0, rel=1e-3)
        assert s.duration_seconds == pytest.approx(300.0, rel=1e-3)

    @pytest.mark.asyncio
    async def test_empty_sessions(self):
        svc, repo = _make_jellyfin_service()
        repo.get_sessions.return_value = []

        result = await svc.get_sessions()

        assert result.sessions == []

    @pytest.mark.asyncio
    async def test_error_returns_empty(self):
        svc, repo = _make_jellyfin_service()
        repo.get_sessions.side_effect = RuntimeError("conn refused")

        result = await svc.get_sessions()

        assert result.sessions == []

    @pytest.mark.asyncio
    async def test_zero_ticks_returns_zero_seconds(self):
        svc, repo = _make_jellyfin_service()
        repo.get_sessions.return_value = [_jellyfin_session(
            position_ticks=0,
            runtime_ticks=0,
        )]

        result = await svc.get_sessions()

        s = result.sessions[0]
        assert s.position_seconds == 0.0
        assert s.duration_seconds == 0.0


# Live presence registry (NowPlayingService) + external poller

from types import SimpleNamespace  # noqa: E402

from services.now_playing_service import ExternalSession, NowPlayingService  # noqa: E402
from services import now_playing_poller as poller  # noqa: E402
from api.v1.schemas.jellyfin import JellyfinSessionInfo, JellyfinSessionsResponse  # noqa: E402
from api.v1.schemas.navidrome import (  # noqa: E402
    NavidromeNowPlayingEntrySchema,
    NavidromeNowPlayingResponse,
)
from api.v1.schemas.plex import PlexSessionInfo, PlexSessionsResponse  # noqa: E402


class _RecordingSSE:
    def __init__(self):
        self.published: list[tuple[str, str, dict]] = []

    async def publish(self, channel, event, data):
        self.published.append((channel, event, data))


class _FakePrefs:
    def __init__(self, visibility_by_user=None):
        self._vis = visibility_by_user or {}

    async def get(self, user_id):
        return SimpleNamespace(now_playing_visibility=self._vis.get(user_id, "full"))


def _update_kwargs(**overrides):
    base = dict(
        key="u1:web",
        user_id="u1",
        user_name="Alice",
        source="local",
        device_name="Web",
        track_name="Song",
        artist_name="Artist",
        album_name="Album",
        cover_url="/c.jpg",
        is_paused=False,
        progress_ms=1000,
        duration_ms=200000,
    )
    base.update(overrides)
    return base


@pytest.mark.asyncio
async def test_presence_update_publishes_full_entry():
    sse = _RecordingSSE()
    svc = NowPlayingService(sse, _FakePrefs())
    await svc.update(**_update_kwargs())
    assert sse.published
    channel, event, data = sse.published[-1]
    assert channel == "now-playing" and event == "snapshot"
    sessions = data["sessions"]
    assert len(sessions) == 1
    assert sessions[0]["track_name"] == "Song"
    assert sessions[0]["redacted"] is False
    assert sessions[0]["progress_ms"] == 1000


@pytest.mark.asyncio
async def test_presence_track_hidden_redacts_song_but_keeps_progress():
    svc = NowPlayingService(_RecordingSSE(), _FakePrefs({"u1": "track_hidden"}))
    await svc.update(**_update_kwargs(track_name="Secret", artist_name="SArtist", progress_ms=5000))
    snap = svc.snapshot()
    assert len(snap) == 1
    entry = snap[0]
    assert entry.redacted is True
    assert entry.track_name == "" and entry.artist_name == ""
    assert entry.album_name is None and entry.cover_url == ""
    # identity + progress survive redaction
    assert entry.user_name == "Alice"
    assert entry.progress_ms == 5000 and entry.duration_ms == 200000


@pytest.mark.asyncio
async def test_presence_offline_hides_entry_entirely():
    svc = NowPlayingService(_RecordingSSE(), _FakePrefs({"u1": "offline"}))
    await svc.update(**_update_kwargs())
    assert svc.snapshot() == []


@pytest.mark.asyncio
async def test_presence_remove_drops_entry_and_publishes_empty():
    sse = _RecordingSSE()
    svc = NowPlayingService(sse, _FakePrefs())
    await svc.update(**_update_kwargs())
    await svc.remove("u1:web")
    assert svc.snapshot() == []
    assert sse.published[-1][2]["sessions"] == []


@pytest.mark.asyncio
async def test_presence_reconcile_external_replaces_and_noops_when_empty():
    sse = _RecordingSSE()
    svc = NowPlayingService(sse, _FakePrefs())
    # empty + nothing existing -> no publish (idle integration doesn't churn)
    await svc.reconcile_source("jellyfin", [])
    assert sse.published == []
    session = ExternalSession(
        key="jellyfin:s1",
        user_name="Bob",
        device_name="TV",
        track_name="T",
        artist_name="A",
        album_name=None,
        cover_url="",
        is_paused=False,
        progress_ms=0,
        duration_ms=1000,
    )
    await svc.reconcile_source("jellyfin", [session])
    assert len(svc.snapshot()) == 1
    # external sessions carry no user_id, so they're never redacted
    assert svc.snapshot()[0].redacted is False
    await svc.reconcile_source("jellyfin", [])
    assert svc.snapshot() == []


@pytest.mark.asyncio
async def test_presence_sweep_drops_stale_sessions():
    import asyncio

    svc = NowPlayingService(_RecordingSSE(), _FakePrefs(), ttl_seconds=0.0)
    await svc.update(**_update_kwargs())
    await asyncio.sleep(0.01)
    await svc.sweep()
    assert svc.snapshot() == []


@pytest.mark.asyncio
async def test_presence_set_visibility_changes_projection_live():
    svc = NowPlayingService(_RecordingSSE(), _FakePrefs())
    await svc.update(**_update_kwargs())
    assert svc.snapshot()[0].track_name == "Song"
    await svc.set_visibility("u1", "track_hidden")
    assert svc.snapshot()[0].redacted is True
    assert svc.snapshot()[0].track_name == ""
    await svc.set_visibility("u1", "offline")
    assert svc.snapshot() == []


def test_poller_map_jellyfin_skips_empty_and_converts_units():
    resp = JellyfinSessionsResponse(
        sessions=[
            JellyfinSessionInfo(
                session_id="s1",
                user_name="Al",
                device_name="TV",
                track_name="T",
                artist_name="A",
                album_name="Alb",
                cover_url="/c",
                is_paused=True,
                position_seconds=12.5,
                duration_seconds=200.0,
            ),
            JellyfinSessionInfo(session_id="s2", track_name=""),
        ]
    )
    out = poller.map_jellyfin(resp)
    assert len(out) == 1
    assert out[0].key == "jellyfin:s1"
    assert out[0].progress_ms == 12500 and out[0].duration_ms == 200000
    assert out[0].is_paused is True


def test_poller_map_navidrome_builds_cover_and_progress():
    resp = NavidromeNowPlayingResponse(
        entries=[
            NavidromeNowPlayingEntrySchema(
                user_name="Al",
                player_name="P",
                album_id="alb",
                track_name="T",
                artist_name="A",
                album_name="Alb",
                cover_art_id="cov",
                duration_seconds=300,
                estimated_position_seconds=10.0,
                minutes_ago=0,
            )
        ]
    )
    out = poller.map_navidrome(resp)
    assert out[0].cover_url == "/api/v1/navidrome/cover/cov"
    assert out[0].progress_ms == 10000
    assert out[0].is_paused is False


def test_poller_map_plex_reads_paused_state():
    resp = PlexSessionsResponse(
        sessions=[
            PlexSessionInfo(
                session_id="p1",
                user_name="Al",
                track_title="T",
                artist_name="A",
                album_name="Alb",
                cover_url="/c",
                player_device="Dev",
                player_state="paused",
                progress_ms=5000,
                duration_ms=10000,
            )
        ]
    )
    out = poller.map_plex(resp)
    assert out[0].key == "plex:p1" and out[0].track_name == "T"
    assert out[0].is_paused is True


@pytest.mark.asyncio
async def test_poller_gates_each_source_on_integration_status():
    from unittest.mock import AsyncMock, MagicMock

    now_playing = AsyncMock()
    home = MagicMock()
    home.get_integration_status.return_value = SimpleNamespace(
        jellyfin=True, navidrome=False, plex=False
    )
    jelly = MagicMock()
    jelly.get_sessions = AsyncMock(return_value=JellyfinSessionsResponse(sessions=[]))
    nav = MagicMock()
    nav.get_now_playing = AsyncMock()
    plex = MagicMock()
    plex.get_sessions = AsyncMock()

    await poller.poll_external_once(now_playing, home, jelly, nav, plex)

    jelly.get_sessions.assert_awaited_once()
    # disabled sources are reconciled to empty without an upstream fetch
    nav.get_now_playing.assert_not_awaited()
    plex.get_sessions.assert_not_awaited()
    assert now_playing.reconcile_source.await_count == 3


class _FailingPrefs:
    async def get(self, user_id):
        raise RuntimeError("prefs DB unavailable")


@pytest.mark.asyncio
async def test_presence_fails_closed_when_visibility_load_errors():
    # a transient prefs-DB error must not leak a hidden track: project conservatively
    svc = NowPlayingService(_RecordingSSE(), _FailingPrefs())
    await svc.update(**_update_kwargs(track_name="Secret", artist_name="SecretArtist"))
    snap = svc.snapshot()
    assert len(snap) == 1
    assert snap[0].redacted is True
    assert snap[0].track_name == "" and snap[0].artist_name == ""
    # presence + progress still surface; only the track is withheld
    assert snap[0].user_name == "Alice"
    assert snap[0].progress_ms == 1000
