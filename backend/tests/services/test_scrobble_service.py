import time
from datetime import datetime, timezone

import pytest
from unittest.mock import AsyncMock, MagicMock

from api.v1.schemas.scrobble import NowPlayingRequest, ScrobbleRequest
from infrastructure.persistence.user_listening_prefs_store import UserListeningPrefsRecord
from services.navidrome_playback_service import NavidromePlaybackService
from services.scrobble_service import ScrobbleService


def _prefs(
    scrobble_lastfm: bool = True,
    scrobble_lb: bool = True,
    navidrome_handles_external: bool = False,
) -> UserListeningPrefsRecord:
    return UserListeningPrefsRecord(
        user_id="u",
        scrobble_to_lastfm=scrobble_lastfm,
        scrobble_to_listenbrainz=scrobble_lb,
        navidrome_handles_external_scrobbles=navidrome_handles_external,
        primary_music_source="listenbrainz",
        now_playing_visibility="full",
        auto_request_personal_mix=False,
        updated_at="",
    )


def _make_service(
    lastfm_linked: bool = True,
    lb_linked: bool = True,
    scrobble_lastfm: bool = True,
    scrobble_lb: bool = True,
    navidrome_handles_external: bool = False,
):
    lastfm_repo = AsyncMock()
    lb_repo = AsyncMock()
    factory = AsyncMock()
    factory.resolve_lastfm.return_value = lastfm_repo if lastfm_linked else None
    factory.resolve_listenbrainz.return_value = lb_repo if lb_linked else None
    prefs_store = AsyncMock()
    prefs_store.get.return_value = _prefs(
        scrobble_lastfm, scrobble_lb, navidrome_handles_external
    )
    history_store = AsyncMock()
    service = ScrobbleService(factory, prefs_store, history_store)
    return service, lastfm_repo, lb_repo, factory, prefs_store, history_store


def _now_playing_req(**overrides) -> NowPlayingRequest:
    defaults = dict(track_name="Song", artist_name="Artist", album_name="Album", duration_ms=200_000)
    defaults.update(overrides)
    return NowPlayingRequest(**defaults)


def _scrobble_req(**overrides) -> ScrobbleRequest:
    defaults = dict(
        track_name="Song",
        artist_name="Artist",
        album_name="Album",
        timestamp=int(time.time()) - 60,
        duration_ms=200_000,
    )
    defaults.update(overrides)
    return ScrobbleRequest(**defaults)


class TestReportNowPlaying:
    @pytest.mark.asyncio
    async def test_dispatches_to_both_services(self):
        service, lastfm, lb, *_ = _make_service()
        result = await service.report_now_playing(_now_playing_req(), user_id="u")
        assert result.accepted is True
        assert "lastfm" in result.services
        assert "listenbrainz" in result.services
        lastfm.update_now_playing.assert_awaited_once()
        lb.submit_now_playing.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_dispatches_only_to_lastfm(self):
        service, lastfm, lb, *_ = _make_service(scrobble_lb=False)
        result = await service.report_now_playing(_now_playing_req(), user_id="u")
        assert result.accepted is True
        assert "lastfm" in result.services
        assert "listenbrainz" not in result.services
        lb.submit_now_playing.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_no_services_when_toggles_off(self):
        service, *_ = _make_service(scrobble_lastfm=False, scrobble_lb=False)
        result = await service.report_now_playing(_now_playing_req(), user_id="u")
        assert result.accepted is False
        assert result.services == {}

    @pytest.mark.asyncio
    async def test_no_services_when_unlinked(self):
        service, lastfm, lb, *_ = _make_service(lastfm_linked=False, lb_linked=False)
        result = await service.report_now_playing(_now_playing_req(), user_id="u")
        assert result.accepted is False
        assert result.services == {}
        lastfm.update_now_playing.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_lastfm_failure_isolated(self):
        service, lastfm, lb, *_ = _make_service()
        lastfm.update_now_playing.side_effect = RuntimeError("API down")
        result = await service.report_now_playing(_now_playing_req(), user_id="u")
        assert result.accepted is True
        assert result.services["lastfm"].success is False
        assert result.services["listenbrainz"].success is True

    @pytest.mark.asyncio
    async def test_now_playing_writes_no_history(self):
        service, _, _, _, _, history = _make_service()
        await service.report_now_playing(_now_playing_req(), user_id="u")
        history.insert.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_navidrome_owned_now_playing_is_not_forwarded(self):
        service, lastfm, lb, *_ = _make_service(navidrome_handles_external=True)
        result = await service.report_now_playing(
            _now_playing_req(source="navidrome"), user_id="u"
        )
        assert result.accepted is True
        assert result.services == {}
        lastfm.update_now_playing.assert_not_awaited()
        lb.submit_now_playing.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_navidrome_now_playing_forwards_when_droppedneedle_is_owner(self):
        service, lastfm, lb, *_ = _make_service(navidrome_handles_external=False)
        await service.report_now_playing(
            _now_playing_req(source="navidrome"), user_id="u"
        )
        lastfm.update_now_playing.assert_awaited_once()
        lb.submit_now_playing.assert_awaited_once()


class TestSubmitScrobble:
    @pytest.mark.asyncio
    async def test_linked_forwards_and_records_history(self):
        service, lastfm, lb, _, _, history = _make_service()
        result = await service.submit_scrobble(_scrobble_req(), user_id="u")
        assert result.accepted is True
        lastfm.scrobble.assert_awaited_once()
        lb.submit_single_listen.assert_awaited_once()
        history.insert.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_unlinked_records_history_but_no_forward(self):
        # other user with no linked account (D1/D6)
        # no linked account (D1/D6)
        service, lastfm, lb, _, _, history = _make_service(lastfm_linked=False, lb_linked=False)
        result = await service.submit_scrobble(_scrobble_req(), user_id="other")
        assert result.accepted is True
        assert result.services == {}
        lastfm.scrobble.assert_not_awaited()
        lb.submit_single_listen.assert_not_awaited()
        history.insert.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_short_track_records_history_but_no_forward(self):
        service, lastfm, lb, _, _, history = _make_service()
        result = await service.submit_scrobble(_scrobble_req(duration_ms=15_000), user_id="u")
        assert result.accepted is True
        assert result.services == {}
        lastfm.scrobble.assert_not_awaited()
        history.insert.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_zero_duration_not_skipped(self):
        service, lastfm, _, _, _, history = _make_service()
        result = await service.submit_scrobble(_scrobble_req(duration_ms=0), user_id="u")
        assert result.accepted is True
        lastfm.scrobble.assert_awaited_once()
        history.insert.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_history_insert_maps_fields(self):
        service, _, _, _, _, history = _make_service()
        ts = int(time.time()) - 60
        await service.submit_scrobble(
            _scrobble_req(timestamp=ts, mbid="rec-1", duration_ms=200_000), user_id="u"
        )
        call = history.insert.await_args
        assert call.args[0] == "u"
        assert call.kwargs["track_name"] == "Song"
        assert call.kwargs["artist_name"] == "Artist"
        assert call.kwargs["album_name"] == "Album"
        assert call.kwargs["recording_mbid"] == "rec-1"
        assert call.kwargs["duration_ms"] == 200_000
        assert call.kwargs["source"] is None
        expected = datetime.fromtimestamp(ts, tz=timezone.utc).isoformat()
        assert call.kwargs["played_at"] == expected

    @pytest.mark.asyncio
    async def test_dedup_blocks_second_submit_same_user(self):
        service, lastfm, _, _, _, history = _make_service()
        ts = int(time.time()) - 60
        await service.submit_scrobble(_scrobble_req(timestamp=ts), user_id="u")
        result2 = await service.submit_scrobble(_scrobble_req(timestamp=ts), user_id="u")
        assert result2.accepted is True
        assert result2.services == {}
        assert lastfm.scrobble.await_count == 1
        assert history.insert.await_count == 1

    @pytest.mark.asyncio
    async def test_dedup_is_per_user(self):
        service, lastfm, _, _, _, history = _make_service()
        ts = int(time.time()) - 60
        await service.submit_scrobble(_scrobble_req(timestamp=ts), user_id="user-a")
        await service.submit_scrobble(_scrobble_req(timestamp=ts), user_id="user-b")
        assert lastfm.scrobble.await_count == 2
        assert history.insert.await_count == 2

    @pytest.mark.asyncio
    async def test_failure_isolation_still_accepted(self):
        service, lastfm, _, _, _, history = _make_service()
        lastfm.scrobble.side_effect = RuntimeError("network")
        result = await service.submit_scrobble(_scrobble_req(), user_id="u")
        assert result.accepted is True  # play recorded locally even when forward fails
        assert result.services["lastfm"].success is False
        assert result.services["listenbrainz"].success is True
        history.insert.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_toggle_off_lastfm_not_forwarded(self):
        service, lastfm, _, _, _, _ = _make_service(scrobble_lastfm=False)
        result = await service.submit_scrobble(_scrobble_req(), user_id="u")
        assert "lastfm" not in result.services
        lastfm.scrobble.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_navidrome_owned_scrobble_records_history_without_forwarding(self):
        service, lastfm, lb, _, _, history = _make_service(
            navidrome_handles_external=True
        )
        result = await service.submit_scrobble(
            _scrobble_req(source="navidrome"), user_id="u"
        )
        assert result.accepted is True
        assert result.services == {}
        history.insert.assert_awaited_once()
        assert history.insert.await_args.kwargs["source"] == "navidrome"
        lastfm.scrobble.assert_not_awaited()
        lb.submit_single_listen.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_navidrome_owned_path_keeps_server_scrobble_without_duplicate_forwarding(self):
        navidrome_repo = MagicMock()
        navidrome_repo.scrobble = AsyncMock(return_value=True)
        navidrome = NavidromePlaybackService(navidrome_repo)
        service, lastfm, lb, _, _, history = _make_service(
            navidrome_handles_external=True
        )

        navidrome_accepted = await navidrome.scrobble("song-1")
        result = await service.submit_scrobble(
            _scrobble_req(source="navidrome"), user_id="u"
        )

        assert navidrome_accepted is True
        assert result.accepted is True
        navidrome_repo.scrobble.assert_awaited_once()
        history.insert.assert_awaited_once()
        lastfm.scrobble.assert_not_awaited()
        lb.submit_single_listen.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_navidrome_still_forwards_when_droppedneedle_is_owner(self):
        service, lastfm, lb, *_ = _make_service(navidrome_handles_external=False)
        await service.submit_scrobble(_scrobble_req(source="navidrome"), user_id="u")
        lastfm.scrobble.assert_awaited_once()
        lb.submit_single_listen.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_navidrome_preference_does_not_suppress_local_plays(self):
        service, lastfm, lb, *_ = _make_service(navidrome_handles_external=True)
        await service.submit_scrobble(_scrobble_req(source="local"), user_id="u")
        lastfm.scrobble.assert_awaited_once()
        lb.submit_single_listen.assert_awaited_once()


class TestTimestampValidation:
    def test_future_timestamp_rejected(self):
        with pytest.raises(ValueError, match="future"):
            _scrobble_req(timestamp=int(time.time()) + 3600)

    def test_old_timestamp_rejected(self):
        with pytest.raises(ValueError, match="14 days"):
            _scrobble_req(timestamp=int(time.time()) - 15 * 86400)

    def test_valid_timestamp_accepted(self):
        req = _scrobble_req(timestamp=int(time.time()) - 60)
        assert req.timestamp > 0


class TestDedupEviction:
    @pytest.mark.asyncio
    async def test_evicts_when_exceeding_max(self):
        service, *_ = _make_service()
        base_ts = int(time.time()) - 86400
        for i in range(205):
            req = _scrobble_req(artist_name=f"artist-{i}", timestamp=base_ts + i)
            await service.submit_scrobble(req, user_id="u")
        assert len(service._dedup_cache) <= 200


class TestPluginDispatch:
    @pytest.mark.asyncio
    async def test_accepted_scrobble_fans_out_to_plugins(self):
        """01b scrobbler capability: an accepted play dispatches to the plugin
        host fire-and-forget, without blocking the response."""
        import asyncio
        from unittest.mock import AsyncMock, MagicMock

        service, *_ = _make_service()
        host = MagicMock()
        host.dispatch_scrobble = AsyncMock()
        service._plugin_host = host

        result = await service.submit_scrobble(_scrobble_req(), user_id="u")
        assert result.accepted is True
        await asyncio.gather(*service._plugin_tasks)

        host.dispatch_scrobble.assert_awaited_once()
        event = host.dispatch_scrobble.await_args.args[0]
        assert event.track == _scrobble_req().track_name

    @pytest.mark.asyncio
    async def test_short_tracks_do_not_reach_plugins(self):
        import asyncio
        from unittest.mock import AsyncMock, MagicMock

        service, *_ = _make_service()
        host = MagicMock()
        host.dispatch_scrobble = AsyncMock()
        service._plugin_host = host

        await service.submit_scrobble(_scrobble_req(duration_ms=5_000), user_id="u")
        await asyncio.gather(*service._plugin_tasks)

        host.dispatch_scrobble.assert_not_awaited()
