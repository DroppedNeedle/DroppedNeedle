"""MbCanonicalStore recording -> release-group homes map (#204)."""

import sqlite3
import threading
import time

import pytest

import repositories.musicbrainz_base as mb_base

from infrastructure.persistence.mb_canonical_store import MbCanonicalStore
from models.release_home import RecordingHome
from repositories.musicbrainz_base import OFFICIAL_MB_API_BASE, MbSourceContext

_CONTEXT = MbSourceContext(
    source_url=OFFICIAL_MB_API_BASE, generation=0, source_mode="official", source_id=""
)
_OTHER_CONTEXT = MbSourceContext(
    source_url="http://mirror", generation=0, source_mode="mirror", source_id="m1"
)
_DAY = 86400.0


@pytest.fixture(autouse=True)
def official_musicbrainz_source():
    """Start from the official source at generation 0 and put back whatever
    was current: other suites move the process-wide source, and a stale one
    makes every banked write a (correctly) skipped no-op."""
    before = mb_base.capture_mb_source_context()
    before_runtime = mb_base.brainzmash_runtime_enabled()
    mb_base.set_mb_api_base(
        mb_base.OFFICIAL_MB_API_BASE, source_mode="official", source_id="", generation=0
    )
    yield
    mb_base.set_mb_api_base(
        before.source_url,
        source_mode=before.source_mode,
        source_id=before.source_id,
        generation=before.generation,
        brainzmash_binding_valid=before_runtime,
    )


def _home(rg: str, **overrides) -> RecordingHome:
    fields = {
        "release_group_mbid": rg,
        "title": rg.title(),
        "primary_type": "Album",
        "secondary_types": (),
        "official": True,
        "is_various": False,
        "artist_credit": "Creedence Clearwater Revival",
        "first_release_date": "1969-11-02",
        "release_mbid": f"rel-{rg.lower()}",
    }
    fields.update(overrides)
    return RecordingHome(**fields)


@pytest.fixture
def db_path(tmp_path):
    return tmp_path / "library.db"


@pytest.fixture
def store(db_path):
    return MbCanonicalStore(db_path=db_path, write_lock=threading.Lock())


@pytest.mark.asyncio
async def test_round_trip_preserves_every_field(store):
    comp = _home(
        "rg-chronicle",
        secondary_types=("Compilation",),
        official=False,
        is_various=True,
        artist_credit="Various Artists",
        first_release_date=None,
        primary_type=None,
    )
    album = _home("RG-Willy", artist_mbids=("ccr", "fogerty"))
    await store.save_recording_release_groups(
        "REC-1", [comp, album], source_context=_CONTEXT
    )

    got = await store.get_recording_release_groups(
        ["rec-1"], source_context=_CONTEXT, max_age_seconds=_DAY
    )

    # MBIDs are normalized to lower case; rows come back ordered by MBID.
    assert got == {
        "rec-1": [
            comp,
            _home("rg-willy", title="Rg-Willy", artist_mbids=("ccr", "fogerty")),
        ]
    }


@pytest.mark.asyncio
async def test_browsed_empty_recording_is_authoritative(store):
    await store.save_recording_release_groups("rec-1", [], source_context=_CONTEXT)
    got = await store.get_recording_release_groups(
        ["rec-1", "rec-unknown"], source_context=_CONTEXT, max_age_seconds=_DAY
    )
    assert got == {"rec-1": []}


@pytest.mark.asyncio
async def test_save_replaces_previous_homes(store):
    await store.save_recording_release_groups(
        "rec-1", [_home("rg-a"), _home("rg-b")], source_context=_CONTEXT
    )
    await store.save_recording_release_groups(
        "rec-1", [_home("rg-c")], source_context=_CONTEXT
    )
    got = await store.get_recording_release_groups(
        ["rec-1"], source_context=_CONTEXT, max_age_seconds=_DAY
    )
    assert [home.release_group_mbid for home in got["rec-1"]] == ["rg-c"]


@pytest.mark.asyncio
async def test_stale_rows_are_misses(store, db_path):
    await store.save_recording_release_groups(
        "rec-1", [_home("rg-a")], source_context=_CONTEXT
    )
    conn = sqlite3.connect(db_path)
    conn.execute(
        "UPDATE recording_release_group_fetch SET saved_at = ?",
        (time.time() - 31 * _DAY,),
    )
    conn.commit()
    conn.close()

    got = await store.get_recording_release_groups(
        ["rec-1"], source_context=_CONTEXT, max_age_seconds=30 * _DAY
    )
    assert got == {}


@pytest.mark.asyncio
async def test_rows_are_scoped_to_source_context(store):
    await store.save_recording_release_groups(
        "rec-1", [_home("rg-a")], source_context=_CONTEXT
    )
    got = await store.get_recording_release_groups(
        ["rec-1"], source_context=_OTHER_CONTEXT, max_age_seconds=_DAY
    )
    assert got == {}


@pytest.mark.asyncio
async def test_empty_input_reads_nothing(store):
    assert (
        await store.get_recording_release_groups(
            [], source_context=_CONTEXT, max_age_seconds=_DAY
        )
        == {}
    )


def test_tables_survive_reconstruction(db_path):
    MbCanonicalStore(db_path=db_path, write_lock=threading.Lock())
    MbCanonicalStore(db_path=db_path, write_lock=threading.Lock())
    conn = sqlite3.connect(db_path)
    tables = {
        row[0]
        for row in conn.execute("SELECT name FROM sqlite_master WHERE type='table'")
    }
    conn.close()
    assert {"recording_release_groups", "recording_release_group_fetch"} <= tables


@pytest.mark.asyncio
async def test_batch_read_beyond_sqlite_parameter_limit(store):
    recordings = [f"rec-{i:04d}" for i in range(1200)]
    for recording in recordings:
        await store.save_recording_release_groups(
            recording, [_home(f"rg-{recording}")], source_context=_CONTEXT
        )
    got = await store.get_recording_release_groups(
        recordings, source_context=_CONTEXT, max_age_seconds=_DAY
    )
    assert len(got) == 1200
    assert got["rec-1199"][0].release_group_mbid == "rg-rec-1199"


@pytest.mark.asyncio
async def test_rows_without_an_edition_are_stale(store):
    """Homes banked before editions were recorded must be refetched: a track
    request needs the edition that carries the recording."""
    await store.save_recording_release_groups(
        "rec-old", [_home("rg-a", release_mbid=None)], source_context=_CONTEXT
    )
    await store.save_recording_release_groups(
        "rec-new", [_home("rg-a")], source_context=_CONTEXT
    )
    got = await store.get_recording_release_groups(
        ["rec-old", "rec-new"], source_context=_CONTEXT, max_age_seconds=_DAY
    )
    assert list(got) == ["rec-new"]
    assert got["rec-new"][0].release_mbid == "rel-rg-a"
