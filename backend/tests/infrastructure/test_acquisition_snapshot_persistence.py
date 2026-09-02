"""Acquisition-quality snapshot persistence (Acquisition plan Phase 2):

- idempotent construction (twice per store on one tmp SQLite file) with every
  new column appearing EXACTLY once;
- snapshot round-trip through ``update_task_quality_fields`` and back out of
  ``get_task`` (re-decoded ``AcquisitionQualitySnapshot`` equality plus hash
  stability across independent ``build_snapshot`` calls);
- legacy-row tolerance (a pre-column row decodes with Nones / manual False);
- the backfill marker singleton lifecycle;
- the startup-backfill read helpers for tasks, search jobs, and Free Music.

Never mocks sqlite3: every test runs against a real ``tmp_path`` library.db.
"""

import asyncio
import json
import sqlite3
import threading
from pathlib import Path

import msgspec
import pytest

from api.v1.schemas.settings import DownloadPolicySettings
from infrastructure.persistence.download_store import DownloadStore
from infrastructure.persistence.free_music_store import FreeMusicStore
from infrastructure.serialization import to_jsonable
from models.acquisition_quality import (
    AcquisitionQualitySnapshot,
    EvidenceCertainty,
    EvidenceProvenance,
    QualityRecipeEntry,
)
from services.native.acquisition.quality import build_snapshot

_NEW_TASK_COLUMNS = (
    "quality_snapshot_json",
    "quality_snapshot_hash",
    "quality_snapshot_summary",
    "quality_preference_step",
    "quality_certainty",
    "quality_provenance",
    "manual_quality_override",
)
_NEW_SEARCH_JOB_COLUMNS = (
    "quality_snapshot_json",
    "quality_snapshot_hash",
    "quality_snapshot_summary",
)
_NEW_FREE_MUSIC_COLUMNS = (
    "quality_snapshot_json",
    "quality_snapshot_hash",
    "quality_snapshot_summary",
    "tried_candidates_json",
)

# Pre-feature download_tasks DDL: byte-for-byte the live block MINUS the seven
# acquisition-quality columns, so constructing the store exercises the additive
# ratchet over genuine legacy rows (defaults must appear, not errors).
_LEGACY_DOWNLOAD_TASKS_SCHEMA = """
CREATE TABLE auth_users (id TEXT PRIMARY KEY, username TEXT);
INSERT INTO auth_users VALUES ('user-a', 'alice');
CREATE TABLE download_tasks (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    request_history_mbid TEXT,
    download_type TEXT NOT NULL DEFAULT 'album',
    release_group_mbid TEXT NOT NULL,
    release_mbid TEXT,
    release_track_mbid TEXT,
    recording_mbid TEXT,
    artist_mbid TEXT,
    artist_name TEXT NOT NULL,
    album_title TEXT NOT NULL,
    track_title TEXT,
    track_number INTEGER,
    disc_number INTEGER,
    year INTEGER,
    track_count INTEGER,
    track_duration_seconds REAL,
    download_client TEXT NOT NULL DEFAULT 'slskd',
    source TEXT NOT NULL DEFAULT 'soulseek',
    origin TEXT NOT NULL DEFAULT 'user',
    source_username TEXT,
    source_directory TEXT,
    search_query TEXT,
    search_job_id TEXT,
    candidate_index INTEGER,
    status TEXT NOT NULL DEFAULT 'queued'
        CHECK(status IN ('queued','downloading','processing',
                         'completed','partial','failed','cancelled')),
    preflight_score REAL,
    progress_percent INTEGER NOT NULL DEFAULT 0,
    total_size_bytes INTEGER,
    downloaded_bytes INTEGER NOT NULL DEFAULT 0,
    files_total INTEGER NOT NULL DEFAULT 0,
    files_completed INTEGER NOT NULL DEFAULT 0,
    files_failed INTEGER NOT NULL DEFAULT 0,
    quality_format TEXT,
    quality_bitrate INTEGER,
    quality_sample_rate INTEGER,
    quality_bit_depth INTEGER,
    advertised_queue_depth INTEGER,
    queue_position_start INTEGER,
    queue_position_end INTEGER,
    remote_queued INTEGER NOT NULL DEFAULT 0,
    preferred_quality_fallback_at REAL,
    quality_pool_key TEXT,
    attempt_number INTEGER NOT NULL DEFAULT 0,
    attempt_total INTEGER NOT NULL DEFAULT 0,
    has_next_source INTEGER NOT NULL DEFAULT 0,
    staging_path TEXT,
    final_path TEXT,
    error_message TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    last_polled_at REAL,
    created_at REAL NOT NULL,
    started_at REAL,
    completed_at REAL,
    cancelled_at REAL,
    updated_at REAL NOT NULL
);
INSERT INTO download_tasks
    (id, user_id, release_group_mbid, artist_name, album_title, status,
     created_at, updated_at)
VALUES
    ('legacy-task', 'user-a', 'rg-legacy', 'Old Artist', 'Old Album',
     'queued', 100.0, 100.0);
"""


def _seed_auth_users(db_path: Path) -> None:
    conn = sqlite3.connect(db_path)
    try:
        conn.execute(
            "CREATE TABLE IF NOT EXISTS auth_users (id TEXT PRIMARY KEY, username TEXT)"
        )
        conn.execute(
            "INSERT OR IGNORE INTO auth_users (id, username) VALUES ('user-a', 'alice')"
        )
        conn.commit()
    finally:
        conn.close()


def _make_download_store(db_path: Path) -> DownloadStore:
    store = DownloadStore(db_path=db_path, write_lock=threading.Lock())
    _seed_auth_users(db_path)
    return store


def _make_free_music_store(db_path: Path) -> FreeMusicStore:
    return FreeMusicStore(db_path=db_path, write_lock=threading.Lock())


def _column_occurrences(db_path: Path, table: str) -> dict[str, int]:
    """Column-name -> occurrence count from PRAGMA table_info."""
    with sqlite3.connect(db_path) as conn:
        rows = conn.execute(f"PRAGMA table_info({table})").fetchall()
    counts: dict[str, int] = {}
    for row in rows:
        name = str(row[1])
        counts[name] = counts.get(name, 0) + 1
    return counts


def _policy(**overrides: object) -> DownloadPolicySettings:
    base = dict(
        quality_min="mp3_192",
        quality_max="lossless",
        preferred_lossy_bitrate_kbps=320,
        lossy_max_bitrate_kbps=320,
        lossless_preference="cd",
        lossless_max_bit_depth=16,
        lossless_max_sample_rate_hz=48000,
        unknown_quality_behavior="review",
        source_selection_mode="source_first",
    )
    base.update(overrides)
    return DownloadPolicySettings(**base)


def _snapshot_blob(snapshot: AcquisitionQualitySnapshot) -> str:
    return json.dumps(to_jsonable(snapshot))


# --- Idempotent construction ---------------------------------------------------


@pytest.mark.asyncio
async def test_download_store_constructs_twice_columns_appear_once(
    tmp_path: Path,
):
    db_path = tmp_path / "library.db"
    DownloadStore(db_path=db_path, write_lock=threading.Lock())
    _seed_auth_users(db_path)
    DownloadStore(db_path=db_path, write_lock=threading.Lock())

    task_counts = _column_occurrences(db_path, "download_tasks")
    for column in _NEW_TASK_COLUMNS:
        assert task_counts.get(column) == 1, column
    job_counts = _column_occurrences(db_path, "search_jobs")
    for column in _NEW_SEARCH_JOB_COLUMNS:
        assert job_counts.get(column) == 1, column


@pytest.mark.asyncio
async def test_free_music_store_constructs_twice_columns_appear_once(
    tmp_path: Path,
):
    db_path = tmp_path / "library.db"
    FreeMusicStore(db_path=db_path, write_lock=threading.Lock())
    FreeMusicStore(db_path=db_path, write_lock=threading.Lock())

    counts = _column_occurrences(db_path, "free_music_tasks")
    for column in _NEW_FREE_MUSIC_COLUMNS:
        assert counts.get(column) == 1, column


@pytest.mark.asyncio
async def test_both_stores_share_one_sqlite_file_and_reconstruct_cleanly(
    tmp_path: Path,
):
    db_path = tmp_path / "library.db"
    _make_download_store(db_path)  # first build + FK prerequisite seeding
    _make_free_music_store(db_path)
    # Second builds over the SAME file with a shared lock: every executescript
    # and ratchet must stay a no-op, and the marker table lands exactly once.
    lock = threading.Lock()
    DownloadStore(db_path=db_path, write_lock=lock)
    FreeMusicStore(db_path=db_path, write_lock=lock)
    task_counts = _column_occurrences(db_path, "download_tasks")
    music_counts = _column_occurrences(db_path, "free_music_tasks")
    for column in _NEW_TASK_COLUMNS:
        assert task_counts.get(column) == 1, column
    for column in _NEW_FREE_MUSIC_COLUMNS:
        assert music_counts.get(column) == 1, column

    with sqlite3.connect(db_path) as conn:
        marker = conn.execute(
            "SELECT name FROM sqlite_master WHERE type = 'table' "
            "AND name = 'acquisition_snapshot_backfill'"
        ).fetchall()
    assert len(marker) == 1


# --- DownloadTask snapshot writers ---------------------------------------------


@pytest.mark.asyncio
async def test_update_task_quality_fields_roundtrips_snapshot(
    tmp_path: Path,
):
    store = _make_download_store(tmp_path / "library.db")
    policy = _policy()
    snapshot = build_snapshot(policy)
    task = await store.create_task(
        user_id="user-a",
        release_group_mbid="rg-roundtrip",
        artist_name="Round Trip",
        album_title="Persist Me",
    )

    await store.update_task_quality_fields(
        [
            {
                "id": task.id,
                "quality_snapshot_json": _snapshot_blob(snapshot),
                "quality_snapshot_hash": snapshot.snapshot_hash,
                "quality_snapshot_summary": snapshot.summary,
                "quality_preference_step": 1,
                "quality_certainty": EvidenceCertainty.EXACT.value,
                "quality_provenance": EvidenceProvenance.SOURCE_METADATA.value,
                "manual_quality_override": True,
            }
        ]
    )

    fetched = await store.get_task(task.id)
    assert fetched is not None
    assert fetched.manual_quality_override is True
    assert fetched.quality_snapshot_hash == snapshot.snapshot_hash
    assert fetched.quality_snapshot_summary == snapshot.summary
    assert fetched.quality_preference_step == 1
    assert fetched.quality_certainty == EvidenceCertainty.EXACT.value
    assert fetched.quality_provenance == EvidenceProvenance.SOURCE_METADATA.value

    # Re-decode the stored JSON through the struct boundary: equality with the
    # original snapshot, derived order intact, and a SECOND build from the same
    # policy yields the same stable hash (later settings saves never mutate it).
    decoded = msgspec.convert(
        json.loads(str(fetched.quality_snapshot_json)),
        type=AcquisitionQualitySnapshot,
        strict=False,
    )
    assert decoded == snapshot
    assert decoded.quality_preference_order == [
        "lossless",
        "mp3_320",
        "mp3_256",
        "mp3_192",
    ]
    assert build_snapshot(policy).snapshot_hash == fetched.quality_snapshot_hash


@pytest.mark.asyncio
async def test_update_task_quality_fields_partial_writes_reject_bad_keys(
    tmp_path: Path,
):
    store = _make_download_store(tmp_path / "library.db")
    task = await store.create_task(
        user_id="user-a",
        release_group_mbid="rg-partial",
        artist_name="Artist",
        album_title="Album",
    )

    await store.update_task_quality_fields(
        [{"id": task.id, "quality_snapshot_hash": "hash-one"}]
    )

    after_first = await store.get_task(task.id)
    assert after_first is not None
    assert after_first.quality_snapshot_hash == "hash-one"
    assert after_first.quality_snapshot_json is None
    assert after_first.manual_quality_override is False

    # Validation completes before any SQL runs, so even the VALID dict ahead of
    # the offending one never lands.
    with pytest.raises(ValueError, match="not updatable"):
        await store.update_task_quality_fields(
            [
                {"id": task.id, "quality_certainty": "exact"},
                {"id": task.id, "bogus_column": 1},
            ]
        )
    unchanged = await store.get_task(task.id)
    assert unchanged is not None
    assert unchanged.quality_snapshot_hash == "hash-one"
    assert unchanged.quality_certainty is None


# --- SearchJob snapshot writer + backfill feed ----------------------------------


@pytest.mark.asyncio
async def test_search_job_snapshots_write_read_and_feed(tmp_path: Path):
    store = _make_download_store(tmp_path / "library.db")
    job = await store.create_search_job(
        "user-a", "Feed Artist", "Feed Album", 2024, 10, "rg-feed", "feed query"
    )

    # Created without a snapshot -> visible in the backfill feed until written.
    before = await store.list_search_jobs_missing_snapshot()
    assert job.id in {j.id for j in before}

    await store.update_search_job_quality_snapshots(
        [
            {
                "id": job.id,
                "quality_snapshot_json": '{"schema_version": 1}',
                "quality_snapshot_hash": "job-hash",
                "quality_snapshot_summary": "Accepting lossless first.",
            }
        ]
    )

    fetched = await store.get_search_job(job.id)
    assert fetched is not None
    assert fetched.quality_snapshot_json == '{"schema_version": 1}'
    assert fetched.quality_snapshot_hash == "job-hash"
    assert fetched.quality_snapshot_summary == "Accepting lossless first."

    after = await store.list_search_jobs_missing_snapshot()
    assert job.id not in {j.id for j in after}

    with pytest.raises(ValueError, match="not updatable"):
        await store.update_search_job_quality_snapshots(
            [{"id": job.id, "candidates_blob": "[]"}]
        )


# --- Startup-backfill feeds -----------------------------------------------------


@pytest.mark.asyncio
async def test_list_tasks_missing_snapshot_filters_statuses_and_limit(
    tmp_path: Path,
):
    store = _make_download_store(tmp_path / "library.db")
    older_queued = await store.create_task(
        user_id="user-a", release_group_mbid="rg-1", artist_name="A", album_title="B"
    )
    await asyncio.sleep(0.01)
    newer_queued = await store.create_task(
        user_id="user-a", release_group_mbid="rg-2", artist_name="A", album_title="C"
    )
    await asyncio.sleep(0.01)
    failed = await store.create_task(
        user_id="user-a",
        release_group_mbid="rg-3",
        artist_name="A",
        album_title="D",
        status="failed",
    )
    terminal_done = await store.create_task(
        user_id="user-a",
        release_group_mbid="rg-4",
        artist_name="A",
        album_title="E",
        status="completed",
    )

    queued_ids = [t.id for t in await store.list_tasks_missing_snapshot(["queued"])]
    assert queued_ids == [newer_queued.id, older_queued.id]

    limited = await store.list_tasks_missing_snapshot(["queued"], limit=1)
    assert [t.id for t in limited] == [newer_queued.id]

    mixed = await store.list_tasks_missing_snapshot(["queued", "failed"])
    # Newest first across the requested statuses: the failed task was created
    # last, so it heads the feed even though queued rows dominate by count.
    assert [t.id for t in mixed] == [
        failed.id,
        newer_queued.id,
        older_queued.id,
    ]

    assert [] == await store.list_tasks_missing_snapshot([])

    # Once the snapshot lands on a row, it leaves the feed (terminal rows are
    # outside this helper entirely - it takes an explicit status filter).
    await store.update_task_quality_fields(
        [{"id": newer_queued.id, "quality_snapshot_json": "{}"}]
    )
    remaining = await store.list_tasks_missing_snapshot(["queued"])
    assert [t.id for t in remaining] == [older_queued.id]

    with sqlite3.connect(tmp_path / "library.db") as conn:
        landed = conn.execute(
            "SELECT quality_snapshot_json FROM download_tasks WHERE id = ?",
            (terminal_done.id,),
        ).fetchone()[0]
    assert landed is None


@pytest.mark.asyncio
async def test_task_snapshot_backfill_cas_preserves_live_pin_and_fills_null(
    tmp_path: Path,
):
    store = _make_download_store(tmp_path / "library.db")
    live = await store.create_task(
        user_id="user-a",
        release_group_mbid="rg-live",
        artist_name="A",
        album_title="Live",
    )
    pending = await store.create_task(
        user_id="user-a",
        release_group_mbid="rg-pending",
        artist_name="A",
        album_title="Pending",
    )
    await store.update_task_quality_fields(
        [
            {
                "id": live.id,
                "quality_snapshot_json": "live-json",
                "quality_snapshot_hash": "live-hash",
                "quality_snapshot_summary": "Live policy",
            }
        ]
    )

    changed = await store.backfill_task_quality_fields(
        [
            {
                "id": live.id,
                "quality_snapshot_json": "migration-json",
                "quality_snapshot_hash": "migration-hash",
                "quality_snapshot_summary": "Migration policy",
            },
            {
                "id": pending.id,
                "quality_snapshot_json": "migration-json",
                "quality_snapshot_hash": "migration-hash",
                "quality_snapshot_summary": "Migration policy",
            },
        ]
    )

    assert changed == 1
    live_after = await store.get_task(live.id)
    pending_after = await store.get_task(pending.id)
    assert live_after is not None and pending_after is not None
    assert (live_after.quality_snapshot_json, live_after.quality_snapshot_hash) == (
        "live-json",
        "live-hash",
    )
    assert live_after.quality_snapshot_summary == "Live policy"
    assert (
        pending_after.quality_snapshot_json,
        pending_after.quality_snapshot_hash,
    ) == (
        "migration-json",
        "migration-hash",
    )


@pytest.mark.asyncio
async def test_search_snapshot_backfill_cas_preserves_live_pin_and_fills_null(
    tmp_path: Path,
):
    store = _make_download_store(tmp_path / "library.db")
    live = await store.create_search_job(
        "user-a", "A", "Live", None, None, "rg-live", "q"
    )
    pending = await store.create_search_job(
        "user-a", "A", "Pending", None, None, "rg-pending", "q"
    )
    await store.update_search_job_quality_snapshots(
        [
            {
                "id": live.id,
                "quality_snapshot_json": "live-json",
                "quality_snapshot_hash": "live-hash",
                "quality_snapshot_summary": "Live policy",
            }
        ]
    )

    changed = await store.backfill_search_job_quality_snapshots(
        [
            {
                "id": live.id,
                "quality_snapshot_json": "migration-json",
                "quality_snapshot_hash": "migration-hash",
                "quality_snapshot_summary": "Migration policy",
            },
            {
                "id": pending.id,
                "quality_snapshot_json": "migration-json",
                "quality_snapshot_hash": "migration-hash",
                "quality_snapshot_summary": "Migration policy",
            },
        ]
    )

    assert changed == 1
    live_after = await store.get_search_job(live.id)
    pending_after = await store.get_search_job(pending.id)
    assert live_after is not None and pending_after is not None
    assert (live_after.quality_snapshot_json, live_after.quality_snapshot_hash) == (
        "live-json",
        "live-hash",
    )
    assert live_after.quality_snapshot_summary == "Live policy"
    assert (
        pending_after.quality_snapshot_json,
        pending_after.quality_snapshot_hash,
    ) == (
        "migration-json",
        "migration-hash",
    )


# --- Backfill marker ------------------------------------------------------------


@pytest.mark.asyncio
async def test_backfill_marker_gates_completion_and_keeps_single_row(
    tmp_path: Path,
):
    store = _make_download_store(tmp_path / "library.db")

    assert await store.acquisition_backfill_completed() is False

    await store.mark_acquisition_backfill(native_tasks=3, search_jobs=2)
    assert await store.acquisition_backfill_completed() is True

    await store.mark_acquisition_backfill(native_tasks=9, search_jobs=1)
    with sqlite3.connect(tmp_path / "library.db") as conn:
        rows = conn.execute(
            "SELECT id, native_tasks, search_jobs FROM acquisition_snapshot_backfill"
        ).fetchall()
    assert len(rows) == 1
    assert rows[0][0] == 1
    assert rows[0][1] == 9
    assert rows[0][2] == 1


# --- Legacy-row tolerance --------------------------------------------------------


@pytest.mark.asyncio
async def test_legacy_row_ratcheted_then_decoded_with_defaults(tmp_path: Path):
    db_path = tmp_path / "library.db"
    conn = sqlite3.connect(db_path)
    try:
        conn.executescript(_LEGACY_DOWNLOAD_TASKS_SCHEMA)
        conn.commit()
    finally:
        conn.close()

    # Constructing the store runs the additive ratchets over the legacy row.
    store = DownloadStore(db_path=db_path, write_lock=threading.Lock())
    counts = _column_occurrences(db_path, "download_tasks")
    for column in _NEW_TASK_COLUMNS:
        assert counts.get(column) == 1, column

    fetched = await store.get_task("legacy-task")
    assert fetched is not None
    assert fetched.quality_snapshot_json is None
    assert fetched.quality_snapshot_hash is None
    assert fetched.quality_snapshot_summary is None
    assert fetched.quality_preference_step is None
    assert fetched.quality_certainty is None
    assert fetched.quality_provenance is None
    assert fetched.manual_quality_override is False
    assert fetched.release_group_mbid == "rg-legacy"

    # And the freshly-ratcheted legacy row feeds the startup backfill.
    waiting = await store.list_tasks_missing_snapshot(["queued"])
    assert [t.id for t in waiting] == ["legacy-task"]


# --- Free Music ------------------------------------------------------------------


@pytest.mark.asyncio
async def test_free_music_create_carries_snapshot_and_writer_flips_fields(
    tmp_path: Path,
):
    store = _make_free_music_store(tmp_path / "library.db")
    ladder = json.dumps(
        [
            {"identifier": "item-1", "format": "Flac"},
            {"identifier": "item-2", "format": "VBR MP3"},
        ]
    )
    await store.create(
        "fm-snapshotted",
        "user-a",
        "album",
        "rg-fm",
        "Free Artist",
        "Free Title",
        quality_snapshot_hash="snap-hash",
        quality_snapshot_json='{"schema_version": 1}',
        quality_snapshot_summary="Lossless with 320 kbps fallback.",
        tried_candidates_json=ladder,
    )

    got = await store.get("fm-snapshotted")
    assert got is not None
    assert got.quality_snapshot_hash == "snap-hash"
    assert got.quality_snapshot_json == '{"schema_version": 1}'
    assert got.quality_snapshot_summary == "Lossless with 320 kbps fallback."
    assert got.tried_candidates_json == ladder

    # Provided-key-writes-value: the explicit None clears the summary while the
    # omitted keys stay untouched.
    await store.update_free_music_quality_fields(
        [
            {
                "id": "fm-snapshotted",
                "quality_snapshot_hash": "replaced-hash",
                "quality_snapshot_summary": None,
                "tried_candidates_json": "[]",
            }
        ]
    )
    flipped = await store.get("fm-snapshotted")
    assert flipped is not None
    assert flipped.quality_snapshot_hash == "replaced-hash"
    assert flipped.quality_snapshot_summary is None
    assert flipped.quality_snapshot_json == '{"schema_version": 1}'
    assert flipped.tried_candidates_json == "[]"

    with pytest.raises(ValueError, match="not updatable"):
        await store.update_free_music_quality_fields(
            [{"id": "fm-snapshotted", "bytes_downloaded": 5}]
        )
    unchanged = await store.get("fm-snapshotted")
    assert unchanged is not None
    assert unchanged.quality_snapshot_hash == "replaced-hash"


@pytest.mark.asyncio
async def test_free_music_snapshot_backfill_cas_preserves_live_pin_and_fills_null(
    tmp_path: Path,
):
    store = _make_free_music_store(tmp_path / "library.db")
    await store.create("fm-live", "user-a", "album", "rg-live", "Artist", "Live")
    await store.create(
        "fm-pending", "user-a", "album", "rg-pending", "Artist", "Pending"
    )

    await store.update_free_music_quality_fields(
        [
            {
                "id": "fm-live",
                "quality_snapshot_json": '{"schema_version": 2, "live": true}',
                "quality_snapshot_hash": "live-hash",
                "quality_snapshot_summary": "Live policy",
            }
        ]
    )
    changed = await store.backfill_free_music_quality_fields(
        [
            {
                "id": "fm-live",
                "quality_snapshot_json": '{"schema_version": 2, "migration": true}',
                "quality_snapshot_hash": "migration-hash",
                "quality_snapshot_summary": "Migrated policy",
            },
            {
                "id": "fm-pending",
                "quality_snapshot_json": '{"schema_version": 2, "migration": true}',
                "quality_snapshot_hash": "migration-hash",
                "quality_snapshot_summary": "Migrated policy",
            },
        ]
    )

    assert changed == 1
    live = await store.get("fm-live")
    pending = await store.get("fm-pending")
    assert live is not None and pending is not None
    assert live.quality_snapshot_hash == "live-hash"
    assert live.quality_snapshot_json == '{"schema_version": 2, "live": true}'
    assert pending.quality_snapshot_hash == "migration-hash"
    assert pending.quality_snapshot_summary == "Migrated policy"


@pytest.mark.asyncio
async def test_free_music_missing_snapshot_feed_includes_terminal_history(
    tmp_path: Path,
):
    store = _make_free_music_store(tmp_path / "library.db")
    await store.create("fm-older", "user-a", "album", "rg-a", "Artist A", "Title A")
    await asyncio.sleep(0.01)
    await store.create("fm-newer", "user-a", "track", "rec-b", "Artist B", "Title B")
    # Terminal history keeps its snapshot so the UI can show its policy summary.
    await store.update("fm-newer", status="completed")

    ids = [t.id for t in await store.list_tasks_missing_snapshot()]
    assert ids == ["fm-newer", "fm-older"]

    await store.update_free_music_quality_fields(
        [{"id": "fm-older", "quality_snapshot_json": "{}"}]
    )
    remaining = await store.list_tasks_missing_snapshot()
    assert [t.id for t in remaining] == ["fm-newer"]


@pytest.mark.asyncio
async def test_recipe_search_job_and_selected_quality_fields_round_trip(tmp_path: Path):
    store = _make_download_store(tmp_path / "library.db")
    recipe = [
        QualityRecipeEntry(format="flac", quality="cd"),
        QualityRecipeEntry(format="mp3", quality="320_plus"),
    ]
    snapshot = build_snapshot(_policy(quality_recipe=recipe))
    job = await store.create_search_job(
        "user-a",
        "Recipe Artist",
        "Recipe Album",
        2024,
        2,
        "rg-recipe",
        "recipe query",
        quality_snapshot_json=_snapshot_blob(snapshot),
        quality_snapshot_hash=snapshot.snapshot_hash,
        quality_snapshot_summary=snapshot.summary,
    )
    task = await store.create_task(
        user_id="user-a",
        release_group_mbid="rg-recipe",
        artist_name="Recipe Artist",
        album_title="Recipe Album",
    )

    await store.link_picked_candidate(
        task_id=task.id,
        search_job_id=job.id,
        candidate_index=0,
        source_username="peer",
        source_directory="Recipe Artist - Recipe Album",
        preflight_score=0.91,
        quality_preference_step=1,
        quality_certainty=EvidenceCertainty.EXACT.value,
        quality_provenance=EvidenceProvenance.SOURCE_METADATA.value,
    )

    fetched_task = await store.get_task(task.id)
    fetched_job = await store.get_search_job(job.id)
    assert fetched_task is not None
    assert fetched_task.quality_preference_step == 1
    assert fetched_task.quality_certainty == EvidenceCertainty.EXACT.value
    assert fetched_task.quality_provenance == EvidenceProvenance.SOURCE_METADATA.value
    assert fetched_job is not None
    assert fetched_job.quality_snapshot_hash == snapshot.snapshot_hash
    assert (
        msgspec.convert(
            json.loads(fetched_job.quality_snapshot_json),
            type=AcquisitionQualitySnapshot,
            strict=False,
        )
        == snapshot
    )
