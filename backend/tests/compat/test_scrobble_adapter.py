"""T0.8 - compat scrobble adapter + Q19 schema passthrough."""

import sqlite3
import threading
import time
from datetime import datetime, timezone
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from api.v1.schemas.scrobble import ScrobbleRequest, ScrobbleResponse
from core.exceptions import ResourceNotFoundError
from infrastructure.persistence.play_history_store import PlayHistoryStore
from services.compat.compat_scrobble_adapter import CompatScrobbleAdapter
from services.scrobble_service import ScrobbleService

pytestmark = pytest.mark.asyncio


@pytest.fixture
def play_history_store(db_path: Path, write_lock: threading.Lock) -> PlayHistoryStore:
    return PlayHistoryStore(db_path=db_path, write_lock=write_lock)


@pytest.fixture
def scrobble_service(play_history_store: PlayHistoryStore) -> ScrobbleService:
    prefs = AsyncMock()
    prefs.get = AsyncMock(
        return_value=SimpleNamespace(
            scrobble_to_lastfm=False, scrobble_to_listenbrainz=False
        )
    )
    return ScrobbleService(
        client_factory=AsyncMock(),
        listening_prefs_store=prefs,
        play_history_store=play_history_store,
    )


@pytest.fixture
def adapter(scrobble_service, library_view_service) -> CompatScrobbleAdapter:
    return CompatScrobbleAdapter(scrobble_service, library_view_service)


def _play_rows(db_path: Path) -> list[sqlite3.Row]:
    conn = sqlite3.connect(db_path)
    try:
        conn.row_factory = sqlite3.Row
        return conn.execute(
            "SELECT * FROM play_history ORDER BY played_at"
        ).fetchall()
    finally:
        conn.close()


async def test_scrobble_records_play_with_source_and_rg(
    adapter, seeded_library, db_path
):
    _db, _lm, ids = seeded_library
    resp = await adapter.scrobble(
        ids["tracks"][0], user_id="user-alice", client="Symfonium"
    )
    assert resp.accepted is True
    rows = _play_rows(db_path)
    assert len(rows) == 1
    assert rows[0]["source"] == "symfonium"             # lowercased client
    assert rows[0]["release_group_mbid"] == ids["rg"]   # matches the track
    assert rows[0]["track_name"] == "Airbag"
    assert rows[0]["user_id"] == "user-alice"


async def test_now_playing_does_not_record_a_play(adapter, seeded_library, db_path):
    _db, _lm, ids = seeded_library
    resp = await adapter.now_playing(
        ids["tracks"][0], user_id="user-alice", client="Finamp"
    )
    # now-playing path returns a response (accepted depends on linked services)...
    assert isinstance(resp, ScrobbleResponse)
    # ...but is NOT a play - nothing written to play_history
    assert _play_rows(db_path) == []


async def test_scrobble_uses_provided_timestamp(adapter, seeded_library, db_path):
    _db, _lm, ids = seeded_library
    ts = int(time.time()) - 3600  # an hour ago (Jellyfin start-time path, within window)
    await adapter.scrobble(
        ids["tracks"][0], user_id="user-alice", client="finamp", played_at=ts
    )
    rows = _play_rows(db_path)
    expected = datetime.fromtimestamp(ts, tz=timezone.utc).isoformat()
    assert rows[0]["played_at"] == expected


async def test_scrobble_unknown_track_raises(adapter):
    with pytest.raises(ResourceNotFoundError):
        await adapter.scrobble("missing-file", user_id="user-alice", client="x")


async def test_native_flow_unaffected_records_with_null_source(
    scrobble_service, db_path, auth_store
):
    # a native ScrobbleRequest leaves source/release_group_mbid unset (None)
    req = ScrobbleRequest(
        track_name="Native Track", artist_name="Native Artist",
        timestamp=int(time.time()) - 3600, album_name="Native Album",
        duration_ms=200_000,
    )
    await scrobble_service.submit_scrobble(req, user_id="user-alice")
    rows = _play_rows(db_path)
    assert len(rows) == 1
    assert rows[0]["source"] is None
    assert rows[0]["release_group_mbid"] is None
    assert rows[0]["track_name"] == "Native Track"


async def test_q21_start_tracking_roundtrip(adapter):
    adapter.mark_started("user-alice", "item-1")
    assert adapter.pop_started("user-alice", "item-1") is not None
    # popped -> gone
    assert adapter.pop_started("user-alice", "item-1") is None


async def test_now_playing_writes_presence(
    scrobble_service, library_view_service, seeded_library
):
    _db, _lm, ids = seeded_library
    presence = AsyncMock()
    adapter = CompatScrobbleAdapter(scrobble_service, library_view_service, presence)
    await adapter.now_playing(
        ids["tracks"][0], user_id="user-alice", client="Finamp", user_name="Alice"
    )
    presence.update.assert_awaited_once()
    kw = presence.update.await_args.kwargs
    assert kw["key"] == "user-alice:compat:finamp"
    assert kw["user_id"] == "user-alice" and kw["user_name"] == "Alice"
    assert kw["source"] == "finamp"
    assert kw["track_name"] == "Airbag"
    assert kw["is_paused"] is False


async def test_scrobble_clears_presence(
    scrobble_service, library_view_service, seeded_library
):
    _db, _lm, ids = seeded_library
    presence = AsyncMock()
    adapter = CompatScrobbleAdapter(scrobble_service, library_view_service, presence)
    await adapter.scrobble(ids["tracks"][0], user_id="user-alice", client="Symfonium")
    presence.remove.assert_awaited_once_with("user-alice:compat:symfonium")


async def test_clear_presence_removes_compat_key(scrobble_service, library_view_service):
    presence = AsyncMock()
    adapter = CompatScrobbleAdapter(scrobble_service, library_view_service, presence)
    await adapter.clear_presence("user-alice", "Finamp")
    presence.remove.assert_awaited_once_with("user-alice:compat:finamp")


async def test_progress_updates_presence_position(
    scrobble_service, library_view_service, seeded_library
):
    _db, _lm, ids = seeded_library
    presence = AsyncMock()
    adapter = CompatScrobbleAdapter(scrobble_service, library_view_service, presence)
    await adapter.progress(
        ids["tracks"][0],
        user_id="user-alice",
        user_name="Alice",
        client="Finamp",
        position_ms=12345,
        is_paused=True,
    )
    presence.update.assert_awaited_once()
    kw = presence.update.await_args.kwargs
    assert kw["progress_ms"] == 12345 and kw["is_paused"] is True
