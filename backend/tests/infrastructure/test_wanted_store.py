"""WantedStore tests - construct-twice idempotency, enrolment insert semantics,
due selection ordering/limit, state transitions (stop/resume/fulfil/re-arm),
seen-set dedup, the one-transaction cycle write, and prune growth control."""

import sqlite3
import threading
import time
from pathlib import Path

import pytest

from infrastructure.persistence.wanted_store import WantedStore


@pytest.fixture
def store(tmp_path: Path) -> WantedStore:
    return WantedStore(db_path=tmp_path / "library.db", write_lock=threading.Lock())


async def _watch(store: WantedStore, mbid: str = "RG-1", **overrides) -> bool:
    fields = {
        "release_group_mbid": mbid,
        "user_id": "user-a",
        "artist_name": "Yan Qing",
        "album_title": "the arrival",
        "kind": "missing",
        "next_check_at": time.time() + 60,
    }
    fields.update(overrides)
    return await store.create_watch(**fields)


def test_construct_twice_is_idempotent(tmp_path: Path):
    db_path = tmp_path / "library.db"
    lock = threading.Lock()
    WantedStore(db_path=db_path, write_lock=lock)
    WantedStore(db_path=db_path, write_lock=lock)  # re-run must not error
    assert db_path.exists()


@pytest.mark.asyncio
async def test_create_watch_and_get_roundtrip(store):
    inserted = await _watch(store, year=2026, artist_mbid="am-1",
                            first_release_date="2026-06-23")
    assert inserted is True
    watch = await store.get_watch("rg-1")  # lookup is case-insensitive
    assert watch is not None
    assert watch.release_group_mbid == "RG-1"
    assert watch.state == "watching"
    assert watch.kind == "missing"
    assert watch.first_release_date == "2026-06-23"
    assert watch.check_count == 0
    assert watch.new_candidate_count == 0


@pytest.mark.asyncio
async def test_create_watch_never_overwrites_existing(store):
    assert await _watch(store) is True
    assert await _watch(store, album_title="other title") is False
    watch = await store.get_watch("rg-1")
    assert watch.album_title == "the arrival"


@pytest.mark.asyncio
async def test_list_due_orders_and_limits(store):
    now = time.time()
    await _watch(store, "rg-late", next_check_at=now - 10)
    await _watch(store, "rg-early", next_check_at=now - 100)
    await _watch(store, "rg-future", next_check_at=now + 1000)
    due = await store.list_due(now, limit=10)
    assert [w.release_group_mbid for w in due] == ["rg-early", "rg-late"]
    capped = await store.list_due(now, limit=1)
    assert [w.release_group_mbid for w in capped] == ["rg-early"]


@pytest.mark.asyncio
async def test_list_due_skips_non_watching_states(store):
    now = time.time()
    await _watch(store, "rg-stopped", next_check_at=now - 10)
    await _watch(store, "rg-fulfilled", next_check_at=now - 10)
    await store.stop_watch("rg-stopped")
    await store.mark_fulfilled("rg-fulfilled", "satisfied")
    due = await store.list_due(now, limit=10)
    assert due == []


@pytest.mark.asyncio
async def test_stop_and_resume_transitions(store):
    await _watch(store)
    assert await store.stop_watch("rg-1") is True
    assert (await store.get_watch("rg-1")).state == "stopped"
    assert await store.stop_watch("rg-1") is False  # already stopped

    resumed_at = time.time()
    assert await store.resume_watch("rg-1", now=resumed_at) is True
    watch = await store.get_watch("rg-1")
    assert watch.state == "watching"
    assert watch.next_check_at == pytest.approx(resumed_at)
    # resume re-anchors the dormancy clock (plan §8.2)
    assert watch.created_at == pytest.approx(resumed_at)


@pytest.mark.asyncio
async def test_resume_does_not_touch_fulfilled(store):
    await _watch(store)
    await store.mark_fulfilled("rg-1", "satisfied")
    assert await store.resume_watch("rg-1") is False
    assert (await store.get_watch("rg-1")).state == "fulfilled"


@pytest.mark.asyncio
async def test_rearm_only_from_fulfilled(store):
    await _watch(store)
    # watching: never re-armed
    assert await store.rearm_watch(
        "rg-1", user_id="user-b", kind="partial", next_check_at=time.time()
    ) is False

    await store.record_cycle(
        "rg-1", outcome="new_manual", next_check_at=time.time(), quiet=False,
        new_candidate_count=3, seen=[("soulseek", "peer\x1fdir")],
    )
    await store.mark_fulfilled("rg-1", "satisfied")
    rearm_at = time.time()
    assert await store.rearm_watch(
        "rg-1", user_id="user-b", kind="partial", next_check_at=rearm_at + 60, now=rearm_at
    ) is True
    watch = await store.get_watch("rg-1")
    assert watch.state == "watching"
    assert watch.user_id == "user-b"
    assert watch.kind == "partial"
    assert watch.check_count == 0
    assert watch.quiet_streak == 0
    assert watch.last_outcome is None
    assert watch.new_candidate_count == 0
    assert watch.created_at == pytest.approx(rearm_at)


@pytest.mark.asyncio
async def test_record_cycle_updates_counters_and_streak(store):
    await _watch(store)
    now = time.time()
    await store.record_cycle(
        "rg-1", outcome="no_results", next_check_at=now + 100, quiet=True, now=now
    )
    await store.record_cycle(
        "rg-1", outcome="seen_only", next_check_at=now + 200, quiet=True, now=now
    )
    watch = await store.get_watch("rg-1")
    assert watch.check_count == 2
    assert watch.quiet_streak == 2
    assert watch.last_outcome == "seen_only"
    assert watch.next_check_at == pytest.approx(now + 200)

    # a loud outcome resets the streak but keeps the badge count when None
    await store.record_cycle(
        "rg-1", outcome="new_manual", next_check_at=now + 300, quiet=False,
        new_candidate_count=2, now=now,
    )
    watch = await store.get_watch("rg-1")
    assert watch.quiet_streak == 0
    assert watch.new_candidate_count == 2
    await store.record_cycle(
        "rg-1", outcome="seen_only", next_check_at=now + 400, quiet=True, now=now
    )
    assert (await store.get_watch("rg-1")).new_candidate_count == 2  # untouched


@pytest.mark.asyncio
async def test_record_cycle_dormancy_flip(store):
    await _watch(store)
    await store.record_cycle(
        "rg-1", outcome="no_results", next_check_at=time.time(), quiet=True,
        go_dormant=True,
    )
    assert (await store.get_watch("rg-1")).state == "dormant"
    # a dormant row is not re-flipped nor selected; recording again keeps dormant
    await store.record_cycle(
        "rg-1", outcome="no_results", next_check_at=time.time(), quiet=True,
        go_dormant=False,
    )
    assert (await store.get_watch("rg-1")).state == "dormant"


@pytest.mark.asyncio
async def test_seen_set_dedup_semantics(store):
    await _watch(store)
    seen_rows = [("soulseek", "peer\x1fdir-a"), ("usenet", "title\x1f120")]
    await store.record_cycle(
        "rg-1", outcome="new_manual", next_check_at=time.time(), quiet=False,
        new_candidate_count=2, seen=seen_rows,
    )
    # same identities again (plus one new) - inserts dedup on the PK
    await store.record_cycle(
        "rg-1", outcome="new_manual", next_check_at=time.time(), quiet=False,
        new_candidate_count=1, seen=[*seen_rows, ("soulseek", "peer\x1fdir-b")],
    )
    identities = await store.seen_identities("rg-1")
    assert identities == {
        ("soulseek", "peer\x1fdir-a"),
        ("usenet", "title\x1f120"),
        ("soulseek", "peer\x1fdir-b"),
    }


@pytest.mark.asyncio
async def test_clear_new_candidates(store):
    await _watch(store)
    await store.record_cycle(
        "rg-1", outcome="new_manual", next_check_at=time.time(), quiet=False,
        new_candidate_count=4,
    )
    await store.clear_new_candidates("rg-1")
    assert (await store.get_watch("rg-1")).new_candidate_count == 0


@pytest.mark.asyncio
async def test_reschedule_touches_only_next_check(store):
    await _watch(store)
    await store.record_cycle(
        "rg-1", outcome="no_results", next_check_at=time.time(), quiet=True
    )
    before = await store.get_watch("rg-1")
    await store.reschedule("rg-1", before.next_check_at + 999)
    after = await store.get_watch("rg-1")
    assert after.next_check_at == pytest.approx(before.next_check_at + 999)
    assert after.check_count == before.check_count
    assert after.last_outcome == before.last_outcome


@pytest.mark.asyncio
async def test_list_watches_scoping(store):
    await _watch(store, "rg-a", user_id="user-a")
    await _watch(store, "rg-b", user_id="user-b")
    mine = await store.list_watches("user-a")
    assert [w.release_group_mbid for w in mine] == ["rg-a"]
    everyone = await store.list_watches(None)
    assert {w.release_group_mbid for w in everyone} == {"rg-a", "rg-b"}


@pytest.mark.asyncio
async def test_prune_drops_old_terminal_and_orphaned_seen(store, tmp_path: Path):
    old = time.time() - 400 * 86400
    await _watch(store, "rg-old-stopped")
    await _watch(store, "rg-fresh-stopped")
    await _watch(store, "rg-watching")
    await store.record_cycle(
        "rg-old-stopped", outcome="no_results", next_check_at=old, quiet=True,
        seen=[("soulseek", "x\x1fy")], now=old,
    )
    await store.record_cycle(
        "rg-watching", outcome="no_results", next_check_at=old, quiet=True,
        seen=[("soulseek", "keep\x1fme")], now=old,
    )
    await store.stop_watch("rg-old-stopped")
    await store.stop_watch("rg-fresh-stopped")
    # seed a fully orphaned seen row (no watch at all) with raw sqlite
    conn = sqlite3.connect(tmp_path / "library.db")
    conn.execute(
        "INSERT INTO wanted_seen_candidates VALUES ('rg-gone', 'soulseek', 'a\x1fb', ?)",
        (old,),
    )
    conn.commit()
    conn.close()

    watches, seen = await store.prune(retention_days=180)
    assert watches == 1  # only the OLD stopped one
    assert seen == 2  # its seen row + the orphan
    assert await store.get_watch("rg-old-stopped") is None
    assert (await store.get_watch("rg-fresh-stopped")).state == "stopped"
    assert (await store.get_watch("rg-watching")).state == "watching"
    assert await store.seen_identities("rg-watching") == {("soulseek", "keep\x1fme")}
