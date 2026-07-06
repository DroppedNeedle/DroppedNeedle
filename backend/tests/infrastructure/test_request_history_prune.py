"""prune_old_terminal_requests × wanted watches (Wanted plan §4.4): a terminal
request past retention survives while its mbid has a live (watching/dormant)
watch - pruning it would orphan the watch and break the status-flip linkage -
and the guard degrades cleanly on a DB without the wanted_watches table."""

import sqlite3
import threading
import time
from datetime import datetime, timezone
from pathlib import Path

import pytest

from infrastructure.persistence.request_history import RequestHistoryStore
from infrastructure.persistence.wanted_store import WantedStore


def _old_iso(days: int = 300) -> str:
    return datetime.fromtimestamp(
        time.time() - days * 86400, tz=timezone.utc
    ).isoformat()


async def _seed_terminal_request(store: RequestHistoryStore, mbid: str) -> None:
    await store.async_record_request(mbid, "Artist", "Album", user_id="user-a")
    # push it terminal + old with raw sqlite (requested_at drives the age check)
    conn = sqlite3.connect(store.db_path)
    conn.execute(
        "UPDATE request_history SET status = 'failed', requested_at = ?,"
        " completed_at = ? WHERE musicbrainz_id_lower = ?",
        (_old_iso(), _old_iso(), mbid.lower()),
    )
    conn.commit()
    conn.close()


@pytest.mark.asyncio
async def test_watched_terminal_request_survives_prune_unwatched_twin_dies(tmp_path: Path):
    lock = threading.Lock()
    db_path = tmp_path / "library.db"
    requests = RequestHistoryStore(db_path=db_path, write_lock=lock)
    wanted = WantedStore(db_path=db_path, write_lock=lock)

    await _seed_terminal_request(requests, "rg-watched")
    await _seed_terminal_request(requests, "rg-dormant")
    await _seed_terminal_request(requests, "rg-stopped")
    await _seed_terminal_request(requests, "rg-unwatched")

    for mbid in ("rg-watched", "rg-dormant", "rg-stopped"):
        await wanted.create_watch(
            release_group_mbid=mbid, user_id="user-a", artist_name="Artist",
            album_title="Album", kind="missing", next_check_at=time.time(),
        )
    await wanted.record_cycle(
        "rg-dormant", outcome="no_results", next_check_at=time.time(), quiet=True,
        go_dormant=True,
    )
    await wanted.stop_watch("rg-stopped")

    pruned = await requests.prune_old_terminal_requests(180)

    # live watches (watching + dormant) protect their rows; stopped does not
    assert pruned == 2
    assert await requests.async_get_record("rg-watched") is not None
    assert await requests.async_get_record("rg-dormant") is not None
    assert await requests.async_get_record("rg-stopped") is None
    assert await requests.async_get_record("rg-unwatched") is None


@pytest.mark.asyncio
async def test_prune_still_works_without_the_wanted_table(tmp_path: Path):
    requests = RequestHistoryStore(
        db_path=tmp_path / "solo.db", write_lock=threading.Lock()
    )
    await _seed_terminal_request(requests, "rg-old")
    assert await requests.prune_old_terminal_requests(180) == 1
    assert await requests.async_get_record("rg-old") is None
