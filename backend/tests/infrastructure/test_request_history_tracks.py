import sqlite3

import pytest

from infrastructure.persistence.request_history import RequestHistoryStore


@pytest.mark.asyncio
async def test_exact_track_metadata_survives_request_history_round_trip(tmp_path):
    store = RequestHistoryStore(tmp_path / "droppedneedle.db")

    await store.async_record_request(
        musicbrainz_id="recording-1",
        artist_name="Radiohead",
        album_title="OK Computer",
        artist_mbid="artist-1",
        user_id="listener-1",
        requested_by_name="Listener",
        release_mbid="release-1",
        initial_status="awaiting_approval",
        request_kind="track",
        track_title="Airbag",
        duration_seconds=287,
        track_release_group_mbid="release-group-1",
    )

    record = await store.async_get_record("RECORDING-1")

    assert record is not None
    assert record.request_kind == "track"
    assert record.track_title == "Airbag"
    assert record.duration_seconds == 287
    assert record.track_release_group_mbid == "release-group-1"


@pytest.mark.asyncio
async def test_legacy_album_request_defaults_to_album_kind(tmp_path):
    store = RequestHistoryStore(tmp_path / "droppedneedle.db")

    await store.async_record_request("release-group-1", "Radiohead", "OK Computer")

    record = await store.async_get_record("release-group-1")

    assert record is not None
    assert record.request_kind == "album"
    assert record.track_title is None


@pytest.mark.asyncio
async def test_shared_request_remains_visible_and_private_for_each_listener(tmp_path):
    store = RequestHistoryStore(tmp_path / "droppedneedle.db")
    await store.async_record_request(
        "release-group-1",
        "Artist",
        "Album",
        user_id="listener-1",
        requested_by_name="First listener",
    )
    await store.async_add_requester("release-group-1", "listener-2", "Second listener")

    first = await store.async_get_active_requests_for_user("listener-1")
    second = await store.async_get_active_requests_for_user("listener-2")
    assert [record.user_id for record in first] == ["listener-1"]
    assert [record.user_id for record in second] == ["listener-2"]
    assert [record.requested_by_name for record in second] == ["Second listener"]
    second_history, total = await store.async_get_history_for_user("listener-2")
    assert total == 1
    assert [record.user_id for record in second_history] == ["listener-2"]

    assert await store.async_requester_count("release-group-1") == 2
    assert await store.async_remove_requester("listener-1", "release-group-1")
    canonical = await store.async_get_record("release-group-1")
    assert canonical is not None
    assert canonical.user_id == "listener-2"


@pytest.mark.asyncio
async def test_existing_user_attribution_is_backfilled_during_upgrade(tmp_path):
    path = tmp_path / "droppedneedle.db"
    store = RequestHistoryStore(path)
    await store.async_record_request(
        "release-group-1",
        "Artist",
        "Album",
        user_id="legacy-listener",
        requested_by_name="Legacy listener",
    )
    with sqlite3.connect(path) as connection:
        connection.execute("DROP TABLE request_history_requesters")

    upgraded = RequestHistoryStore(path)
    active = await upgraded.async_get_active_requests_for_user("legacy-listener")
    assert [record.musicbrainz_id for record in active] == ["release-group-1"]
