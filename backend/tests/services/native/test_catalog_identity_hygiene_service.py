import sqlite3
import threading
import json
from pathlib import Path
from unittest.mock import AsyncMock, Mock

import pytest

from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.local_catalog import (
    CatalogMembership,
    LocalAlbum,
    LocalAlbumExternalIdentity,
    LocalArtist,
    LocalArtistCredit,
    LocalTrack,
    LocalTrackExternalIdentity,
)
from services.native.artist_identity_reconciliation_service import (
    ArtistIdentityReconciliationService,
)
from services.native.background_workload_gate import BackgroundWorkloadGate
from services.native.catalog_identity_hygiene_service import (
    CatalogIdentityHygieneService,
)
from services.native.library_operation_supervisor import LibraryOperationSupervisor

CORRECT_RELEASE_GROUP = "f2026101-945b-3d05-9ef4-aa718fc3feef"
WRONG_RELEASE_GROUP = "ce1dcba9-80aa-35d7-a536-955423fa87b3"
WRONG_RELEASE = "cfdf4c40-b510-46f4-8343-b6bbf2c2bd13"
WRONG_RECORDING = "11111111-1111-4111-8111-111111111111"
WRONG_RELEASE_TRACK = "22222222-2222-4222-8222-222222222222"


@pytest.fixture
def db_path(tmp_path: Path) -> Path:
    path = tmp_path / "library.db"
    with sqlite3.connect(path) as connection:
        connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
        connection.execute("INSERT INTO auth_users VALUES ('admin')")
    return path


@pytest.fixture
def store(db_path: Path) -> NativeLibraryStore:
    return NativeLibraryStore(db_path, threading.Lock())


def _artist(artist_id: str, name: str, created_at: float) -> LocalArtist:
    return LocalArtist(
        id=artist_id,
        display_name=name,
        folded_name=name.casefold(),
        normalized_name=name.casefold(),
        kind="group",
        created_at=created_at,
        updated_at=created_at,
    )


def _track(
    track_id: str,
    album_id: str,
    artist_name: str,
    *,
    filename: str,
    embedded_release_group: str = CORRECT_RELEASE_GROUP,
) -> LocalTrack:
    position = filename.split(" ", 1)[0]
    disc_number = int(position[:2])
    track_number = int(position[2:4])
    return LocalTrack(
        id=track_id,
        local_album_id=album_id,
        root_id="root-1",
        file_path=f"/music/Pink Floyd/The Wall (1979)/{filename}",
        relative_path=f"Pink Floyd/The Wall (1979)/{filename}",
        path_hash=f"hash-{track_id}",
        file_size_bytes=100,
        file_mtime_ns=200,
        stat_revision=f"stat-{track_id}",
        tag_revision=f"tag-{track_id}",
        title=filename.removesuffix(".flac"),
        disc_number=disc_number,
        track_number=track_number,
        artist_name=artist_name,
        album_title="The Wall",
        album_artist_name="Pink Floyd",
        tag_album_title="The Wall",
        tag_album_artist_name="Pink Floyd",
        embedded_release_group_mbid=embedded_release_group,
        file_format="flac",
        imported_at=1,
        membership_source="legacy_import",
        membership_locked=True,
        applied_policy="automatic",
    )


async def _seed_split(
    store: NativeLibraryStore,
    *,
    exact_track_map: bool = False,
    shared_directory: bool = True,
    extra_target_track: bool = False,
) -> tuple[str, str]:
    pink_floyd = _artist("artist-pink-floyd", "Pink Floyd", 1)
    billy = _artist("artist-billy", "Billy Sherwood", 2)
    target_id = "album-z-target"
    source_id = "album-a-source"
    target_track = _track(
        "track-target", target_id, "Pink Floyd", filename="0103 Track.flac"
    )
    if not shared_directory:
        target_track.file_path = "/music/Pink Floyd/Other/0103 Track.flac"
        target_track.relative_path = "Pink Floyd/Other/0103 Track.flac"
    target_tracks = [target_track]
    if extra_target_track:
        target_tracks.append(
            _track(
                "track-target-extra",
                target_id,
                "Pink Floyd",
                filename="0104 Extra.flac",
            )
        )
    await store.create_catalog_membership(
        CatalogMembership(
            album=LocalAlbum(
                id=target_id,
                root_id="root-1",
                grouping_key=f"legacy:{CORRECT_RELEASE_GROUP}",
                title="The Wall",
                album_artist_id=pink_floyd.id,
                album_artist_name="Pink Floyd",
                grouping_source="legacy_import",
                grouping_locked=True,
                created_at=1,
                updated_at=1,
            ),
            artists=[pink_floyd],
            tracks=target_tracks,
            album_credits=[
                LocalArtistCredit(local_artist_id=pink_floyd.id, position=0)
            ],
            track_credits={
                track.id: [LocalArtistCredit(local_artist_id=pink_floyd.id, position=0)]
                for track in target_tracks
            },
        )
    )
    source_tracks = [
        _track("track-source-1", source_id, "Pink Floyd", filename="0101 One.flac"),
        _track("track-source-2", source_id, "Pink Floyd", filename="0102 Two.flac"),
    ]
    await store.create_catalog_membership(
        CatalogMembership(
            album=LocalAlbum(
                id=source_id,
                root_id="root-1",
                grouping_key=f"legacy:{WRONG_RELEASE_GROUP}",
                title="The Wall",
                album_artist_id=billy.id,
                album_artist_name="Billy Sherwood",
                grouping_source="legacy_import",
                grouping_locked=True,
                created_at=2,
                updated_at=2,
            ),
            artists=[billy],
            tracks=source_tracks,
            album_credits=[LocalArtistCredit(local_artist_id=billy.id, position=0)],
            track_credits={
                track.id: [LocalArtistCredit(local_artist_id=pink_floyd.id, position=0)]
                for track in source_tracks
            },
        )
    )
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id=target_id,
            release_group_mbid=CORRECT_RELEASE_GROUP,
            decision_source="legacy_import",
            selected_at=2,
        ),
        expected_album_revision=1,
    )
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id=source_id,
            release_group_mbid=WRONG_RELEASE_GROUP,
            release_mbid=WRONG_RELEASE,
            decision_source="legacy_import",
            selected_at=2,
        ),
        expected_album_revision=1,
    )
    for index, track in enumerate(source_tracks, 1):
        await store.attach_track_identity(
            LocalTrackExternalIdentity(
                local_track_id=track.id,
                recording_mbid=WRONG_RECORDING,
                release_mbid=WRONG_RELEASE,
                release_track_mbid=(WRONG_RELEASE_TRACK if exact_track_map else None),
                medium_position=1 if exact_track_map else None,
                release_track_position=index if exact_track_map else None,
                decision_source="legacy_import",
                selected_at=2,
            ),
            expected_track_revision=1,
        )
    return source_id, target_id


async def _run_backfill(
    store: NativeLibraryStore,
    *,
    workload_gate: BackgroundWorkloadGate | None = None,
) -> tuple[dict, CatalogIdentityHygieneService, AsyncMock]:
    changed = AsyncMock()
    service = CatalogIdentityHygieneService(
        store,
        workload_gate,
        changed,
        clock=lambda: 3,
    )
    job = await service.enqueue_backfill()
    claimed = await store.claim_operation_job(
        "worker", now=3, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    return await service.run_claimed(claimed, "worker"), service, changed


@pytest.mark.asyncio
async def test_provider_contradiction_repairs_split_without_touching_files(
    store: NativeLibraryStore, db_path: Path
) -> None:
    source_id, target_id = await _seed_split(store)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_user_favorites VALUES " "('admin','album',?,1)",
            (source_id,),
        )
        connection.execute(
            "INSERT INTO library_play_history "
            "(id,user_id,local_album_id,track_name,artist_name,played_at) "
            "VALUES ('history','admin',?,'One','Pink Floyd','2026-01-01')",
            (source_id,),
        )
        connection.execute(
            "INSERT INTO library_compat_id_map VALUES ('jf-source','album',?)",
            (source_id,),
        )
        connection.execute(
            "INSERT INTO library_migration_provenance "
            "(source_kind,source_key,target_kind,target_id,source_revision,imported_at) "
            "VALUES ('legacy_album','source','local_album',?,'v1',1)",
            (source_id,),
        )
        connection.execute(
            "INSERT INTO local_entity_source_links "
            "(id,local_album_id,provider,external_entity_type,external_id,canonical_url,"
            "decision_source,verified_at,created_at,updated_at) "
            "VALUES ('link-source',?,'musicbrainz','release_group',?,"
            "'https://musicbrainz.org','legacy_import',1,1,1)",
            (source_id, WRONG_RELEASE_GROUP),
        )

    wake_revision = store.work_wakeups.revision("identification")
    result, _service, changed = await _run_backfill(store)

    assert result["state"] == "succeeded"
    assert changed.await_count == 1
    assert store.work_wakeups.revision("identification") == wake_revision + 1
    with sqlite3.connect(db_path) as connection:
        retired_into = connection.execute(
            "SELECT retired_into_album_id FROM local_albums WHERE id = ?", (source_id,)
        ).fetchone()[0]
        target_tracks = connection.execute(
            "SELECT id FROM local_tracks WHERE local_album_id = ? ORDER BY id",
            (target_id,),
        ).fetchall()
        source_identity_count = connection.execute(
            "SELECT COUNT(*) FROM local_album_external_identities WHERE local_album_id = ?",
            (source_id,),
        ).fetchone()[0]
        source_track_identity_count = connection.execute(
            "SELECT COUNT(*) FROM local_track_external_identities "
            "WHERE local_track_id IN ('track-source-1','track-source-2')"
        ).fetchone()[0]
        alias = connection.execute(
            "SELECT local_album_id FROM local_album_aliases WHERE alias = ?",
            (source_id,),
        ).fetchone()[0]
        favorite = connection.execute(
            "SELECT item_id FROM library_user_favorites WHERE item_kind = 'album'"
        ).fetchone()[0]
        history = connection.execute(
            "SELECT local_album_id FROM library_play_history WHERE id = 'history'"
        ).fetchone()[0]
        compat = connection.execute(
            "SELECT internal_id FROM library_compat_id_map WHERE jf_id = 'jf-source'"
        ).fetchone()[0]
        provenance = connection.execute(
            "SELECT target_id FROM library_migration_provenance "
            "WHERE source_key = 'source'"
        ).fetchone()[0]
        source_link = connection.execute(
            "SELECT local_album_id FROM local_entity_source_links WHERE id = 'link-source'"
        ).fetchone()[0]
        action = connection.execute(
            "SELECT actor_user_id, reason_code, after_json FROM library_catalog_actions "
            "WHERE reason_code = 'AUTOMATIC_PROVIDER_CONTRADICTION_CATALOG_REPAIR'"
        ).fetchone()
        queued_identification = connection.execute(
            "SELECT id, local_album_id, kind, input_revision FROM "
            "library_identification_jobs WHERE state = 'queued'"
        ).fetchone()
        work_result = json.loads(
            connection.execute(
                "SELECT result_json FROM library_operation_work WHERE job_id = ? "
                "AND local_album_id = ?",
                (result["id"], source_id),
            ).fetchone()[0]
        )
        filesystem_journal_count = connection.execute(
            "SELECT COUNT(*) FROM library_file_mutation_journal"
        ).fetchone()[0]
    assert retired_into == target_id
    assert [row[0] for row in target_tracks] == [
        "track-source-1",
        "track-source-2",
        "track-target",
    ]
    assert source_identity_count == 0
    assert source_track_identity_count == 0
    assert alias == target_id
    assert favorite == target_id
    assert history == target_id
    assert compat == target_id
    assert provenance == target_id
    assert source_link == target_id
    assert action is not None and action[0] is None
    assert action[1] == "AUTOMATIC_PROVIDER_CONTRADICTION_CATALOG_REPAIR"
    assert '"filesystem_writes": 0' in action[2]
    assert queued_identification[1] == target_id
    assert queued_identification[2] == "post_processing"
    assert queued_identification[3] == work_result["reidentification_input_revision"]
    assert queued_identification[0] == work_result["reidentification_job_id"]
    assert work_result["reidentification_job_created"] is True
    assert filesystem_journal_count == 0


@pytest.mark.asyncio
async def test_complete_exact_track_map_is_never_overridden(
    store: NativeLibraryStore, db_path: Path
) -> None:
    source_id, target_id = await _seed_split(store, exact_track_map=True)

    _result, _service, changed = await _run_backfill(store)

    with sqlite3.connect(db_path) as connection:
        source = connection.execute(
            "SELECT retired_into_album_id FROM local_albums WHERE id = ?", (source_id,)
        ).fetchone()[0]
        source_track_count = connection.execute(
            "SELECT COUNT(*) FROM local_tracks WHERE local_album_id = ?", (source_id,)
        ).fetchone()[0]
        action_count = connection.execute(
            "SELECT COUNT(*) FROM library_catalog_actions WHERE reason_code = "
            "'AUTOMATIC_PROVIDER_CONTRADICTION_CATALOG_REPAIR'"
        ).fetchone()[0]
    assert source is None
    assert source_track_count == 2
    assert target_id != source_id
    assert action_count == 0
    assert changed.await_count == 0


@pytest.mark.asyncio
async def test_different_physical_directories_are_not_merged(
    store: NativeLibraryStore, db_path: Path
) -> None:
    source_id, _target_id = await _seed_split(store, shared_directory=False)

    await _run_backfill(store)

    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT retired_into_album_id FROM local_albums WHERE id = ?",
                (source_id,),
            ).fetchone()[0]
            is None
        )


@pytest.mark.asyncio
async def test_later_manual_track_identity_blocks_legacy_repair(
    store: NativeLibraryStore, db_path: Path
) -> None:
    source_id, _target_id = await _seed_split(store)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_track_external_identities SET decision_source = 'manual' "
            "WHERE local_track_id = 'track-source-1'"
        )

    await _run_backfill(store)

    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT retired_into_album_id FROM local_albums WHERE id = ?",
                (source_id,),
            ).fetchone()[0]
            is None
        )
        assert (
            connection.execute(
                "SELECT decision_source FROM local_track_external_identities "
                "WHERE local_track_id = 'track-source-1'"
            ).fetchone()[0]
            == "manual"
        )


@pytest.mark.asyncio
async def test_duplicate_disc_track_position_blocks_legacy_repair(
    store: NativeLibraryStore, db_path: Path
) -> None:
    source_id, _target_id = await _seed_split(store)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET track_number = 1 WHERE id = 'track-target'"
        )

    await _run_backfill(store)

    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT retired_into_album_id FROM local_albums WHERE id = ?",
                (source_id,),
            ).fetchone()[0]
            is None
        )


@pytest.mark.asyncio
async def test_nonpositive_disc_track_position_blocks_without_failing_job(
    store: NativeLibraryStore, db_path: Path
) -> None:
    source_id, _target_id = await _seed_split(store)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET disc_number = 0 WHERE id = 'track-source-1'"
        )

    result, _service, _changed = await _run_backfill(store)

    assert result["state"] == "succeeded"
    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT retired_into_album_id FROM local_albums WHERE id = ?",
                (source_id,),
            ).fetchone()[0]
            is None
        )


@pytest.mark.asyncio
async def test_manual_management_override_blocks_legacy_repair(
    store: NativeLibraryStore, db_path: Path
) -> None:
    source_id, _target_id = await _seed_split(store)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_management_overrides "
            "(id,subject_kind,local_album_id,field_name,value_json,mode,"
            "subject_revision,created_at,updated_at) "
            "VALUES ('override','album',?,'album','\"The Wall\"','replace',1,1,1)",
            (source_id,),
        )

    await _run_backfill(store)

    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT retired_into_album_id FROM local_albums WHERE id = ?",
                (source_id,),
            ).fetchone()[0]
            is None
        )
        assert (
            connection.execute(
                "SELECT local_album_id FROM library_management_overrides "
                "WHERE id = 'override'"
            ).fetchone()[0]
            == source_id
        )


@pytest.mark.asyncio
async def test_target_management_override_blocks_legacy_repair(
    store: NativeLibraryStore, db_path: Path
) -> None:
    source_id, target_id = await _seed_split(store)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_management_overrides "
            "(id,subject_kind,local_album_id,field_name,value_json,mode,"
            "subject_revision,created_at,updated_at) "
            "VALUES ('override','album',?,'album','\"The Wall\"','replace',1,1,1)",
            (target_id,),
        )

    await _run_backfill(store)

    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT retired_into_album_id FROM local_albums WHERE id = ?",
                (source_id,),
            ).fetchone()[0]
            is None
        )
        assert (
            connection.execute(
                "SELECT local_album_id FROM library_management_overrides "
                "WHERE id = 'override'"
            ).fetchone()[0]
            == target_id
        )


@pytest.mark.asyncio
async def test_nonindexed_source_track_blocks_legacy_repair(
    store: NativeLibraryStore, db_path: Path
) -> None:
    source_id, _target_id = await _seed_split(store)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET availability = 'missing' "
            "WHERE id = 'track-source-2'"
        )

    await _run_backfill(store)

    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT retired_into_album_id FROM local_albums WHERE id = ?",
                (source_id,),
            ).fetchone()[0]
            is None
        )
        assert (
            connection.execute(
                "SELECT local_album_id FROM local_tracks WHERE id = 'track-source-2'"
            ).fetchone()[0]
            == source_id
        )


@pytest.mark.asyncio
async def test_nonindexed_target_track_blocks_legacy_repair(
    store: NativeLibraryStore, db_path: Path
) -> None:
    source_id, target_id = await _seed_split(store, extra_target_track=True)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET availability = 'missing' "
            "WHERE id = 'track-target-extra'"
        )

    await _run_backfill(store)

    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT retired_into_album_id FROM local_albums WHERE id = ?",
                (source_id,),
            ).fetchone()[0]
            is None
        )
        assert (
            connection.execute(
                "SELECT local_album_id FROM local_tracks WHERE id = 'track-target-extra'"
            ).fetchone()[0]
            == target_id
        )


@pytest.mark.asyncio
async def test_artist_projection_trust_gate_blocks_contradictory_legacy_identity(
    store: NativeLibraryStore, db_path: Path
) -> None:
    source_id, _target_id = await _seed_split(store)
    provider = AsyncMock()
    service = ArtistIdentityReconciliationService(store, provider, clock=lambda: 3)
    job = await service.enqueue_album(source_id)
    assert job is not None
    claimed = await store.claim_operation_job(
        "worker", now=3, lease_seconds=60, kind="repair"
    )
    assert claimed is not None

    result = await service.run_claimed(claimed, "worker")

    assert result["state"] == "succeeded"
    provider.get_canonical_release.assert_not_awaited()
    with sqlite3.connect(db_path) as connection:
        state = connection.execute(
            "SELECT state, reason_code FROM library_artist_reconciliation_state "
            "WHERE local_album_id = ?",
            (source_id,),
        ).fetchone()
        action_count = connection.execute(
            "SELECT COUNT(*) FROM library_catalog_actions WHERE reason_code LIKE "
            "'AUTOMATIC_PROVIDER%CONVERGENCE'"
        ).fetchone()[0]
    assert state == (
        "provider_conflict",
        "LEGACY_IDENTITY_CONTRADICTS_EMBEDDED_RELEASE_GROUP",
    )
    assert action_count == 0


@pytest.mark.asyncio
async def test_unique_empty_automatic_shell_retires_and_preserves_alias(
    store: NativeLibraryStore, db_path: Path
) -> None:
    pink_floyd = _artist("artist-pink-floyd", "Pink Floyd", 1)
    target_id = "album-target"
    shell_id = "album-shell"
    target_track = _track(
        "track-target", target_id, "Pink Floyd", filename="0101 One.flac"
    )
    await store.create_catalog_membership(
        CatalogMembership(
            album=LocalAlbum(
                id=target_id,
                root_id="root-1",
                grouping_key="active",
                title="The Wall",
                album_artist_id=pink_floyd.id,
                album_artist_name="Pink Floyd",
                created_at=1,
                updated_at=1,
            ),
            artists=[pink_floyd],
            tracks=[target_track],
            album_credits=[
                LocalArtistCredit(local_artist_id=pink_floyd.id, position=0)
            ],
            track_credits={
                target_track.id: [
                    LocalArtistCredit(local_artist_id=pink_floyd.id, position=0)
                ]
            },
        )
    )
    await store.create_catalog_membership(
        CatalogMembership(
            album=LocalAlbum(
                id=shell_id,
                root_id="root-1",
                grouping_key="stale-shell",
                title="The Wall",
                album_artist_id=pink_floyd.id,
                album_artist_name="Pink Floyd",
                created_at=2,
                updated_at=2,
            ),
            album_credits=[
                LocalArtistCredit(local_artist_id=pink_floyd.id, position=0)
            ],
        )
    )

    first = CatalogIdentityHygieneService(store, clock=lambda: 3)
    first_job = await first.enqueue_backfill()
    second_job = await first.enqueue_backfill()
    assert second_job["id"] == first_job["id"]
    claimed = await store.claim_operation_job(
        "worker", now=3, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    await first.run_claimed(claimed, "worker")

    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT retired_into_album_id FROM local_albums WHERE id = ?",
                (shell_id,),
            ).fetchone()[0]
            == target_id
        )
        assert (
            connection.execute(
                "SELECT local_album_id FROM local_album_aliases WHERE alias = ?",
                (shell_id,),
            ).fetchone()[0]
            == target_id
        )
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_catalog_actions WHERE reason_code = "
                "'AUTOMATIC_EMPTY_ALBUM_SHELL_CONVERGENCE'"
            ).fetchone()[0]
            == 1
        )


@pytest.mark.asyncio
async def test_empty_shell_never_transfers_unproven_provider_identity(
    store: NativeLibraryStore, db_path: Path
) -> None:
    artist = _artist("artist", "Artist", 1)
    target_id = "album-target"
    shell_id = "album-shell"
    target_track = LocalTrack(
        id="track-target",
        local_album_id=target_id,
        root_id="root-1",
        file_path="/music/Artist/Release/01.flac",
        relative_path="Artist/Release/01.flac",
        path_hash="hash",
        file_size_bytes=1,
        file_mtime_ns=1,
        stat_revision="stat",
        title="Track",
        artist_name="Artist",
        album_title="Release",
        album_artist_name="Artist",
        file_format="flac",
        imported_at=1,
    )
    await store.create_catalog_membership(
        CatalogMembership(
            album=LocalAlbum(
                id=target_id,
                root_id="root-1",
                grouping_key="target",
                title="Release",
                album_artist_id=artist.id,
                album_artist_name="Artist",
                created_at=1,
                updated_at=1,
            ),
            artists=[artist],
            tracks=[target_track],
            album_credits=[LocalArtistCredit(local_artist_id=artist.id, position=0)],
            track_credits={
                target_track.id: [
                    LocalArtistCredit(local_artist_id=artist.id, position=0)
                ]
            },
        )
    )
    await store.create_catalog_membership(
        CatalogMembership(
            album=LocalAlbum(
                id=shell_id,
                root_id="root-1",
                grouping_key="shell",
                title="Release",
                album_artist_id=artist.id,
                album_artist_name="Artist",
                created_at=2,
                updated_at=2,
            ),
            album_credits=[LocalArtistCredit(local_artist_id=artist.id, position=0)],
        )
    )
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id=shell_id,
            release_group_mbid=CORRECT_RELEASE_GROUP,
            decision_source="automatic",
            selected_at=2,
        ),
        expected_album_revision=1,
    )

    await _run_backfill(store)

    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT retired_into_album_id FROM local_albums WHERE id = ?",
                (shell_id,),
            ).fetchone()[0]
            is None
        )
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM local_album_external_identities "
                "WHERE local_album_id = ?",
                (target_id,),
            ).fetchone()[0]
            == 0
        )
        assert (
            connection.execute(
                "SELECT release_group_mbid FROM local_album_external_identities "
                "WHERE local_album_id = ?",
                (shell_id,),
            ).fetchone()[0]
            == CORRECT_RELEASE_GROUP
        )


@pytest.mark.asyncio
async def test_scan_gate_defers_same_durable_hygiene_work(
    store: NativeLibraryStore,
) -> None:
    await _seed_split(store)
    gate = BackgroundWorkloadGate()
    gate.set_scan_active(True)
    result, service, changed = await _run_backfill(store, workload_gate=gate)

    assert result["state"] == "queued"
    assert changed.await_count == 0
    gate.set_scan_active(False)
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    resumed = await service.run_claimed(claimed, "worker")
    assert resumed["state"] == "succeeded"


@pytest.mark.asyncio
async def test_shared_operation_supervisor_dispatches_catalog_hygiene_repair() -> None:
    store = AsyncMock()
    job = {"id": "catalog-hygiene-job", "kind": "repair"}
    store.claim_operation_job.side_effect = [None, None, job]
    store.get_operation_snapshot.return_value = {
        "snapshot": {
            "scope_json": '{"purpose":"catalog_identity_hygiene"}',
            "phase": "audit",
        }
    }
    operations = Mock()
    operations.response_for.return_value = "response"
    hygiene = AsyncMock()
    hygiene.run_claimed.return_value = {"id": job["id"], "state": "succeeded"}
    supervisor = LibraryOperationSupervisor(
        store,
        operations,
        AsyncMock(),
        AsyncMock(),
        catalog_identity_hygiene=hygiene,
    )

    result = await supervisor.run_once("worker", now=3)

    assert result == "response"
    hygiene.run_claimed.assert_awaited_once_with(job, "worker")


@pytest.mark.asyncio
async def test_album_reenqueue_with_moved_revision_joins_queued_job(
    store: NativeLibraryStore, db_path: Path
) -> None:
    source_id, _target_id = await _seed_split(store)
    service = CatalogIdentityHygieneService(store, clock=lambda: 3)

    first = await service.enqueue_album(source_id)
    assert first is not None
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET row_revision = row_revision + 1 "
            "WHERE local_album_id = ?",
            (source_id,),
        )
    second = await service.enqueue_album(source_id)
    assert second is not None
    assert second["id"] == first["id"]
    with sqlite3.connect(db_path) as connection:
        count = connection.execute(
            "SELECT COUNT(*) FROM library_operation_jobs WHERE kind = 'repair'"
        ).fetchone()[0]
    assert count == 1


@pytest.mark.asyncio
async def test_album_reenqueue_after_terminal_state_creates_fresh_job(
    store: NativeLibraryStore, db_path: Path
) -> None:
    source_id, _target_id = await _seed_split(store)
    service = CatalogIdentityHygieneService(store, clock=lambda: 3)

    first = await service.enqueue_album(source_id)
    assert first is not None
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_operation_jobs SET state = 'succeeded', "
            "terminal_code = 'HYGIENE_COMPLETED', terminal_at = 4 WHERE id = ?",
            (first["id"],),
        )
        connection.execute(
            "UPDATE local_tracks SET row_revision = row_revision + 1 "
            "WHERE local_album_id = ?",
            (source_id,),
        )
    second = await service.enqueue_album(source_id)
    assert second is not None
    assert second["id"] != first["id"]
    with sqlite3.connect(db_path) as connection:
        count = connection.execute(
            "SELECT COUNT(*) FROM library_operation_jobs WHERE kind = 'repair'"
        ).fetchone()[0]
    assert count == 2
