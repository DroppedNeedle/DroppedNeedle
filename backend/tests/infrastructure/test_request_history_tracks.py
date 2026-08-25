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
