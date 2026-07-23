import hashlib
import json
import sqlite3
import threading
from pathlib import Path

import msgspec
import pytest

from core.exceptions import (
    ConflictError,
    RevisionOverflowError,
    StaleRevisionError,
    ValidationError,
)
from infrastructure.persistence.native_library_store import (
    MANAGEMENT_PERSISTENCE_BATCH_SIZE,
    MAX_REVISION,
    NativeLibraryStore,
)
from models.library_management import (
    LibraryFileMutationJournal,
    LibraryManagementBaseline,
    LibraryManagementBlob,
    LibraryManagementCatalogMutation,
    LibraryManagementJobSnapshot,
    LibraryManagementMetadataSnapshot,
    LibraryManagementOperationSnapshot,
    LibraryManagementOverride,
    LibraryManagementPlanItem,
    LibraryManagementTagEditFieldIntent,
    LibraryManagementTagEditIntent,
)
from models.audio import AudioInfo, AudioTag
from models.library_management_planning import (
    LibraryManagementCatalogFilter,
    LibraryManagementRootScope,
    LibraryManagementSelection,
    NormalizedLibraryManagementSelection,
)
from models.library_work import OperationJob
from models.local_catalog import LocalTrackExternalIdentity


def _seed_auth(path: Path) -> None:
    with sqlite3.connect(path) as connection:
        connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
        connection.execute("INSERT INTO auth_users(id) VALUES ('admin')")


@pytest.fixture
def db_path(tmp_path: Path) -> Path:
    path = tmp_path / "library.db"
    _seed_auth(path)
    return path


@pytest.fixture
def store(db_path: Path) -> NativeLibraryStore:
    value = NativeLibraryStore(db_path, threading.Lock())
    _seed_catalog(db_path)
    return value


def _seed_catalog(path: Path) -> None:
    with sqlite3.connect(path) as connection:
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute(
            "INSERT OR IGNORE INTO local_artists "
            "(id, display_name, folded_name, normalized_name, kind, created_at, updated_at) "
            "VALUES ('artist-1', 'Artist', 'artist', 'artist', 'person', 1, 1)"
        )
        connection.execute(
            "INSERT OR IGNORE INTO local_albums "
            "(id, root_id, grouping_key, title, title_folded, album_artist_name, "
            "album_artist_name_folded, album_artist_id, grouping_source, created_at, updated_at) "
            "VALUES ('album-1', 'root-1', 'group-1', 'Album', 'album', 'Artist', "
            "'artist', 'artist-1', 'automatic', 1, 1)"
        )
        connection.execute(
            "INSERT OR IGNORE INTO local_tracks "
            "(id, local_album_id, root_id, file_path, relative_path, path_hash, "
            "file_size_bytes, file_mtime_ns, stat_revision, stat_revision_kind, "
            "tag_revision, title, title_folded, artist_name, artist_name_folded, "
            "album_title, album_title_folded, album_artist_name, "
            "album_artist_name_folded, disc_number, track_number, file_format, "
            "ingest_source, imported_at, membership_source) "
            "VALUES ('track-1', 'album-1', 'root-1', '/music/track.flac', "
            "'track.flac', 'path-hash', 100, 10, 'stat-1', 'exact', 'tag-1', "
            "'Track', 'track', 'Artist', 'artist', 'Album', 'album', 'Artist', "
            "'artist', 1, 1, 'flac', 'scan', 1, 'automatic')"
        )


@pytest.mark.asyncio
async def test_management_identity_snapshot_retains_selected_edition_and_mappings(
    store: NativeLibraryStore, db_path: Path
) -> None:
    with sqlite3.connect(db_path) as connection:
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, release_mbid, "
            "decision_source, selected_at, row_revision) "
            "VALUES ('album-1', 'musicbrainz', 'release-group-1', 'release-1', "
            "'manual', 2, 4)"
        )
        connection.execute(
            "INSERT INTO local_track_external_identities "
            "(local_track_id, provider, recording_mbid, release_mbid, "
            "release_track_mbid, medium_position, release_track_position, "
            "decision_source, selected_at, row_revision) "
            "VALUES ('track-1', 'musicbrainz', 'recording-1', 'release-1', "
            "'release-track-1', 2, 7, 'manual', 2, 5)"
        )

    identity = await store.get_accepted_library_management_identity(
        "album-1", local_track_ids=("track-1",)
    )

    assert identity is not None
    assert identity.release_group_mbid == "release-group-1"
    assert identity.release_mbid == "release-1"
    assert identity.identity_revision == 4
    assert identity.tracks[0].release_track_mbid == "release-track-1"
    assert identity.tracks[0].recording_mbid == "recording-1"
    assert identity.tracks[0].medium_position == 2
    assert identity.tracks[0].release_track_position == 7
    assert identity.tracks[0].identity_revision == 5


def _job_snapshot(job_id: str = "management-1") -> LibraryManagementJobSnapshot:
    return LibraryManagementJobSnapshot(
        job_id=job_id,
        mode="preview",
        origin="manual",
        phase="planning",
        selection_json='{"track_ids":["track-1"]}',
        profile_revision="profile-1",
        settings_revision="settings-1",
        naming_revision="naming-1",
        policy_revision="policy-1",
        catalog_revision=0,
        profile_snapshot_json="{}",
        created_at=10,
        updated_at=10,
    )


def _plan_item(job_id: str, ordinal: int) -> LibraryManagementPlanItem:
    return LibraryManagementPlanItem(
        job_id=job_id,
        ordinal=ordinal,
        bundle_ordinal=0,
        local_album_id="album-1",
        local_track_id="track-1",
        expected_album_revision=1,
        expected_track_revision=1,
        expected_catalog_revision=0,
        expected_policy_revision="policy-1",
        expected_profile_revision="profile-1",
        expected_root_id="root-1",
        expected_relative_path="track.flac",
        expected_stat_revision="stat-1",
        expected_tag_revision="tag-1",
        expected_file_fingerprint="fingerprint-1",
        source_path_identity="source-1",
        destination_root_id="root-1",
        destination_relative_path=f"organized/{ordinal}.flac",
        desired_document_json="{}",
        desired_document_hash=hashlib.sha256(b"{}").hexdigest(),
        eligibility="eligible",
        created_at=11 + ordinal,
    )


@pytest.mark.asyncio
async def test_management_history_filters_source_or_destination_root(
    store: NativeLibraryStore,
) -> None:
    for job_id, source_root, destination_root in (
        ("history-root-match", "root-1", "archive-root"),
        ("history-root-other", "other-root", "other-root"),
    ):
        await store.create_library_management_job(
            OperationJob(
                id=job_id,
                kind="library_management",
                requested_by_user_id="admin",
                input_catalog_revision=0,
                created_at=10,
            ),
            _job_snapshot(job_id),
        )
        await store.append_library_management_plan_items(
            job_id,
            [
                msgspec.structs.replace(
                    _plan_item(job_id, 0),
                    expected_root_id=source_root,
                    destination_root_id=destination_root,
                )
            ],
            expected_snapshot_revision=1,
        )

    source_matches = await store.list_library_management_operations(
        limit=10, root_id="root-1"
    )
    destination_matches = await store.list_library_management_operations(
        limit=10, root_id="archive-root"
    )

    assert [row["id"] for row in source_matches] == ["history-root-match"]
    assert [row["id"] for row in destination_matches] == ["history-root-match"]


@pytest.mark.asyncio
async def test_ready_management_preview_becomes_exact_idempotent_apply(
    store: NativeLibraryStore, db_path: Path
) -> None:
    token_hash = hashlib.sha256(b"preview-token").hexdigest()
    snapshot = msgspec.structs.replace(
        _job_snapshot("management-apply"),
        preview_token_hash=token_hash,
        preview_expires_at=100,
    )
    await store.create_library_management_job(
        OperationJob(
            id="management-apply",
            kind="library_management",
            input_catalog_revision=0,
            created_at=10,
        ),
        snapshot,
    )
    await store.append_library_management_plan_items(
        "management-apply",
        [
            _plan_item("management-apply", 0),
            msgspec.structs.replace(
                _plan_item("management-apply", 1),
                bundle_ordinal=1,
                eligibility="blocked",
                reason_code="FORMAT_UNSUPPORTED",
            ),
        ],
        expected_snapshot_revision=1,
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_management_job_snapshots SET phase='ready' "
            "WHERE job_id='management-apply'"
        )
        connection.execute(
            "UPDATE library_operation_jobs SET state='ready' "
            "WHERE id='management-apply'"
        )

    started = await store.begin_library_management_apply(
        "management-apply",
        preview_token_hash=token_hash,
        expected_job_revision=1,
        idempotency_key="apply-once",
        now=20,
    )
    repeated = await store.begin_library_management_apply(
        "management-apply",
        preview_token_hash=token_hash,
        expected_job_revision=999,
        idempotency_key="apply-once",
        now=21,
    )
    saved = await store.get_library_management_job_snapshot("management-apply")
    eligible_work = await store.get_operation_work_item("management-apply", 0)
    blocked_work = await store.get_operation_work_item("management-apply", 1)

    assert started["state"] == "queued"
    assert started["expected_work_count"] == 1
    assert repeated["row_revision"] == started["row_revision"]
    assert saved is not None and saved.mode == "apply" and saved.phase == "applying"
    assert saved.apply_idempotency_key == "apply-once"
    assert eligible_work is not None and eligible_work["state"] == "pending"
    assert blocked_work is None


@pytest.mark.asyncio
async def test_management_apply_rejects_token_expiry_and_catalog_staleness(
    store: NativeLibraryStore, db_path: Path
) -> None:
    token_hash = hashlib.sha256(b"preview-token").hexdigest()
    for job_id, expires_at in (("expired-apply", 19), ("stale-apply", 100)):
        await store.create_library_management_job(
            OperationJob(
                id=job_id,
                kind="library_management",
                input_catalog_revision=0,
                created_at=10,
            ),
            msgspec.structs.replace(
                _job_snapshot(job_id),
                preview_token_hash=token_hash,
                preview_expires_at=expires_at,
            ),
        )
        await store.append_library_management_plan_items(
            job_id, [_plan_item(job_id, 0)], expected_snapshot_revision=1
        )
        with sqlite3.connect(db_path) as connection:
            connection.execute(
                "UPDATE library_management_job_snapshots SET phase='ready' WHERE job_id=?",
                (job_id,),
            )
            connection.execute(
                "UPDATE library_operation_jobs SET state='ready' WHERE id=?",
                (job_id,),
            )

    with pytest.raises(StaleRevisionError, match="expired"):
        await store.begin_library_management_apply(
            "expired-apply",
            preview_token_hash=token_hash,
            expected_job_revision=1,
            idempotency_key="expired-key",
            now=20,
        )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_catalog_revision SET value=value+1 WHERE singleton=1"
        )
    with pytest.raises(StaleRevisionError, match="catalog changed"):
        await store.begin_library_management_apply(
            "stale-apply",
            preview_token_hash=token_hash,
            expected_job_revision=1,
            idempotency_key="stale-key",
            now=20,
        )


def test_schema_ratchet_is_idempotent_and_has_no_management_side_effects(
    db_path: Path,
) -> None:
    NativeLibraryStore(db_path, threading.Lock())
    NativeLibraryStore(db_path, threading.Lock())

    with sqlite3.connect(db_path) as connection:
        tables = {
            str(row[0])
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table'"
            )
        }
        indexes = {
            str(row[0])
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type = 'index'"
            )
        }
        foreign_key_errors = connection.execute("PRAGMA foreign_key_check").fetchall()
        operation_count = connection.execute(
            "SELECT COUNT(*) FROM library_operation_jobs "
            "WHERE kind = 'library_management'"
        ).fetchone()[0]
        baseline_count = connection.execute(
            "SELECT COUNT(*) FROM library_management_baselines"
        ).fetchone()[0]
        management_job_columns = {
            str(row[1])
            for row in connection.execute(
                "PRAGMA table_info(library_management_job_snapshots)"
            )
        }
        journal_columns = {
            str(row[1])
            for row in connection.execute(
                "PRAGMA table_info(library_file_mutation_journal)"
            )
        }

    assert {
        "local_track_genres",
        "library_management_blobs",
        "library_management_baselines",
        "library_management_metadata_snapshots",
        "library_management_job_snapshots",
        "library_management_plan_items",
        "library_operation_control_idempotency",
        "library_file_mutation_journal",
        "library_management_collision_evidence",
    }.issubset(tables)
    assert {
        "idx_management_plan_cursor",
        "idx_management_plan_destination",
        "idx_management_apply_idempotency",
        "idx_management_journal_recovery",
        "idx_local_track_identity_release_track",
    }.issubset(indexes)
    assert foreign_key_errors == []
    assert operation_count == 0
    assert baseline_count == 0
    assert "proposed_settings_revision" in management_job_columns
    assert "apply_idempotency_key" in management_job_columns
    assert "recovery_evidence_json" in journal_columns


def test_management_preview_proposed_revision_ratchet_is_idempotent(
    db_path: Path,
) -> None:
    NativeLibraryStore(db_path, threading.Lock())
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "ALTER TABLE library_management_job_snapshots "
            "DROP COLUMN proposed_settings_revision"
        )

    NativeLibraryStore(db_path, threading.Lock())
    NativeLibraryStore(db_path, threading.Lock())

    with sqlite3.connect(db_path) as connection:
        columns = {
            str(row[1])
            for row in connection.execute(
                "PRAGMA table_info(library_management_job_snapshots)"
            )
        }
    assert "proposed_settings_revision" in columns


def test_management_recovery_evidence_ratchet_is_idempotent(
    db_path: Path,
) -> None:
    NativeLibraryStore(db_path, threading.Lock())
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "ALTER TABLE library_file_mutation_journal "
            "DROP COLUMN recovery_evidence_json"
        )

    NativeLibraryStore(db_path, threading.Lock())
    NativeLibraryStore(db_path, threading.Lock())

    with sqlite3.connect(db_path) as connection:
        columns = {
            str(row[1])
            for row in connection.execute(
                "PRAGMA table_info(library_file_mutation_journal)"
            )
        }
    assert "recovery_evidence_json" in columns


def _downgrade_operation_kind_check(path: Path) -> None:
    with sqlite3.connect(path) as connection:
        connection.execute("PRAGMA foreign_keys=OFF")
        connection.execute(
            """
            CREATE TABLE library_operation_jobs__old (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL CHECK(kind IN (
                    'bulk_review_apply','repair','explicit_reidentification'
                )),
                state TEXT NOT NULL CHECK(state IN (
                    'queued','running','paused','ready','succeeded','failed',
                    'cancelled','stopped'
                )),
                requested_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
                input_catalog_revision INTEGER
                    CHECK(input_catalog_revision BETWEEN 0 AND 9223372036854775807),
                expected_work_count INTEGER NOT NULL DEFAULT 0 CHECK(expected_work_count >= 0),
                completed_count INTEGER NOT NULL DEFAULT 0 CHECK(completed_count >= 0),
                succeeded_count INTEGER NOT NULL DEFAULT 0 CHECK(succeeded_count >= 0),
                failed_count INTEGER NOT NULL DEFAULT 0 CHECK(failed_count >= 0),
                skipped_count INTEGER NOT NULL DEFAULT 0 CHECK(skipped_count >= 0),
                control_request TEXT NOT NULL DEFAULT 'none'
                    CHECK(control_request IN ('none','pause','stop')),
                terminal_code TEXT,
                idempotency_key TEXT UNIQUE,
                lease_owner TEXT,
                lease_expires_at REAL,
                heartbeat_at REAL,
                created_at REAL NOT NULL,
                started_at REAL,
                phase_started_at REAL,
                phase_timings_json TEXT NOT NULL DEFAULT '{}',
                updated_at REAL NOT NULL,
                terminal_at REAL,
                row_revision INTEGER NOT NULL DEFAULT 1
                    CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
                event_revision INTEGER NOT NULL DEFAULT 0
                    CHECK(event_revision BETWEEN 0 AND 9223372036854775807)
            )
            """
        )
        connection.execute(
            "INSERT INTO library_operation_jobs__old "
            "SELECT * FROM library_operation_jobs"
        )
        connection.execute("DROP TABLE library_operation_jobs")
        connection.execute(
            "ALTER TABLE library_operation_jobs__old RENAME TO library_operation_jobs"
        )


def test_exact_old_operation_constraint_is_rebuilt_without_losing_children(
    db_path: Path,
) -> None:
    NativeLibraryStore(db_path, threading.Lock())
    _seed_catalog(db_path)
    with sqlite3.connect(db_path) as connection:
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute(
            "INSERT INTO library_operation_jobs "
            "(id, kind, state, expected_work_count, created_at, updated_at) "
            "VALUES ('repair-1', 'repair', 'queued', 1, 2, 2)"
        )
        connection.execute(
            "INSERT INTO library_operation_work "
            "(job_id, ordinal, local_track_id, expected_subject_revision, "
            "expected_input_revision, action, idempotency_key, updated_at) "
            "VALUES ('repair-1', 0, 'track-1', 1, 'input-1', 'repair', "
            "'repair-1:track-1', 2)"
        )
        connection.execute(
            "INSERT INTO library_repair_snapshots "
            "(job_id, scope_json, target_matcher_version, created_at) "
            "VALUES ('repair-1', '{}', 'matcher-1', 2)"
        )
    _downgrade_operation_kind_check(db_path)

    NativeLibraryStore(db_path, threading.Lock())
    NativeLibraryStore(db_path, threading.Lock())

    with sqlite3.connect(db_path) as connection:
        connection.execute("PRAGMA foreign_keys=ON")
        table_sql = str(
            connection.execute(
                "SELECT sql FROM sqlite_master WHERE type = 'table' "
                "AND name = 'library_operation_jobs'"
            ).fetchone()[0]
        )
        old_job = connection.execute(
            "SELECT kind FROM library_operation_jobs WHERE id = 'repair-1'"
        ).fetchone()
        old_work = connection.execute(
            "SELECT action FROM library_operation_work "
            "WHERE job_id = 'repair-1' AND ordinal = 0"
        ).fetchone()
        old_snapshot = connection.execute(
            "SELECT target_matcher_version FROM library_repair_snapshots "
            "WHERE job_id = 'repair-1'"
        ).fetchone()
        connection.execute(
            "INSERT INTO library_operation_jobs "
            "(id, kind, state, created_at, updated_at) "
            "VALUES ('management-1', 'library_management', 'queued', 3, 3)"
        )
        with pytest.raises(sqlite3.IntegrityError):
            connection.execute(
                "INSERT INTO library_operation_jobs "
                "(id, kind, state, created_at, updated_at) "
                "VALUES ('unknown-1', 'unknown', 'queued', 3, 3)"
            )
        foreign_key_errors = connection.execute("PRAGMA foreign_key_check").fetchall()

    assert "library_management" in table_sql
    assert old_job == ("repair",)
    assert old_work == ("repair",)
    assert old_snapshot == ("matcher-1",)
    assert foreign_key_errors == []


@pytest.mark.asyncio
async def test_release_track_identity_is_nullable_then_persisted_without_inference(
    store: NativeLibraryStore,
) -> None:
    await store.attach_track_identity(
        LocalTrackExternalIdentity(
            local_track_id="track-1",
            recording_mbid="recording-1",
            release_mbid="release-1",
            decision_source="manual",
            selected_at=2,
        ),
        expected_track_revision=1,
    )
    legacy = await store.get_target_track("track-1")

    await store.attach_track_identity(
        LocalTrackExternalIdentity(
            local_track_id="track-1",
            recording_mbid="recording-1",
            release_mbid="release-1",
            release_track_mbid="release-track-1",
            medium_position=2,
            release_track_position=3,
            decision_source="manual",
            selected_at=3,
        ),
        expected_track_revision=2,
    )
    verified = await store.get_target_track("track-1")

    assert legacy is not None
    assert legacy["release_track_mbid"] is None
    assert verified is not None
    assert verified["recording_mbid"] == "recording-1"
    assert verified["release_track_mbid"] == "release-track-1"
    assert verified["medium_position"] == 2
    assert verified["release_track_position"] == 3


@pytest.mark.asyncio
async def test_metadata_snapshots_are_hash_verified_deduplicated_and_immutable(
    store: NativeLibraryStore, db_path: Path
) -> None:
    payload = '{"id":"release-1"}'
    payload_hash = hashlib.sha256(payload.encode()).hexdigest()
    snapshot = LibraryManagementMetadataSnapshot(
        id="metadata-1",
        provider="musicbrainz",
        entity_kind="release",
        entity_id="release-1",
        input_hash="a" * 64,
        canonical_payload_json=payload,
        payload_sha256=payload_hash,
        fetched_at=2,
    )

    created = await store.put_management_metadata_snapshot(snapshot)
    deduplicated = await store.put_management_metadata_snapshot(
        LibraryManagementMetadataSnapshot(
            id="metadata-2",
            provider=snapshot.provider,
            entity_kind=snapshot.entity_kind,
            entity_id=snapshot.entity_id,
            input_hash=snapshot.input_hash,
            canonical_payload_json=payload,
            payload_sha256=payload_hash,
            fetched_at=3,
        )
    )
    with pytest.raises(ValidationError):
        await store.put_management_metadata_snapshot(
            LibraryManagementMetadataSnapshot(
                id="bad",
                provider="musicbrainz",
                entity_kind="release",
                entity_id="release-2",
                input_hash="b" * 64,
                canonical_payload_json="{}",
                payload_sha256="c" * 64,
                fetched_at=4,
            )
        )
    with sqlite3.connect(db_path) as connection:
        with pytest.raises(sqlite3.IntegrityError):
            connection.execute(
                "UPDATE library_management_metadata_snapshots "
                "SET canonical_payload_json = '{}' WHERE id = 'metadata-1'"
            )

    assert created.id == "metadata-1"
    assert deduplicated.id == "metadata-1"


@pytest.mark.asyncio
async def test_first_management_baseline_is_single_and_keeps_blob_referenced(
    store: NativeLibraryStore,
) -> None:
    blob = LibraryManagementBlob(
        sha256="a" * 64,
        kind="tag_snapshot",
        byte_length=4,
        relative_path="aa/" + "a" * 64,
        created_at=2,
    )
    await store.register_management_blob(blob)
    first = LibraryManagementBaseline(
        id="baseline-1",
        local_track_id="track-1",
        original_root_id="root-1",
        original_relative_path="track.flac",
        format="flac",
        adapter_version="1",
        semantic_snapshot_blob_sha256=blob.sha256,
        stat_revision="stat-1",
        tag_revision="tag-1",
        created_at=3,
    )

    created, was_created = await store.ensure_management_baseline(first)
    existing, second_created = await store.ensure_management_baseline(
        LibraryManagementBaseline(
            id="baseline-2",
            local_track_id="track-1",
            original_root_id="root-2",
            original_relative_path="different.flac",
            format="flac",
            adapter_version="2",
            semantic_snapshot_blob_sha256=blob.sha256,
            stat_revision="stat-2",
            tag_revision="tag-2",
            created_at=4,
        )
    )
    with pytest.raises(ConflictError):
        await store.delete_unreferenced_management_blob(blob.sha256)

    assert was_created is True
    assert second_created is False
    assert created.id == "baseline-1"
    assert existing.id == "baseline-1"
    assert existing.original_relative_path == "track.flac"


@pytest.mark.asyncio
async def test_plan_pagination_and_journal_transitions_are_bounded_and_cas_safe(
    store: NativeLibraryStore,
) -> None:
    job_id = "management-1"
    await store.create_library_management_job(
        OperationJob(
            id=job_id,
            kind="library_management",
            input_catalog_revision=0,
            created_at=10,
        ),
        _job_snapshot(job_id),
    )
    revision = await store.append_library_management_plan_items(
        job_id,
        [
            msgspec.structs.replace(
                _plan_item(job_id, 0),
                diff_json=json.dumps(
                    {
                        "requires_write": True,
                        "tags_changed": True,
                        "field_mutations": [{"operation": "preserve"}],
                    }
                ),
                capability_json=json.dumps(
                    {"audio_format": "flac", "representation_losses": ["joined"]}
                ),
                collision_json=json.dumps(
                    [{"classification": "normalized_path_collision"}]
                ),
            ),
            msgspec.structs.replace(
                _plan_item(job_id, 1),
                eligibility="blocked",
                reason_code="PATH_COLLISION_DIFFERENT",
                diff_json=json.dumps({"requires_write": False}),
                capability_json=json.dumps({"audio_format": "mp3"}),
            ),
        ],
        expected_snapshot_revision=1,
    )
    first_page = await store.list_library_management_plan_items(job_id, limit=1)
    second_page = await store.list_library_management_plan_items(
        job_id, after_ordinal=first_page[-1].ordinal, limit=1
    )
    filtered_tags = await store.list_library_management_plan_items(
        job_id,
        eligibility="eligible",
        root_id="root-1",
        audio_format="flac",
        change_kind="tags",
    )
    filtered_reason = await store.list_library_management_plan_items(
        job_id,
        reason_code="PATH_COLLISION_DIFFERENT",
        change_kind="no_change",
    )
    filtered_facets = await store.list_library_management_plan_items(
        job_id,
        artist_id="artist-1",
        album_id="album-1",
        collision_class="normalized_path_collision",
        has_preserved_value=True,
        has_representation_loss=True,
    )
    with pytest.raises(ValidationError):
        await store.list_library_management_plan_items(
            job_id, limit=MANAGEMENT_PERSISTENCE_BATCH_SIZE + 1
        )
    with pytest.raises(StaleRevisionError):
        await store.append_library_management_plan_items(
            job_id,
            [_plan_item(job_id, 2)],
            expected_snapshot_revision=1,
        )

    journal = LibraryFileMutationJournal(
        id="journal-1",
        job_id=job_id,
        plan_item_ordinal=0,
        subject_kind="audio",
        subject_key="track-1",
        local_track_id="track-1",
        state="planned",
        created_at=20,
        updated_at=20,
    )
    await store.create_file_mutation_journal(journal)
    saved = await store.transition_file_mutation_journal(
        journal.id,
        expected_state="planned",
        new_state="snapshot_saved",
        expected_row_revision=1,
        updated_at=21,
    )
    recoverable = await store.list_recoverable_file_mutation_journals()
    bundles = await store.list_recoverable_management_bundles()
    with pytest.raises(ValidationError):
        await store.transition_file_mutation_journal(
            journal.id,
            expected_state="snapshot_saved",
            new_state="completed",
            expected_row_revision=2,
            updated_at=22,
        )
    with pytest.raises(StaleRevisionError):
        await store.transition_file_mutation_journal(
            journal.id,
            expected_state="snapshot_saved",
            new_state="staged",
            expected_row_revision=1,
            updated_at=22,
        )

    assert revision == 2
    assert [item.ordinal for item in first_page] == [0]
    assert [item.ordinal for item in second_page] == [1]
    assert [item.ordinal for item in filtered_tags] == [0]
    assert [item.ordinal for item in filtered_reason] == [1]
    assert [item.ordinal for item in filtered_facets] == [0]
    assert saved.state == "snapshot_saved"
    assert saved.row_revision == 2
    assert [value.id for value in recoverable] == [journal.id]
    assert [(row["job_id"], row["bundle_ordinal"]) for row in bundles] == [(job_id, 0)]

    attention = await store.transition_file_mutation_journal(
        journal.id,
        expected_state="snapshot_saved",
        new_state="needs_attention",
        expected_row_revision=2,
        updated_at=23,
        failure_code="RECOVERY_DESTINATION_CHANGED",
        increment_attempts=True,
        recovery_evidence_json='{"kind":"unexpected"}',
    )
    diagnostics = await store.library_management_recovery_diagnostics()
    assert attention.attempts == 1
    assert attention.recovery_evidence_json == '{"kind":"unexpected"}'
    assert await store.list_recoverable_file_mutation_journals() == []
    assert diagnostics["recoverable_bundle_count"] == 0
    assert diagnostics["needs_attention_count"] == 1


@pytest.mark.asyncio
async def test_management_selection_pages_are_stable_and_expand_track_albums(
    store: NativeLibraryStore, db_path: Path
) -> None:
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO local_tracks "
            "(id, local_album_id, root_id, file_path, relative_path, path_hash, "
            "file_size_bytes, file_mtime_ns, stat_revision, stat_revision_kind, "
            "tag_revision, title, title_folded, artist_name, artist_name_folded, "
            "album_title, album_title_folded, album_artist_name, "
            "album_artist_name_folded, disc_number, track_number, file_format, "
            "ingest_source, imported_at, membership_source, year, genre, genre_folded) "
            "VALUES ('track-2', 'album-1', 'root-1', '/music/disc/track.flac', "
            "'disc/track.flac', 'path-2', 200, 20, 'stat-2', 'exact', 'tag-2', "
            "'Second Track', 'second track', 'Artist', 'artist', 'Album', 'album', "
            "'Artist', 'artist', 1, 2, 'flac', 'scan', 2, 'automatic', 2024, "
            "'Ambient', 'ambient')"
        )
        connection.execute(
            "INSERT INTO local_track_artists "
            "(local_track_id, position, local_artist_id, role) "
            "VALUES ('track-2', 0, 'artist-1', 'primary')"
        )
        connection.execute(
            "INSERT INTO local_track_genres "
            "(local_track_id, position, name, folded_name, source) "
            "VALUES ('track-2', 0, 'Ambient', 'ambient', 'local')"
        )

    track_selection = NormalizedLibraryManagementSelection(
        kind="tracks", ids=("track-2",), requested_track_ids=("track-2",)
    )
    exact = await store.list_library_management_selection_page(track_selection)
    expanded = await store.list_library_management_selection_page(
        msgspec.structs.replace(track_selection, expand_album_bundles=True), limit=1
    )
    expanded_second = await store.list_library_management_selection_page(
        msgspec.structs.replace(track_selection, expand_album_bundles=True),
        cursor=expanded.next_cursor,
        limit=1,
    )
    root_rule = await store.list_library_management_selection_page(
        NormalizedLibraryManagementSelection(
            kind="roots",
            ids=("rule-1",),
            root_scopes=(
                LibraryManagementRootScope(root_id="root-1", relative_prefix="disc"),
            ),
        )
    )
    artist = await store.list_library_management_selection_page(
        NormalizedLibraryManagementSelection(kind="artists", ids=("artist-1",))
    )
    album = await store.list_library_management_selection_page(
        NormalizedLibraryManagementSelection(kind="albums", ids=("album-1",))
    )
    filtered = await store.list_library_management_selection_page(
        NormalizedLibraryManagementSelection(
            kind="filter",
            catalog_filter=LibraryManagementCatalogFilter(
                search="Second", genre="Ambient", from_year=2024, to_year=2024
            ),
        )
    )

    assert [value.local_track_id for value in exact.subjects] == ["track-2"]
    assert [value.local_track_id for value in expanded.subjects] == ["track-1"]
    assert [value.local_track_id for value in expanded_second.subjects] == ["track-2"]
    assert expanded.subjects[0].bundle_ordinal == 0
    assert expanded_second.subjects[0].bundle_ordinal == 0
    assert [value.local_track_id for value in root_rule.subjects] == ["track-2"]
    assert {value.local_track_id for value in artist.subjects} == {
        "track-1",
        "track-2",
    }
    assert {value.local_track_id for value in album.subjects} == {
        "track-1",
        "track-2",
    }
    assert [value.local_track_id for value in filtered.subjects] == ["track-2"]


@pytest.mark.asyncio
async def test_management_selection_is_bounded_with_one_hundred_thousand_rows(
    store: NativeLibraryStore, db_path: Path
) -> None:
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "WITH RECURSIVE counter(value) AS ("
            "SELECT 2 UNION ALL SELECT value + 1 FROM counter WHERE value < 100000) "
            "INSERT INTO local_tracks "
            "(id, local_album_id, root_id, file_path, relative_path, path_hash, "
            "file_size_bytes, file_mtime_ns, stat_revision, stat_revision_kind, "
            "tag_revision, title, title_folded, album_title, album_title_folded, "
            "disc_number, track_number, file_format, ingest_source, imported_at, "
            "membership_source) SELECT printf('track-%06d', value), 'album-1', "
            "'root-1', printf('/music/%06d.flac', value), "
            "printf('%06d.flac', value), printf('path-%06d', value), 1, value, "
            "printf('stat-%06d', value), 'exact', printf('tag-%06d', value), "
            "printf('Track %06d', value), printf('track %06d', value), "
            "'Album', 'album', 1, value, 'flac', 'scan', value, 'automatic' "
            "FROM counter"
        )

    selection = NormalizedLibraryManagementSelection(kind="albums", ids=("album-1",))
    first = await store.list_library_management_selection_page(selection)
    second = await store.list_library_management_selection_page(
        selection, cursor=first.next_cursor
    )

    assert len(first.subjects) == MANAGEMENT_PERSISTENCE_BATCH_SIZE
    assert len(second.subjects) == MANAGEMENT_PERSISTENCE_BATCH_SIZE
    assert first.next_cursor is not None
    assert first.next_cursor.next_ordinal == MANAGEMENT_PERSISTENCE_BATCH_SIZE
    assert first.subjects[-1].local_track_id < second.subjects[0].local_track_id


@pytest.mark.asyncio
async def test_management_preview_seal_detects_normalized_plan_collisions(
    store: NativeLibraryStore,
) -> None:
    job_id = "management-seal"
    await store.create_library_management_job(
        OperationJob(
            id=job_id,
            kind="library_management",
            input_catalog_revision=0,
            created_at=10,
        ),
        _job_snapshot(job_id),
    )
    first = msgspec.structs.replace(
        _plan_item(job_id, 0),
        destination_collision_key="organized/track.flac",
        diff_json=json.dumps({"requires_write": True, "path_changed": True}),
        capability_json=json.dumps({"audio_format": "flac"}),
    )
    second = msgspec.structs.replace(
        _plan_item(job_id, 1),
        destination_relative_path="ORGANIZED/TRACK.flac",
        destination_collision_key="organized/track.flac",
        diff_json=json.dumps({"requires_write": True, "path_changed": True}),
        capability_json=json.dumps({"audio_format": "flac"}),
    )
    revision = await store.append_library_management_plan_items(
        job_id, [first, second], expected_snapshot_revision=1
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=12, lease_seconds=60, kind="library_management"
    )

    snapshot = await store.finalize_library_management_preview(
        job_id,
        "worker-1",
        expected_snapshot_revision=revision,
        now=13,
    )
    plan = await store.list_library_management_plan_items(job_id)
    operation = await store.get_operation_job(job_id)
    summary = json.loads(snapshot.summary_json)

    assert claimed is not None
    assert snapshot.phase == "ready"
    assert operation is not None and operation["state"] == "ready"
    assert [value.eligibility for value in plan] == ["blocked", "blocked"]
    assert {value.reason_code for value in plan} == {"PATH_COLLISION_DIFFERENT"}
    assert summary["blocked_count"] == 2
    assert summary["bundle_count"] == 1


@pytest.mark.asyncio
async def test_management_preview_seal_rejects_a_changed_catalog(
    store: NativeLibraryStore, db_path: Path
) -> None:
    job_id = "management-stale-catalog"
    await store.create_library_management_job(
        OperationJob(
            id=job_id,
            kind="library_management",
            input_catalog_revision=0,
            created_at=10,
        ),
        _job_snapshot(job_id),
    )
    revision = await store.append_library_management_plan_items(
        job_id, [_plan_item(job_id, 0)], expected_snapshot_revision=1
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=12, lease_seconds=60, kind="library_management"
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_catalog_revision SET value = value + 1 WHERE singleton = 1"
        )

    with pytest.raises(StaleRevisionError, match="catalog changed"):
        await store.finalize_library_management_preview(
            job_id,
            "worker-1",
            expected_snapshot_revision=revision,
            now=13,
        )

    snapshot = await store.get_library_management_job_snapshot(job_id)
    operation = await store.get_operation_job(job_id)
    assert claimed is not None
    assert snapshot is not None and snapshot.phase == "planning"
    assert operation is not None and operation["state"] == "running"


@pytest.mark.asyncio
async def test_journal_revision_overflow_is_refused(
    store: NativeLibraryStore, db_path: Path
) -> None:
    job_id = "management-overflow"
    await store.create_library_management_job(
        OperationJob(
            id=job_id,
            kind="library_management",
            input_catalog_revision=0,
            created_at=10,
        ),
        _job_snapshot(job_id),
    )
    await store.append_library_management_plan_items(
        job_id, [_plan_item(job_id, 0)], expected_snapshot_revision=1
    )
    await store.create_file_mutation_journal(
        LibraryFileMutationJournal(
            id="journal-overflow",
            job_id=job_id,
            plan_item_ordinal=0,
            subject_kind="audio",
            subject_key="track-1",
            local_track_id="track-1",
            state="planned",
            created_at=20,
            updated_at=20,
        )
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_file_mutation_journal SET row_revision = ? "
            "WHERE id = 'journal-overflow'",
            (MAX_REVISION,),
        )

    with pytest.raises(RevisionOverflowError):
        await store.transition_file_mutation_journal(
            "journal-overflow",
            expected_state="planned",
            new_state="snapshot_saved",
            expected_row_revision=MAX_REVISION,
            updated_at=21,
        )


async def _seed_published_management_bundle(
    store: NativeLibraryStore,
    db_path: Path,
    *,
    job_id: str,
    tag_edit_intent: LibraryManagementTagEditIntent | None = None,
) -> LibraryManagementCatalogMutation:
    snapshot = msgspec.structs.replace(
        _job_snapshot(job_id),
        intent_json=(
            json.dumps(
                msgspec.to_builtins(tag_edit_intent),
                separators=(",", ":"),
                sort_keys=True,
            )
            if tag_edit_intent is not None
            else "{}"
        ),
    )
    await store.create_library_management_job(
        OperationJob(
            id=job_id,
            kind="library_management",
            requested_by_user_id="admin",
            input_catalog_revision=0,
            expected_work_count=1,
            created_at=10,
        ),
        snapshot,
    )
    item = msgspec.structs.replace(
        _plan_item(job_id, 0),
        local_album_id="album-1",
        expected_album_revision=1,
    )
    await store.append_library_management_plan_items(
        job_id, [item], expected_snapshot_revision=1
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT OR IGNORE INTO local_album_external_identities "
            "(local_album_id,provider,release_group_mbid,release_mbid,"
            "decision_source,selected_at) VALUES "
            "('album-1','musicbrainz','release-group-1','release-1','manual',20)"
        )
        connection.execute(
            "INSERT OR IGNORE INTO local_track_external_identities "
            "(local_track_id,provider,recording_mbid,release_mbid,release_track_mbid,"
            "medium_position,release_track_position,decision_source,selected_at) VALUES "
            "('track-1','musicbrainz','recording-1','release-1','release-track-1',"
            "1,1,'manual',20)"
        )
        connection.execute(
            "UPDATE library_operation_jobs SET state='running',lease_owner='worker-1' "
            "WHERE id=?",
            (job_id,),
        )
        connection.execute(
            "UPDATE library_management_job_snapshots "
            "SET mode='apply',phase='applying' WHERE job_id=?",
            (job_id,),
        )
        connection.execute(
            "INSERT INTO library_operation_work "
            "(job_id,ordinal,local_album_id,expected_subject_revision,"
            "expected_input_revision,action,idempotency_key,state,updated_at) "
            "VALUES (?,0,'album-1',1,'profile-1','library_management',?,'running',20)",
            (job_id, f"{job_id}:bundle:0"),
        )
    blob_hash = hashlib.sha256(f"{job_id}:snapshot".encode()).hexdigest()
    await store.register_management_blob(
        LibraryManagementBlob(
            sha256=blob_hash,
            kind="tag_snapshot",
            byte_length=10,
            relative_path=f"objects/{blob_hash[:2]}/{blob_hash[2:4]}/{blob_hash}",
            created_at=20,
        )
    )
    baseline, _ = await store.ensure_management_baseline(
        LibraryManagementBaseline(
            id=f"{job_id}-baseline",
            local_track_id="track-1",
            original_root_id="root-1",
            original_relative_path="track.flac",
            format="flac",
            adapter_version="1",
            semantic_snapshot_blob_sha256=blob_hash,
            stat_revision="stat-1",
            tag_revision="tag-1",
            created_at=20,
        )
    )
    operation_snapshot = LibraryManagementOperationSnapshot(
        id=f"{job_id}-snapshot",
        job_id=job_id,
        work_ordinal=0,
        local_track_id="track-1",
        before_root_id="root-1",
        before_relative_path="track.flac",
        after_root_id="root-1",
        after_relative_path="organized/track.flac",
        format="flac",
        adapter_version="1",
        semantic_snapshot_blob_sha256=blob_hash,
        source_fingerprint="source-hash",
        created_at=20,
        expires_at=30,
    )
    await store.ensure_management_operation_snapshot(operation_snapshot)
    await store.ensure_file_mutation_journal(
        LibraryFileMutationJournal(
            id=f"{job_id}-journal",
            job_id=job_id,
            plan_item_ordinal=0,
            subject_kind="audio",
            subject_key="track-1",
            local_track_id="track-1",
            source_root_id="root-1",
            source_relative_path="track.flac",
            destination_root_id="root-1",
            destination_relative_path="organized/track.flac",
            source_fingerprint="source-hash",
            staged_fingerprint="published-hash",
            baseline_id=baseline.id,
            operation_snapshot_id=operation_snapshot.id,
            state="published",
            created_at=20,
            updated_at=21,
        )
    )
    return LibraryManagementCatalogMutation(
        journal_id=f"{job_id}-journal",
        plan_item_ordinal=0,
        local_track_id="track-1",
        local_album_id="album-1",
        expected_album_revision=1,
        expected_track_revision=1,
        expected_root_id="root-1",
        expected_relative_path="track.flac",
        expected_stat_revision="stat-1",
        expected_tag_revision="tag-1",
        expected_identity_revision=1,
        expected_album_identity_revision=1,
        expected_override_revision=hashlib.sha256(
            (
                hashlib.sha256(b"[]").hexdigest()
                + "\x00"
                + hashlib.sha256(b"[]").hexdigest()
            ).encode()
        ).hexdigest(),
        expected_release_mbid="release-1",
        expected_recording_mbid="recording-1",
        expected_release_track_mbid="release-track-1",
        destination_root_id="root-1",
        destination_relative_path="organized/track.flac",
        destination_file_path="/music/organized/track.flac",
        destination_path_hash="destination-hash",
        file_size_bytes=120,
        file_mtime_ns=30,
        stat_revision="stat-2",
        tag_revision="tag-2",
        file_fingerprint="published-hash",
        tag=AudioTag(
            title="Managed Track",
            artist="Managed Artist",
            album="Managed Album",
            album_artist="Managed Artist",
            track_number=1,
            disc_number=1,
            year=2026,
            genre="Rock",
            genres=["Rock", "Alternative"],
        ),
        info=AudioInfo(
            duration_seconds=180,
            bitrate=900,
            sample_rate=44_100,
            channels=2,
            file_format="flac",
            file_size_bytes=120,
            bit_depth=16,
        ),
        baseline_id=baseline.id,
        operation_snapshot_id=operation_snapshot.id,
        applied_profile_id="profile-1",
        applied_profile_revision="profile-revision-1",
        applied_projection_hash="projection-1",
        applied_naming_script_revision="naming-1",
    )


@pytest.mark.asyncio
async def test_management_bundle_catalog_commit_is_atomic_and_idempotent(
    store: NativeLibraryStore, db_path: Path
) -> None:
    mutation = await _seed_published_management_bundle(
        store, db_path, job_id="management-commit"
    )

    result = await store.commit_library_management_bundle(
        "management-commit", 0, "worker-1", [mutation], now=30
    )
    repeated = await store.commit_library_management_bundle(
        "management-commit", 0, "worker-1", [mutation], now=31
    )

    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        track = connection.execute(
            "SELECT * FROM local_tracks WHERE id='track-1'"
        ).fetchone()
        album = connection.execute(
            "SELECT * FROM local_albums WHERE id='album-1'"
        ).fetchone()
        genres = connection.execute(
            "SELECT name FROM local_track_genres WHERE local_track_id='track-1' "
            "ORDER BY position"
        ).fetchall()
        track_credits = connection.execute(
            "SELECT artist.display_name,credit.credited_name "
            "FROM local_track_artists credit JOIN local_artists artist "
            "ON artist.id=credit.local_artist_id "
            "WHERE credit.local_track_id='track-1' ORDER BY credit.position"
        ).fetchall()
        album_credits = connection.execute(
            "SELECT artist.display_name,credit.credited_name "
            "FROM local_album_artists credit JOIN local_artists artist "
            "ON artist.id=credit.local_artist_id "
            "WHERE credit.local_album_id='album-1' ORDER BY credit.position"
        ).fetchall()
        state = connection.execute(
            "SELECT * FROM library_track_management_state WHERE local_track_id='track-1'"
        ).fetchone()
        journal = connection.execute(
            "SELECT state FROM library_file_mutation_journal "
            "WHERE id='management-commit-journal'"
        ).fetchone()
        job = connection.execute(
            "SELECT completed_count,succeeded_count FROM library_operation_jobs "
            "WHERE id='management-commit'"
        ).fetchone()
        actions = connection.execute(
            "SELECT COUNT(*) FROM library_catalog_actions "
            "WHERE operation_job_id='management-commit'"
        ).fetchone()[0]

    assert result.catalog_revision == repeated.catalog_revision == 1
    assert track["relative_path"] == "organized/track.flac"
    assert track["title"] == "Managed Track"
    assert album["title"] == "Managed Album"
    assert [row["name"] for row in genres] == ["Rock", "Alternative"]
    assert [tuple(row) for row in track_credits] == [
        ("Managed Artist", "Managed Artist")
    ]
    assert [tuple(row) for row in album_credits] == [
        ("Managed Artist", "Managed Artist")
    ]
    assert state["baseline_id"] == "management-commit-baseline"
    assert state["last_outcome"] == "succeeded"
    assert journal["state"] == "catalog_committed"
    assert tuple(job) == (1, 1)
    assert actions == 1


@pytest.mark.asyncio
async def test_operation_recovery_availability_uses_durable_snapshot_expiry(
    store: NativeLibraryStore, db_path: Path
) -> None:
    mutation = await _seed_published_management_bundle(
        store, db_path, job_id="management-recovery-availability"
    )
    await store.commit_library_management_bundle(
        "management-recovery-availability", 0, "worker-1", [mutation], now=25
    )

    available = await store.get_management_operation_recovery_availability(
        "management-recovery-availability", now=29
    )
    expired = await store.get_management_operation_recovery_availability(
        "management-recovery-availability", now=30
    )

    assert available == {
        "snapshot_count": 1,
        "undo_available_count": 1,
        "undo_expired_count": 0,
        "undo_expires_at": 30.0,
        "baseline_available_count": 1,
    }
    assert expired == {
        "snapshot_count": 1,
        "undo_available_count": 0,
        "undo_expired_count": 1,
        "undo_expires_at": None,
        "baseline_available_count": 1,
    }


@pytest.mark.asyncio
async def test_manual_tag_override_commits_with_catalog_and_journal(
    store: NativeLibraryStore, db_path: Path
) -> None:
    intent = LibraryManagementTagEditIntent(
        local_track_id="track-1",
        local_album_id="album-1",
        mode="save_override",
        fields=[
            LibraryManagementTagEditFieldIntent(
                field_name="genre",
                subject_kind="track",
                value=("Rock", "Alternative"),
            )
        ],
    )
    mutation = await _seed_published_management_bundle(
        store,
        db_path,
        job_id="management-tag-override",
        tag_edit_intent=intent,
    )

    await store.commit_library_management_bundle(
        "management-tag-override", 0, "worker-1", [mutation], now=30
    )

    overrides, track_revision = await store.list_management_overrides(
        subject_kind="track", subject_id="track-1"
    )
    state = await store.get_track_management_state("track-1")
    assert len(overrides) == 1
    assert overrides[0].field_name == "genre"
    assert overrides[0].value_json == '["Rock","Alternative"]'
    assert overrides[0].actor_user_id == "admin"
    empty_album_revision = hashlib.sha256(b"[]").hexdigest()
    combined = hashlib.sha256(
        f"{empty_album_revision}\x00{track_revision}".encode()
    ).hexdigest()
    assert state is not None and state.applied_override_revision == combined


@pytest.mark.asyncio
async def test_reset_to_canonical_deletes_exact_override_in_same_commit(
    store: NativeLibraryStore, db_path: Path
) -> None:
    existing = await store.save_management_override(
        LibraryManagementOverride(
            id="override-title",
            subject_kind="track",
            local_track_id="track-1",
            field_name="title",
            value_json='"Local title"',
            mode="replace",
            actor_user_id="admin",
            subject_revision=1,
            created_at=5,
            updated_at=5,
        ),
        expected_row_revision=None,
    )
    intent = LibraryManagementTagEditIntent(
        local_track_id="track-1",
        local_album_id="album-1",
        mode="reset_canonical",
        fields=[
            LibraryManagementTagEditFieldIntent(
                field_name="title",
                subject_kind="track",
                override_id=existing.id,
                expected_override_row_revision=existing.row_revision,
            )
        ],
    )
    mutation = await _seed_published_management_bundle(
        store,
        db_path,
        job_id="management-tag-reset",
        tag_edit_intent=intent,
    )
    _overrides, expected_revision = await store.list_management_overrides(
        subject_kind="track", subject_id="track-1"
    )
    mutation = msgspec.structs.replace(
        mutation,
        expected_override_revision=hashlib.sha256(
            (hashlib.sha256(b"[]").hexdigest() + "\x00" + expected_revision).encode()
        ).hexdigest(),
    )

    await store.commit_library_management_bundle(
        "management-tag-reset", 0, "worker-1", [mutation], now=30
    )

    assert await store.get_management_override(existing.id) is None
    state = await store.get_track_management_state("track-1")
    _remaining, empty_track_revision = await store.list_management_overrides(
        subject_kind="track", subject_id="track-1"
    )
    empty_album_revision = hashlib.sha256(b"[]").hexdigest()
    combined = hashlib.sha256(
        f"{empty_album_revision}\x00{empty_track_revision}".encode()
    ).hexdigest()
    assert state is not None and state.applied_override_revision == combined


@pytest.mark.asyncio
async def test_management_bundle_catalog_cas_rolls_back_every_database_change(
    store: NativeLibraryStore, db_path: Path
) -> None:
    mutation = await _seed_published_management_bundle(
        store, db_path, job_id="management-stale-catalog"
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_catalog_revision SET value=1 WHERE singleton=1"
        )

    with pytest.raises(StaleRevisionError):
        await store.commit_library_management_bundle(
            "management-stale-catalog", 0, "worker-1", [mutation], now=30
        )

    with sqlite3.connect(db_path) as connection:
        track = connection.execute(
            "SELECT relative_path,title FROM local_tracks WHERE id='track-1'"
        ).fetchone()
        journal = connection.execute(
            "SELECT state FROM library_file_mutation_journal "
            "WHERE id='management-stale-catalog-journal'"
        ).fetchone()
        actions = connection.execute(
            "SELECT COUNT(*) FROM library_catalog_actions "
            "WHERE operation_job_id='management-stale-catalog'"
        ).fetchone()[0]

    assert track == ("track.flac", "Track")
    assert journal == ("published",)
    assert actions == 0


@pytest.mark.asyncio
async def test_management_bundle_commit_requires_every_audio_journal(
    store: NativeLibraryStore, db_path: Path
) -> None:
    mutation = await _seed_published_management_bundle(
        store, db_path, job_id="management-missing-mutation"
    )
    await store.ensure_file_mutation_journal(
        LibraryFileMutationJournal(
            id="management-missing-mutation-second-journal",
            job_id="management-missing-mutation",
            plan_item_ordinal=0,
            subject_kind="audio",
            subject_key="unexpected-second-audio",
            local_track_id="track-1",
            source_root_id="root-1",
            source_relative_path="track.flac",
            destination_root_id="root-1",
            destination_relative_path="organized/track.flac",
            source_fingerprint="source-hash",
            state="published",
            created_at=20,
            updated_at=21,
        )
    )

    with pytest.raises(ValidationError, match="Every audio journal"):
        await store.commit_library_management_bundle(
            "management-missing-mutation", 0, "worker-1", [mutation], now=30
        )
