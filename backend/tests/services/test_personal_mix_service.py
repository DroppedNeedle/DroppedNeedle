"""PersonalMixService: recommendation-playlist ingestion, similar-artist
expansion, playlist upsert, and opt-in auto-request."""

from datetime import datetime, timedelta, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from core.exceptions import ConfigurationError
from services.native.download_service import ALREADY_IN_LIBRARY
from services.personal_mix_service import PersonalMixService

RG1 = "rg-1111-1111-1111-111111111111"
RG2 = "rg-2222-2222-2222-222222222222"
ARTIST_A = "artist-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
ARTIST_B = "artist-bbbb-bbbb-bbbb-bbbbbbbbbbbb"


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _stale_iso() -> str:
    return (datetime.now(timezone.utc) - timedelta(days=10)).isoformat()


def _mix_track(title, creator, album, caa_release_mbid, artist_mbid, recording_mbid):
    return SimpleNamespace(
        title=title, creator=creator, album=album,
        caa_release_mbid=caa_release_mbid, recording_mbid=recording_mbid,
        artist_mbids=[artist_mbid] if artist_mbid else None,
    )


def _lb_playlist(tracks):
    return SimpleNamespace(tracks=tracks)


def _playlist_record(id="mix-1", updated_at="2025-01-01T00:00:00+00:00", user_id="user-a"):
    return SimpleNamespace(id=id, updated_at=updated_at, user_id=user_id)


def _track_record(id):
    return SimpleNamespace(id=id)


@pytest.fixture
def svc():
    client_factory = AsyncMock()
    client_factory.resolve_listenbrainz_username = AsyncMock(return_value="alice")

    mb_repo = AsyncMock()
    mb_repo.get_release_group_id_from_release = AsyncMock(side_effect=lambda rid: rid.replace("caa-", "rg-"))

    library_repo = AsyncMock()
    library_repo.get_library_mbids = AsyncMock(return_value=set())

    playlist_service = AsyncMock()
    playlist_service.get_by_source_ref = AsyncMock(return_value=None)
    playlist_service.create_playlist = AsyncMock(
        return_value=SimpleNamespace(id="mix-1")
    )
    playlist_service.get_tracks = AsyncMock(return_value=[])
    playlist_service.remove_tracks = AsyncMock(return_value=0)
    playlist_service.add_tracks = AsyncMock(return_value=[])

    download_service = AsyncMock()
    download_service.request_album = AsyncMock(return_value="task-1")

    listening_prefs_store = AsyncMock()
    listening_prefs_store.get = AsyncMock(
        return_value=SimpleNamespace(auto_request_personal_mix=False)
    )

    connections_store = AsyncMock()
    connections_store.list_user_ids_for_service = AsyncMock(return_value=[])

    auth_store = AsyncMock()
    auth_store.get_user_by_id = AsyncMock(return_value=SimpleNamespace(id="user-a"))

    lb_repo = AsyncMock()
    lb_repo.get_recommendation_playlists = AsyncMock(return_value=[])

    client_factory.resolve_listenbrainz = AsyncMock(return_value=lb_repo)

    service = PersonalMixService(
        client_factory=client_factory,
        mb_repo=mb_repo,
        library_repo=library_repo,
        playlist_service=playlist_service,
        download_service=download_service,
        listening_prefs_store=listening_prefs_store,
        connections_store=connections_store,
        auth_store=auth_store,
    )
    return SimpleNamespace(
        service=service, client_factory=client_factory, mb_repo=mb_repo,
        library_repo=library_repo, playlist_service=playlist_service,
        download_service=download_service, listening_prefs_store=listening_prefs_store,
        connections_store=connections_store, auth_store=auth_store, lb_repo=lb_repo,
    )


def _set_recommendation_tracks(svc, *, jams=None, exploration=None):
    playlists = []
    tracks_by_id = {}
    if jams is not None:
        playlists.append({"source_patch": "weekly-jams", "playlist_id": "pl-jams"})
        tracks_by_id["pl-jams"] = jams
    if exploration is not None:
        playlists.append({"source_patch": "weekly-exploration", "playlist_id": "pl-expl"})
        tracks_by_id["pl-expl"] = exploration
    svc.lb_repo.get_recommendation_playlists.return_value = playlists

    async def _get_playlist_tracks(playlist_id):
        return _lb_playlist(tracks_by_id[playlist_id])

    svc.lb_repo.get_playlist_tracks = AsyncMock(side_effect=_get_playlist_tracks)


@pytest.mark.asyncio
async def test_not_linked_is_skipped(svc):
    svc.client_factory.resolve_listenbrainz.return_value = None
    result = await svc.service.build_for_user("user-a")
    assert result.skipped is True
    assert result.reason == "listenbrainz_not_linked"
    svc.playlist_service.get_by_source_ref.assert_not_called()


@pytest.mark.asyncio
async def test_fresh_playlist_is_skipped_without_force(svc):
    svc.playlist_service.get_by_source_ref.return_value = _playlist_record(updated_at=_now_iso())
    result = await svc.service.build_for_user("user-a")
    assert result.skipped is True
    assert result.reason == "fresh"
    svc.lb_repo.get_recommendation_playlists.assert_not_called()


@pytest.mark.asyncio
async def test_force_bypasses_freshness_guard(svc):
    svc.playlist_service.get_by_source_ref.return_value = _playlist_record(updated_at=_now_iso())
    _set_recommendation_tracks(
        svc, jams=[_mix_track("Song", "Artist", "Album", "caa-1", ARTIST_A, "rec-1")],
    )
    result = await svc.service.build_for_user("user-a", force=True)
    assert result.skipped is False
    svc.lb_repo.get_recommendation_playlists.assert_awaited_once()


@pytest.mark.asyncio
async def test_builds_new_playlist_from_recommendation_tracks(svc):
    _set_recommendation_tracks(
        svc,
        jams=[_mix_track("Jam Song", "Artist A", "Album A", "caa-1111-1111-1111-111111111111", ARTIST_A, "rec-1")],
        exploration=[_mix_track("Explore Song", "Artist B", "Album B", "caa-2222-2222-2222-222222222222", ARTIST_B, "rec-2")],
    )
    result = await svc.service.build_for_user("user-a")

    assert result.skipped is False
    assert result.track_count == 2
    svc.playlist_service.create_playlist.assert_awaited_once()
    svc.playlist_service.remove_tracks.assert_not_called()
    add_call = svc.playlist_service.add_tracks.await_args
    playlist_id, owner, track_dicts = add_call.args
    assert playlist_id == "mix-1"
    assert owner.id == "user-a"
    names = {t["track_name"] for t in track_dicts}
    assert names == {"Jam Song", "Explore Song"}
    for t in track_dicts:
        assert t["album_id"] in (RG1, RG2)


@pytest.mark.asyncio
async def test_owned_track_is_marked_in_library_and_not_requested(svc):
    svc.library_repo.get_library_mbids.return_value = {RG1}
    _set_recommendation_tracks(
        svc, jams=[_mix_track("Jam Song", "Artist A", "Album A", "caa-1111-1111-1111-111111111111", ARTIST_A, "rec-1")],
    )
    svc.listening_prefs_store.get.return_value = SimpleNamespace(auto_request_personal_mix=True)

    result = await svc.service.build_for_user("user-a")

    assert result.track_count == 1
    assert result.requested_albums == 0
    svc.download_service.request_album.assert_not_called()


@pytest.mark.asyncio
async def test_owned_track_is_linked_to_its_local_file(svc):
    svc.library_repo.get_library_mbids.return_value = {RG1}
    svc.library_repo.get_tracks = AsyncMock(
        return_value=[
            SimpleNamespace(id="file-1", recording_mbid="rec-1", track_number=3, disc_number=1),
        ]
    )
    _set_recommendation_tracks(
        svc, jams=[_mix_track("Jam Song", "Artist A", "Album A", "caa-1111-1111-1111-111111111111", ARTIST_A, "rec-1")],
    )

    await svc.service.build_for_user("user-a")

    track_dicts = svc.playlist_service.add_tracks.await_args.args[2]
    assert len(track_dicts) == 1
    track = track_dicts[0]
    assert track["source_type"] == "local"
    assert track["library_file_id"] == "file-1"
    assert track["track_source_id"] == "file-1"
    assert track["track_number"] == 3
    assert track["disc_number"] == 1


@pytest.mark.asyncio
async def test_recording_mbid_collision_on_another_owned_album_is_not_cross_matched(svc):
    svc.library_repo.get_library_mbids.return_value = {RG1}
    svc.library_repo.get_tracks = AsyncMock(
        return_value=[
            SimpleNamespace(id="file-1", recording_mbid="rec-1", track_number=1, disc_number=1),
        ]
    )
    _set_recommendation_tracks(
        svc, jams=[_mix_track("Song", "Artist B", "Album B", "caa-2222-2222-2222-222222222222", ARTIST_B, "rec-1")],
    )

    await svc.service.build_for_user("user-a")

    track_dicts = svc.playlist_service.add_tracks.await_args.args[2]
    assert len(track_dicts) == 1
    track = track_dicts[0]
    assert track["album_id"] == RG2
    assert track["source_type"] == ""
    assert track["library_file_id"] is None


@pytest.mark.asyncio
async def test_existing_playlist_is_replaced_not_recreated(svc):
    svc.playlist_service.get_by_source_ref.return_value = _playlist_record(updated_at=_stale_iso())
    svc.playlist_service.get_tracks.return_value = [_track_record("old-1"), _track_record("old-2")]
    _set_recommendation_tracks(
        svc, jams=[_mix_track("New Song", "Artist A", "Album A", "caa-1111-1111-1111-111111111111", ARTIST_A, "rec-1")],
    )

    result = await svc.service.build_for_user("user-a")

    assert result.playlist_id == "mix-1"
    svc.playlist_service.create_playlist.assert_not_called()
    svc.playlist_service.remove_tracks.assert_awaited_once()
    removed_ids = svc.playlist_service.remove_tracks.await_args.args[2]
    assert set(removed_ids) == {"old-1", "old-2"}


@pytest.mark.asyncio
async def test_auto_request_off_by_default(svc):
    _set_recommendation_tracks(
        svc, jams=[_mix_track("Song", "Artist A", "Album A", "caa-1111-1111-1111-111111111111", ARTIST_A, "rec-1")],
    )
    result = await svc.service.build_for_user("user-a")
    assert result.requested_albums == 0
    svc.download_service.request_album.assert_not_called()


@pytest.mark.asyncio
async def test_auto_request_requests_missing_albums_when_opted_in(svc):
    svc.listening_prefs_store.get.return_value = SimpleNamespace(auto_request_personal_mix=True)
    _set_recommendation_tracks(
        svc, jams=[_mix_track("Song", "Artist A", "Album A", "caa-1111-1111-1111-111111111111", ARTIST_A, "rec-1")],
    )
    result = await svc.service.build_for_user("user-a")
    assert result.requested_albums == 1
    svc.download_service.request_album.assert_awaited_once()
    kwargs = svc.download_service.request_album.await_args.kwargs
    assert kwargs["user_id"] == "user-a"
    assert kwargs["release_group_mbid"] == RG1
    assert kwargs["origin"] == "user"


@pytest.mark.asyncio
async def test_already_in_library_sentinel_not_counted(svc):
    svc.listening_prefs_store.get.return_value = SimpleNamespace(auto_request_personal_mix=True)
    svc.download_service.request_album.return_value = ALREADY_IN_LIBRARY
    _set_recommendation_tracks(
        svc, jams=[_mix_track("Song", "Artist A", "Album A", "caa-1111-1111-1111-111111111111", ARTIST_A, "rec-1")],
    )
    result = await svc.service.build_for_user("user-a")
    assert result.requested_albums == 0


@pytest.mark.asyncio
async def test_config_error_stops_auto_request_without_crashing(svc):
    svc.listening_prefs_store.get.return_value = SimpleNamespace(auto_request_personal_mix=True)
    svc.download_service.request_album.side_effect = ConfigurationError("download client disabled")
    _set_recommendation_tracks(
        svc, jams=[_mix_track("Song", "Artist A", "Album A", "caa-1111-1111-1111-111111111111", ARTIST_A, "rec-1")],
    )
    result = await svc.service.build_for_user("user-a")
    assert result.requested_albums == 0


@pytest.mark.asyncio
async def test_no_tracks_at_all_is_skipped(svc):
    result = await svc.service.build_for_user("user-a")
    assert result.skipped is True
    assert result.reason == "no_tracks"
    svc.playlist_service.create_playlist.assert_not_called()


@pytest.mark.asyncio
async def test_run_for_all_users_aggregates_and_isolates_errors(monkeypatch, svc):
    svc.connections_store.list_user_ids_for_service.return_value = ["user-a", "user-b", "user-c"]

    from services.personal_mix_service import PersonalMixResult

    async def fake_build(user_id, *, force=False):
        if user_id == "user-a":
            raise RuntimeError("boom")
        if user_id == "user-b":
            return PersonalMixResult(user_id=user_id, skipped=True, reason="fresh")
        return PersonalMixResult(user_id=user_id, playlist_id="mix-c", track_count=5)

    monkeypatch.setattr(svc.service, "build_for_user", fake_build)

    summary = await svc.service.run_for_all_users()
    assert summary.users_considered == 3
    assert summary.errors == 1
    assert summary.skipped == 1
    assert summary.built == 1
