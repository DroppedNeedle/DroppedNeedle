import hashlib
import logging
import sqlite3
import threading
from pathlib import Path

import pytest

from api.v1.schemas.library_policies import LibraryRootSettings, TypedLibrarySettings
from core.dependencies import cache_providers, service_providers
from core.exceptions import (
    StaleRevisionError,
    TargetStartupInvariantError,
    ValidationError,
)
from infrastructure.persistence.native_library_store import (
    VARIOUS_ARTISTS_ID,
    NativeLibraryStore,
)
from models.local_catalog import LocalArtworkAssociation
from services.native.legacy_catalog_importer import (
    REFERENCE_KINDS,
    LegacyCatalogImporter,
)
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.target_startup_validator import TargetStartupValidator

RG = "11111111-1111-4111-8111-111111111111"
RELEASE = "22222222-2222-4222-8222-222222222222"
RECORDING_1 = "33333333-3333-4333-8333-333333333333"
RECORDING_2 = "44444444-4444-4444-8444-444444444444"
ARTIST_1 = "55555555-5555-4555-8555-555555555555"
ARTIST_2 = "66666666-6666-4666-8666-666666666666"
TRACK_1 = "77777777-7777-4777-8777-777777777777"
TRACK_2 = "88888888-8888-4888-8888-888888888888"


def _file_hash(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _create_source(path: Path, root: Path) -> None:
    schema = (
        Path(__file__).parents[1]
        / "fixtures"
        / "feedback_fixes"
        / "legacy_catalog_schema.sql"
    ).read_text(encoding="utf-8")
    identified = root / "Compilation" / "01.flac"
    identified_two = root / "Compilation" / "02.flac"
    unresolved = root / "Local Album" / "01.flac"
    rejected = root / "Rejected" / "01.flac"
    with sqlite3.connect(path) as connection:
        connection.executescript(schema)
        connection.executemany(
            "INSERT INTO auth_users VALUES (?, ?)",
            [("alice", "Alice"), ("admin", "Admin")],
        )
        connection.executemany(
            "INSERT INTO library_files "
            "(id, release_group_mbid, release_mbid, recording_mbid, disc_number, "
            "track_number, track_title, artist_name, artist_mbid, album_artist_name, "
            "album_artist_mbid, album_title, year, file_path, file_size_bytes, file_mtime, "
            "duration_seconds, file_format, bit_rate, sample_rate, bit_depth, channels, "
            "source, is_compilation, tagged_at, imported_at, genre) "
            "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            [
                (
                    TRACK_1,
                    RG,
                    RELEASE,
                    RECORDING_1,
                    1,
                    1,
                    "First",
                    "Artist One",
                    ARTIST_1,
                    "Various Artists",
                    None,
                    "Compilation",
                    2026,
                    str(identified),
                    100,
                    10.5,
                    180.0,
                    "flac",
                    900000,
                    48000,
                    24,
                    2,
                    "scan",
                    1,
                    11.0,
                    10.0,
                    "Electronic",
                ),
                (
                    TRACK_2,
                    RG,
                    RELEASE,
                    RECORDING_2,
                    1,
                    2,
                    "Second",
                    "Artist Two",
                    ARTIST_2,
                    "Various Artists",
                    None,
                    "Compilation",
                    2026,
                    str(identified_two),
                    200,
                    10.75,
                    200.0,
                    "flac",
                    950000,
                    48000,
                    24,
                    2,
                    "scan",
                    1,
                    11.0,
                    10.0,
                    "Electronic",
                ),
            ],
        )
        connection.executemany(
            "INSERT INTO manual_review_queue VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            [
                (
                    1,
                    str(unresolved),
                    "Local Song",
                    "Local Artist",
                    "Local Album",
                    2025,
                    1,
                    1,
                    "flac",
                    190.0,
                    300,
                    None,
                    None,
                    "[]",
                    "text_match",
                    12.0,
                    None,
                    None,
                ),
                (
                    2,
                    str(rejected),
                    "Rejected Song",
                    "Local Artist",
                    "Rejected",
                    2024,
                    1,
                    1,
                    "mp3",
                    160.0,
                    250,
                    None,
                    None,
                    "[]",
                    "text_match",
                    13.0,
                    14.0,
                    "rejected",
                ),
                (
                    3,
                    str(identified),
                    "First",
                    "Artist One",
                    "Compilation",
                    2026,
                    1,
                    1,
                    "flac",
                    180.0,
                    100,
                    None,
                    None,
                    "[]",
                    "text_match",
                    9.0,
                    10.0,
                    "accepted",
                ),
                (
                    4,
                    str(identified_two),
                    "Second",
                    "Artist Two",
                    "Compilation",
                    2026,
                    2,
                    1,
                    "flac",
                    200.0,
                    200,
                    None,
                    None,
                    "[]",
                    "text_match",
                    9.0,
                    10.0,
                    "manual_id",
                ),
            ],
        )
        connection.execute(
            "INSERT INTO library_albums VALUES (?, ?, NULL, NULL, ?, ?, ?, ?, ?, ?)",
            (
                RG,
                RG,
                "Various Artists",
                "Compilation",
                2026,
                "/api/v1/covers/legacy",
                10,
                "{}",
            ),
        )
        connection.executemany(
            "INSERT INTO library_artists VALUES (?, ?, ?, 1, 10, '{}')",
            [(ARTIST_1, ARTIST_1, "Artist One"), (ARTIST_2, ARTIST_2, "Artist Two")],
        )
        connection.execute(
            "INSERT INTO library_album_meta VALUES (?, ?, ?)",
            (RG, "/api/v1/covers/verified", 20.0),
        )
        connection.executemany(
            "INSERT INTO user_favorites VALUES (?, ?, ?, ?)",
            [
                ("alice", "album", RG, 1.0),
                ("alice", "artist", ARTIST_1, 2.0),
                ("alice", "track", TRACK_1, 3.0),
            ],
        )
        connection.execute(
            "INSERT INTO play_history VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (
                "history-1",
                "alice",
                "First",
                "Artist One",
                "Compilation",
                RECORDING_1,
                RG,
                180000,
                "local",
                "2026-07-01T12:00:00Z",
            ),
        )
        connection.execute(
            "INSERT INTO playlists VALUES ('playlist-1', 'Mix', '/covers/mix.jpg', 'a', 'b', 'local')"
        )
        connection.executemany(
            "INSERT INTO playlist_tracks VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            [
                (
                    "playlist-track-1",
                    "playlist-1",
                    0,
                    "First",
                    "Artist One",
                    "Compilation",
                    RG,
                    ARTIST_1,
                    TRACK_1,
                    "/api/v1/covers/verified",
                    "local",
                    '["local"]',
                    "flac",
                    1,
                    1,
                    180,
                    "a",
                    None,
                    TRACK_1,
                ),
                (
                    "playlist-track-missing",
                    "playlist-1",
                    1,
                    "Missing",
                    "Former Artist",
                    "Former Album",
                    "missing-album",
                    "missing-artist",
                    "missing-track",
                    "/api/v1/covers/missing",
                    "local",
                    '["local"]',
                    "mp3",
                    2,
                    1,
                    200,
                    "b",
                    None,
                    "missing-track",
                ),
            ],
        )
        connection.execute(
            "INSERT INTO album_release_pins VALUES (?, ?, 'admin', '2026-07-01')",
            (RG, RELEASE),
        )
        connection.execute(
            "INSERT INTO compat_bookmarks VALUES ('alice', ?, 1234, 'note', 1, 2)",
            (TRACK_1,),
        )
        connection.execute(
            "INSERT INTO compat_play_queues VALUES ('alice', 1, 300, 2, 'client')"
        )
        connection.executemany(
            "INSERT INTO compat_play_queue_items VALUES ('alice', ?, ?)",
            [(0, TRACK_1), (1, TRACK_1)],
        )
        connection.executemany(
            "INSERT INTO compat_id_map VALUES (?, ?, ?)",
            [
                ("a" * 32, "album", RG),
                ("b" * 32, "artist", ARTIST_1),
                ("c" * 32, "track", TRACK_1),
            ],
        )


def _copy_database(source: Path, target: Path) -> None:
    with sqlite3.connect(source) as source_connection:
        with sqlite3.connect(target) as target_connection:
            source_connection.backup(target_connection)


def _insert_identityless_library_file(
    database: Path,
    root: Path,
    file_id: str,
    *,
    release_group_mbid: str | None = None,
) -> None:
    with sqlite3.connect(database) as connection:
        connection.execute(
            "INSERT INTO library_files "
            "(id, release_group_mbid, recording_mbid, disc_number, track_number, "
            "track_title, artist_name, album_artist_name, album_title, file_path, "
            "file_size_bytes, file_mtime, duration_seconds, file_format, source, "
            "is_compilation, tagged_at, imported_at) "
            "VALUES (?,?,NULL,1,1,?,?,?,?,?,?,?,?,?,'manual_review',0,21,20)",
            (
                file_id,
                release_group_mbid,
                "Identityless Track",
                "Local Artist",
                "Local Artist",
                "Identityless Album",
                str(root / "Identityless Album" / "01.flac"),
                1_000,
                20.0,
                180.0,
                "flac",
            ),
        )


class _CoverReader:
    def __init__(self, result: bytes | None = None) -> None:
        self.result = result
        self.paths: list[Path] = []

    def read_cover_art(self, path: Path) -> bytes | None:
        self.paths.append(path)
        return self.result


def _importer(
    database: Path, root: Path, cover_reader: _CoverReader | None = None
) -> tuple[NativeLibraryStore, LegacyCatalogImporter]:
    store = NativeLibraryStore(database, threading.Lock())
    resolver = LibraryPolicyResolver(
        TypedLibrarySettings(
            library_roots=[
                LibraryRootSettings(
                    id="root-1", path=str(root), label="Music", policy="automatic"
                )
            ]
        )
    )
    return store, LegacyCatalogImporter(store, resolver, cover_reader or _CoverReader())


async def _migrate_startup_fixture(
    tmp_path: Path, migration_id: str
) -> tuple[Path, NativeLibraryStore]:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    store, importer = _importer(database, root)
    plan, _report = await importer.prepare(migration_id, now=100)
    await importer.apply(
        migration_id,
        expected_source_revision=plan.source_revision,
        now=101,
    )
    return database, store


@pytest.mark.asyncio
async def test_target_startup_requires_completed_marker_and_revalidates_after_reopen(
    tmp_path: Path,
    caplog: pytest.LogCaptureFixture,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    source = tmp_path / "source.db"
    target = tmp_path / "target.db"
    _create_source(source, root)
    _copy_database(source, target)
    store, importer = _importer(target, root)

    with pytest.raises(TargetStartupInvariantError, match="marker is missing"):
        await TargetStartupValidator(store).validate("cutover")

    plan, _report = await importer.prepare("startup-validation", now=100)
    await importer.apply(
        "startup-validation",
        expected_source_revision=plan.source_revision,
        now=101,
    )
    first_cutover = await TargetStartupValidator(store).validate("cutover")
    first_steady = await TargetStartupValidator(store).validate("steady_state")
    reopened = NativeLibraryStore(target, threading.Lock())
    second_cutover = await TargetStartupValidator(reopened).validate("cutover")
    second_steady = await TargetStartupValidator(reopened).validate("steady_state")

    assert (
        first_cutover["invariants"]
        == second_cutover["invariants"]
        == {
            "foreign_key_violations": 0,
            "orphan_tracks": 0,
            "duplicate_paths": 0,
            "unresolved_provenance": 0,
            "unresolved_references": 0,
        }
    )
    assert (
        first_steady["invariants"]
        == second_steady["invariants"]
        == {
            "foreign_key_violations": 0,
            "orphan_tracks": 0,
            "duplicate_paths": 0,
            "unresolved_provenance": 0,
        }
    )

    await reopened.remove_target_favorite("alice", "track", TRACK_1)

    assert (await reopened.validate_migrated_catalog())["unresolved_references"] == 1
    caplog.clear()
    caplog.set_level(logging.ERROR, logger="services.native.target_startup_validator")
    with pytest.raises(TargetStartupInvariantError) as error:
        await TargetStartupValidator(reopened).validate("cutover")
    assert str(error.value) == "The target catalog failed startup integrity validation."
    assert [record.getMessage() for record in caplog.records] == [
        "target_startup.catalog_integrity_failed phase=cutover "
        "counters=unresolved_references=1"
    ]
    assert TRACK_1 not in caplog.text

    await TargetStartupValidator(NativeLibraryStore(target, threading.Lock())).validate(
        "steady_state"
    )


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "configured_roots",
    [set(), {"root-2"}, {"root-1", "root-2"}],
    ids=["removed", "changed", "added"],
)
async def test_steady_startup_accepts_post_admission_root_changes(
    tmp_path: Path, configured_roots: set[str]
) -> None:
    _database, store = await _migrate_startup_fixture(tmp_path, "root-settings")
    validator = TargetStartupValidator(store, lambda: configured_roots)

    await validator.validate("steady_state")

    with pytest.raises(TargetStartupInvariantError, match="configured library roots"):
        await validator.validate("cutover")


def _allow_duplicate_local_track_paths(database: Path) -> None:
    with sqlite3.connect(database) as connection:
        schema = str(
            connection.execute(
                "SELECT sql FROM sqlite_master WHERE type = 'table' "
                "AND name = 'local_tracks'"
            ).fetchone()[0]
        )
        without_unique_path = schema.replace(
            ",\n    UNIQUE(root_id, relative_path)\n", "\n"
        )
        assert without_unique_path != schema
        replacement_schema = without_unique_path.replace(
            "CREATE TABLE local_tracks",
            "CREATE TABLE local_tracks_without_path_unique",
            1,
        )
        connection.execute("PRAGMA foreign_keys=OFF")
        for (trigger_name,) in connection.execute(
            "SELECT name FROM sqlite_master WHERE type = 'trigger'"
        ).fetchall():
            connection.execute(f'DROP TRIGGER "{trigger_name}"')
        connection.execute(replacement_schema)
        connection.execute(
            "INSERT INTO local_tracks_without_path_unique SELECT * FROM local_tracks"
        )
        connection.execute("DROP TABLE local_tracks")
        connection.execute(
            "ALTER TABLE local_tracks_without_path_unique RENAME TO local_tracks"
        )


@pytest.mark.asyncio
async def test_steady_startup_rejects_every_durable_integrity_failure(
    tmp_path: Path,
) -> None:
    database, store = await _migrate_startup_fixture(tmp_path, "durable-integrity")
    validator = TargetStartupValidator(store)

    with sqlite3.connect(database) as connection:
        marker = connection.execute(
            "SELECT source_revision, target_catalog_revision, created_at "
            "FROM library_migration_markers"
        ).fetchone()
        connection.execute("DELETE FROM library_migration_markers")
    with pytest.raises(TargetStartupInvariantError, match="marker is missing"):
        await validator.validate("steady_state")
    with sqlite3.connect(database) as connection:
        connection.execute(
            "INSERT INTO library_migration_markers "
            "(marker, source_revision, target_catalog_revision, created_at) "
            "VALUES ('legacy_catalog_import_complete', ?, ?, ?)",
            marker,
        )

    with sqlite3.connect(database) as connection:
        connection.execute(
            "UPDATE library_migration_runs SET state = 'failed' WHERE state = 'completed'"
        )
    with pytest.raises(TargetStartupInvariantError, match="migration run"):
        await validator.validate("steady_state")
    with sqlite3.connect(database) as connection:
        connection.execute(
            "UPDATE library_migration_runs SET state = 'completed' WHERE state = 'failed'"
        )

    with sqlite3.connect(database) as connection:
        connection.execute(
            "UPDATE library_migration_markers "
            "SET target_catalog_revision = target_catalog_revision + 100"
        )
    with pytest.raises(TargetStartupInvariantError, match="predates"):
        await validator.validate("steady_state")
    with sqlite3.connect(database) as connection:
        connection.execute(
            "UPDATE library_migration_markers "
            "SET target_catalog_revision = target_catalog_revision - 100"
        )

    with sqlite3.connect(database) as connection:
        connection.execute(
            "UPDATE library_play_history SET local_track_id = 'missing-track' "
            "WHERE id = 'history-1'"
        )
    assert (await store.validate_catalog_integrity())["foreign_key_violations"] == 1
    with pytest.raises(TargetStartupInvariantError, match="integrity validation"):
        await validator.validate("steady_state")
    with sqlite3.connect(database) as connection:
        connection.execute(
            "UPDATE library_play_history SET local_track_id = ? WHERE id = 'history-1'",
            (TRACK_1,),
        )

    with sqlite3.connect(database) as connection:
        album_id = str(
            connection.execute(
                "SELECT local_album_id FROM local_tracks WHERE id = ?", (TRACK_1,)
            ).fetchone()[0]
        )
        connection.execute(
            "UPDATE local_tracks SET local_album_id = 'missing-album' WHERE id = ?",
            (TRACK_1,),
        )
    assert (await store.validate_catalog_integrity())["orphan_tracks"] == 1
    with pytest.raises(TargetStartupInvariantError, match="integrity validation"):
        await validator.validate("steady_state")
    with sqlite3.connect(database) as connection:
        connection.execute(
            "UPDATE local_tracks SET local_album_id = ? WHERE id = ?",
            (album_id, TRACK_1),
        )

    _allow_duplicate_local_track_paths(database)
    with sqlite3.connect(database) as connection:
        columns = [
            str(row[1]) for row in connection.execute("PRAGMA table_info(local_tracks)")
        ]
        selected = ", ".join("?" if name == "id" else f'"{name}"' for name in columns)
        connection.execute(
            f"INSERT INTO local_tracks SELECT {selected} FROM local_tracks WHERE id = ?",
            ("duplicate-track", TRACK_1),
        )
    assert (await store.validate_catalog_integrity())["duplicate_paths"] == 1
    with pytest.raises(TargetStartupInvariantError, match="integrity validation"):
        await validator.validate("steady_state")
    with sqlite3.connect(database) as connection:
        connection.execute("DELETE FROM local_tracks WHERE id = 'duplicate-track'")

    with sqlite3.connect(database) as connection:
        provenance = connection.execute(
            "SELECT source_kind, source_key, target_id "
            "FROM library_migration_provenance LIMIT 1"
        ).fetchone()
        connection.execute(
            "UPDATE library_migration_provenance SET target_id = '' "
            "WHERE source_kind = ? AND source_key = ?",
            provenance[:2],
        )
    assert (await store.validate_catalog_integrity())["unresolved_provenance"] == 1
    with pytest.raises(TargetStartupInvariantError, match="integrity validation"):
        await validator.validate("steady_state")


@pytest.mark.asyncio
async def test_post_admission_mutations_preserve_steady_startup_integrity(
    tmp_path: Path,
) -> None:
    database, store = await _migrate_startup_fixture(
        tmp_path, "post-admission-mutations"
    )
    with sqlite3.connect(database) as connection:
        provenance_before = connection.execute(
            "SELECT source_kind, source_key, target_kind, target_id, source_revision, "
            "imported_at, migration_run_id FROM library_migration_provenance "
            "ORDER BY source_kind, source_key"
        ).fetchall()
        review = connection.execute(
            "SELECT id, row_revision FROM library_identification_reviews "
            "WHERE reason_code = 'legacy_unresolved'"
        ).fetchone()
        artwork = connection.execute(
            "SELECT artwork.local_album_id, album.row_revision "
            "FROM local_album_artwork artwork JOIN local_albums album "
            "ON album.id = artwork.local_album_id LIMIT 1"
        ).fetchone()

    await store.remove_target_favorite("alice", "track", TRACK_1)
    assert (
        await store.remove_target_playlist_tracks(
            "playlist-1", ["playlist-track-1"], "2026-07-19T00:00:00Z"
        )
        == 1
    )
    assert await store.delete_target_playlist("playlist-1") is True
    await store.delete_target_bookmark("alice", TRACK_1)
    await store.replace_target_play_queue(
        "alice",
        (TRACK_2,),
        current_index=0,
        position_ms=0,
        changed_by_client="regression",
        updated_at=200,
    )
    assert await store.clear_target_album_release_pin(RG) is True
    await store.decide_review(
        str(review[0]),
        expected_review_revision=int(review[1]),
        state="resolved",
        reason_code="user_resolved",
        decided_by_user_id="admin",
        decided_at=200,
    )
    await store.set_artwork(
        LocalArtworkAssociation(
            local_album_id=str(artwork[0]),
            cover_url=None,
            source="manual",
            source_locator="replacement.jpg",
            updated_at=200,
        ),
        expected_album_revision=int(artwork[1]),
    )

    reopened = NativeLibraryStore(database, threading.Lock())
    steady = await TargetStartupValidator(reopened).validate("steady_state")
    cutover = await reopened.validate_migrated_catalog()
    with sqlite3.connect(database) as connection:
        provenance_after = connection.execute(
            "SELECT source_kind, source_key, target_kind, target_id, source_revision, "
            "imported_at, migration_run_id FROM library_migration_provenance "
            "ORDER BY source_kind, source_key"
        ).fetchall()
        current_artwork_source = connection.execute(
            "SELECT source FROM local_album_artwork WHERE local_album_id = ?",
            (artwork[0],),
        ).fetchone()[0]

    assert all(count == 0 for count in steady["invariants"].values())
    assert cutover["unresolved_references"] > 0
    assert provenance_after == provenance_before
    assert current_artwork_source == "manual"
    with pytest.raises(TargetStartupInvariantError, match="integrity validation"):
        await TargetStartupValidator(reopened).validate("cutover")


@pytest.mark.asyncio
async def test_coherent_copy_import_keeps_identityless_library_file_local(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    file_id = "99999999-9999-4999-8999-999999999999"
    _insert_identityless_library_file(database, root, file_id)
    with sqlite3.connect(database) as connection:
        connection.execute(
            "INSERT INTO playlist_tracks VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                "playlist-track-identityless",
                "playlist-1",
                2,
                "Identityless Track",
                "Local Artist",
                "Identityless Album",
                "malformed-release-group",
                "malformed-artist",
                file_id,
                None,
                "local",
                '["local"]',
                "flac",
                1,
                1,
                180,
                "c",
                None,
                file_id,
            ),
        )
    store, importer = _importer(database, root)

    plan, report = await importer.prepare("identityless-plan", now=100)

    assert report.state == "ready"
    assert report.blockers == []
    bundle = next(
        item
        for item in plan.bundles
        if any(track.id == file_id for track in item.membership.tracks)
    )
    assert bundle.album_identity is None
    assert bundle.membership.tracks[0].id == file_id
    assert bundle.reviews[0].reason_code == "legacy_missing_release_group_id"

    applied = await importer.apply(
        "identityless-plan",
        expected_source_revision=plan.source_revision,
        now=101,
    )

    assert applied.state == "applied"
    assert (await store.get_local_track(file_id))["id"] == file_id
    with sqlite3.connect(database) as connection:
        playlist_reference = connection.execute(
            "SELECT local_track_id, reference_tombstone_id "
            "FROM library_playlist_tracks WHERE id = ?",
            ("playlist-track-identityless",),
        ).fetchone()
    assert playlist_reference == (file_id, None)


@pytest.mark.asyncio
async def test_coherent_copy_maps_unambiguous_malformed_album_reference(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    file_id = "99999999-9999-4999-8999-999999999998"
    malformed_release_group = "legacy-release-group"
    _insert_identityless_library_file(
        database,
        root,
        file_id,
        release_group_mbid=malformed_release_group,
    )
    with sqlite3.connect(database) as connection:
        connection.execute(
            "INSERT INTO user_favorites VALUES ('alice', 'album', ?, 4)",
            (malformed_release_group,),
        )
    store, importer = _importer(database, root)

    plan, report = await importer.prepare("malformed-album-reference", now=100)

    assert report.state == "ready"
    bundle = next(
        item
        for item in plan.bundles
        if any(track.id == file_id for track in item.membership.tracks)
    )
    assert bundle.album_identity is None
    assert [(alias.alias, alias.kind) for alias in bundle.album_aliases] == [
        (malformed_release_group, "compat_migration")
    ]

    await importer.apply(
        "malformed-album-reference",
        expected_source_revision=plan.source_revision,
        now=101,
    )

    with sqlite3.connect(database) as connection:
        favorite = connection.execute(
            "SELECT item_id FROM library_user_favorites "
            "WHERE user_id = 'alice' AND item_kind = 'album' AND item_id = ?",
            (bundle.membership.album.id,),
        ).fetchone()
    assert favorite == (bundle.membership.album.id,)


@pytest.mark.asyncio
async def test_coherent_copy_uses_filename_and_folder_for_empty_local_tags(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    file_id = "99999999-9999-4999-8999-999999999997"
    _insert_identityless_library_file(database, root, file_id)
    with sqlite3.connect(database) as connection:
        connection.execute(
            "UPDATE library_files SET track_title = '', album_title = NULL, "
            "file_path = ? WHERE id = ?",
            (str(root / "Folder Fallback" / "filename-title.flac"), file_id),
        )
    _store, importer = _importer(database, root)

    plan, report = await importer.prepare("empty-local-tags", now=100)

    assert report.state == "ready"
    bundle = next(
        item
        for item in plan.bundles
        if any(track.id == file_id for track in item.membership.tracks)
    )
    track = next(track for track in bundle.membership.tracks if track.id == file_id)
    assert bundle.membership.album.title == "Folder Fallback"
    assert track.title == "filename-title"
    assert track.album_title == "Folder Fallback"
    assert track.tag_album_title == ""
    assert track.metadata_incomplete is True


@pytest.mark.asyncio
async def test_coherent_copy_import_reconciles_catalog_references_and_artwork(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    source = tmp_path / "source.db"
    target = tmp_path / "target.db"
    _create_source(source, root)
    source_hash = _file_hash(source)
    _copy_database(source, target)
    store, importer = _importer(target, root)
    legacy_before = await store.get_legacy_migration_snapshot()

    plan, report = await importer.prepare("migration-1", now=100)

    assert report.state == "ready"
    assert report.blockers == []
    assert (report.network_calls, report.tag_reads, report.fingerprints) == (0, 0, 0)
    assert (report.identified_albums, report.identified_tracks) == (1, 2)
    assert (report.local_only_albums, report.local_only_tracks) == (2, 2)
    expected_reference_kinds = {
        "root",
        "library_file",
        "review_row",
        "favorite",
        "history",
        "playlist_track",
        "album_release_pin",
        "compat_bookmark",
        "compat_play_queue",
        "compat_play_queue_item",
        "manual_decision",
        "subsonic_id",
        "jellyfin_id_map",
        "native_album_alias",
        "native_artist_alias",
        "artwork_reference",
    }
    assert set(REFERENCE_KINDS) == expected_reference_kinds
    assert {count.kind for count in report.reference_counts} == expected_reference_kinds
    assert all(
        count.unresolved == 0
        for count in report.reference_counts
        if count.user_id is None
    )
    favorite_user = next(
        count
        for count in report.reference_counts
        if count.kind == "favorite" and count.user_id == "alice"
    )
    assert (favorite_user.source, favorite_user.mapped) == (3, 3)

    applied = await importer.apply(
        "migration-1", expected_source_revision=plan.source_revision, now=101
    )

    assert applied.state == "applied"
    compilation = next(
        bundle for bundle in plan.bundles if bundle.album_identity is not None
    )
    assert await store.get_legacy_migration_snapshot() == legacy_before
    assert _file_hash(source) == source_hash
    assert await store.row_count("local_albums") == 3
    assert await store.row_count("local_tracks") == 4
    assert await store.row_count("local_artists") == 5
    assert await store.row_count("local_artist_external_identities") == 2
    assert await store.row_count("local_album_aliases") == 1
    assert await store.row_count("library_reference_tombstones") == 1
    assert await store.row_count("local_album_artwork") == 1
    assert await store.row_count("local_album_external_identities") == 1
    assert await store.row_count("local_track_external_identities") == 2
    assert await store.row_count("library_user_favorites") == 3
    assert await store.row_count("library_play_history") == 1
    assert await store.row_count("library_playlists") == 1
    assert await store.row_count("library_playlist_tracks") == 2
    assert await store.row_count("library_album_release_pins") == 1
    assert await store.row_count("library_compat_bookmarks") == 1
    assert await store.row_count("library_compat_play_queues") == 1
    assert await store.row_count("library_compat_play_queue_items") == 2
    assert await store.row_count("library_compat_id_map") == 3
    with sqlite3.connect(target) as connection:
        stable_history = connection.execute(
            "SELECT local_track_id, local_album_id, local_artist_id "
            "FROM library_play_history"
        ).fetchone()
        stable_playlist = connection.execute(
            "SELECT local_track_id, local_album_id, local_artist_id, "
            "reference_tombstone_id FROM library_playlist_tracks "
            "ORDER BY position"
        ).fetchall()
    assert stable_history[0] == TRACK_1
    assert stable_history[1:] == (
        compilation.membership.album.id,
        compilation.membership.track_credits[TRACK_1][0].local_artist_id,
    )
    assert stable_playlist[0][:3] == stable_history
    assert stable_playlist[1][0] is None
    assert stable_playlist[1][3] is not None
    assert await store.validate_migrated_catalog() == {
        "foreign_key_violations": 0,
        "orphan_tracks": 0,
        "duplicate_paths": 0,
        "unresolved_provenance": 0,
        "unresolved_references": 0,
    }
    assert (await store.get_local_track(TRACK_1))["id"] == TRACK_1
    assert (await store.get_local_track(TRACK_2))["id"] == TRACK_2
    assert compilation.membership.album.album_artist_id == VARIOUS_ARTISTS_ID
    assert compilation.artwork.cover_url == "/api/v1/covers/verified"
    assert compilation.artwork.source == "provider"
    assert await store.resolve_migrated_reference(
        "subsonic_id", f"track:{TRACK_1}"
    ) == (
        "local_track",
        TRACK_1,
    )
    assert await store.resolve_migrated_reference("favorite", f"alice:album:{RG}") == (
        "local_album",
        compilation.membership.album.id,
    )
    assert await store.resolve_migrated_reference(
        "compat_bookmark", f"alice:{TRACK_1}"
    ) == (
        "local_track",
        TRACK_1,
    )
    assert await store.resolve_album_alias(RG) == compilation.membership.album.id
    old_artist_target = await store.resolve_artist_alias(ARTIST_1)
    old_album = await store.get_local_album(await store.resolve_album_alias(RG))
    search_results = await store.search_local_tracks("First")
    playlist_target = await store.resolve_migrated_reference(
        "playlist_track", "playlist-track-1"
    )
    artwork = await store.get_local_artwork(compilation.membership.album.id)
    stream_target = await store.get_local_track(TRACK_1)
    jellyfin_target = await store.resolve_migrated_reference(
        "jellyfin_id_map", "a" * 32
    )
    jellyfin_view = {
        "Id": "a" * 32,
        "Type": "MusicAlbum",
        "InternalId": jellyfin_target[1],
    }

    assert old_album["title"] == "Compilation"
    assert (
        old_artist_target
        == compilation.membership.track_credits[TRACK_1][0].local_artist_id
    )
    assert [row["id"] for row in search_results] == [TRACK_1]
    assert playlist_target == ("local_track", TRACK_1)
    assert artwork["cover_url"] == "/api/v1/covers/verified"
    assert stream_target["file_path"].endswith("Compilation/01.flac")
    assert jellyfin_view == {
        "Id": "a" * 32,
        "Type": "MusicAlbum",
        "InternalId": compilation.membership.album.id,
    }

    with sqlite3.connect(target) as connection:
        connection.row_factory = sqlite3.Row
        jf_album = connection.execute(
            "SELECT target_id FROM library_migration_provenance "
            "WHERE source_kind = 'jellyfin_id_map' AND source_key = ?",
            ("a" * 32,),
        ).fetchone()
        excluded = connection.execute(
            "SELECT availability FROM local_tracks WHERE availability = 'excluded'"
        ).fetchall()
        queue = connection.execute(
            "SELECT file_id FROM compat_play_queue_items ORDER BY item_index"
        ).fetchall()
        decisions = connection.execute(
            "SELECT reason_code, state, decided_by_user_id, decided_at "
            "FROM library_identification_reviews ORDER BY reason_code"
        ).fetchall()
        artist_aliases = connection.execute(
            "SELECT alias FROM local_artist_aliases ORDER BY alias"
        ).fetchall()
        migration_state = connection.execute(
            "SELECT state, report_json FROM library_migration_runs WHERE id = 'migration-1'"
        ).fetchone()
        marker = connection.execute(
            "SELECT source_revision FROM library_migration_markers "
            "WHERE marker = 'legacy_catalog_import_complete'"
        ).fetchone()
        external_album_ids = {
            row[0]
            for row in connection.execute(
                "SELECT release_group_mbid FROM local_album_external_identities"
            ).fetchall()
        }
        local_album_ids = {
            row[0]
            for row in connection.execute("SELECT id FROM local_albums").fetchall()
        }
    assert jf_album["target_id"] == compilation.membership.album.id
    assert len(excluded) == 1
    assert [row["file_id"] for row in queue] == [TRACK_1, TRACK_1]
    assert {row["reason_code"] for row in decisions} == {
        "legacy_accepted",
        "legacy_manual_id",
        "legacy_rejected",
        "legacy_unresolved",
    }
    assert all(
        row["decided_by_user_id"] is None
        for row in decisions
        if row["reason_code"] != "legacy_unresolved"
    )
    assert all(
        row["decided_at"] is not None
        for row in decisions
        if row["reason_code"] != "legacy_unresolved"
    )
    assert {row["alias"] for row in artist_aliases} == {ARTIST_1, ARTIST_2}
    assert external_album_ids == {RG}
    assert external_album_ids.isdisjoint(local_album_ids)
    assert migration_state["state"] == "completed"
    assert '"state":"applied"' in migration_state["report_json"]
    assert marker["source_revision"] == plan.source_revision


@pytest.mark.asyncio
async def test_import_is_idempotent_and_changed_source_is_refused(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    source = tmp_path / "source.db"
    target = tmp_path / "target.db"
    _create_source(source, root)
    _copy_database(source, target)
    store, importer = _importer(target, root)
    plan, _ = await importer.prepare("migration-1", now=100)
    await importer.apply(
        "migration-1", expected_source_revision=plan.source_revision, now=101
    )
    counts = {
        table: await store.row_count(table)
        for table in (
            "local_albums",
            "local_tracks",
            "library_migration_provenance",
            "library_reference_tombstones",
            "library_user_favorites",
            "library_play_history",
            "library_playlists",
            "library_playlist_tracks",
            "library_album_release_pins",
            "library_compat_bookmarks",
            "library_compat_play_queues",
            "library_compat_play_queue_items",
            "library_compat_id_map",
        )
    }
    revision = await store.get_catalog_revision()

    await importer.apply(
        "migration-1", expected_source_revision=plan.source_revision, now=102
    )

    assert {table: await store.row_count(table) for table in counts} == counts
    assert await store.get_catalog_revision() == revision

    with sqlite3.connect(target) as connection:
        connection.execute(
            "UPDATE user_favorites SET created_at = 99 WHERE user_id = 'alice' "
            "AND item_kind = 'album'"
        )
    with pytest.raises(StaleRevisionError):
        await importer.apply(
            "migration-1", expected_source_revision=plan.source_revision, now=103
        )


@pytest.mark.asyncio
async def test_import_preserves_provider_era_references_and_stale_jellyfin_ids(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "source.db"
    _create_source(database, root)
    with sqlite3.connect(database) as connection:
        connection.execute(
            "INSERT INTO play_history VALUES (?,?,?,?,?,?,?,?,?,?)",
            (
                "provider-era-history",
                "alice",
                "First",
                "Artist One",
                "Compilation",
                None,
                None,
                180000,
                None,
                "2025-01-01T00:00:00Z",
            ),
        )
        connection.execute(
            "INSERT INTO playlist_tracks VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                "provider-era-playlist-track",
                "playlist-1",
                2,
                "Unavailable provider track",
                "Former Artist",
                "Former Album",
                None,
                None,
                None,
                None,
                "",
                "[]",
                None,
                None,
                None,
                None,
                "c",
                None,
                None,
            ),
        )
        connection.executemany(
            "INSERT INTO compat_id_map VALUES (?, ?, ?)",
            [
                ("d" * 32, "library", "music"),
                ("e" * 32, "track", "removed-track"),
                ("f" * 32, "genre", "Electronic"),
                ("g" * 32, "playlist", "playlist-1"),
                ("h" * 32, "playlist", "removed-playlist"),
            ],
        )
    store, importer = _importer(database, root)

    plan, report = await importer.prepare("provider-era", now=100)

    counts = {
        count.kind: count for count in report.reference_counts if count.user_id is None
    }
    assert report.state == "ready"
    assert counts["history"].source == counts["history"].mapped == 2
    assert counts["playlist_track"].source == counts["playlist_track"].mapped == 3
    assert counts["playlist_track"].tombstoned == 2
    assert counts["jellyfin_id_map"].source == counts["jellyfin_id_map"].mapped == 8
    assert counts["jellyfin_id_map"].tombstoned == 2

    await importer.apply(
        "provider-era", expected_source_revision=plan.source_revision, now=101
    )

    with sqlite3.connect(database) as connection:
        history_target = connection.execute(
            "SELECT local_track_id FROM library_play_history WHERE id = ?",
            ("provider-era-history",),
        ).fetchone()
        playlist_target = connection.execute(
            "SELECT reference_tombstone_id FROM library_playlist_tracks WHERE id = ?",
            ("provider-era-playlist-track",),
        ).fetchone()
        mappings = connection.execute(
            "SELECT jf_id, kind, internal_id FROM library_compat_id_map "
            "WHERE jf_id IN (?, ?, ?, ?, ?) ORDER BY jf_id",
            ("d" * 32, "e" * 32, "f" * 32, "g" * 32, "h" * 32),
        ).fetchall()
        stale_provenance = connection.execute(
            "SELECT target_kind FROM library_migration_provenance "
            "WHERE source_kind = 'jellyfin_id_map' AND source_key = ?",
            ("e" * 32,),
        ).fetchone()
    assert history_target == (TRACK_1,)
    assert playlist_target[0] is not None
    assert mappings == [
        ("d" * 32, "library", "music"),
        ("e" * 32, "track", "removed-track"),
        ("f" * 32, "genre", "Electronic"),
        ("g" * 32, "playlist", "playlist-1"),
        ("h" * 32, "playlist", "removed-playlist"),
    ]
    assert stale_provenance == ("reference_tombstone",)
    assert all(
        value == 0 for value in (await store.validate_migrated_catalog()).values()
    )


@pytest.mark.asyncio
async def test_identifierless_history_blocks_when_text_matches_multiple_tracks(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "source.db"
    _create_source(database, root)
    with sqlite3.connect(database) as connection:
        connection.execute(
            "UPDATE library_files SET track_title = 'First', "
            "artist_name = 'Artist One' WHERE id = ?",
            (TRACK_2,),
        )
        connection.execute(
            "INSERT INTO play_history VALUES (?,?,?,?,?,?,?,?,?,?)",
            (
                "ambiguous-provider-era-history",
                "alice",
                "First",
                "Artist One",
                "Compilation",
                None,
                None,
                180000,
                None,
                "2025-01-01T00:00:00Z",
            ),
        )
    _store, importer = _importer(database, root)

    _plan, report = await importer.prepare("ambiguous-history", now=100)

    assert report.state == "blocked"
    assert any(
        blocker
        == "history reference ambiguous-provider-era-history cannot be resolved."
        for blocker in report.blockers
    )


@pytest.mark.asyncio
async def test_apply_refuses_typed_root_changes_after_dry_run(tmp_path: Path) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "target.db"
    _create_source(database, root)
    store, importer = _importer(database, root)
    plan, _ = await importer.prepare("root-change", now=100)
    changed_resolver = LibraryPolicyResolver(
        TypedLibrarySettings(
            library_roots=[
                LibraryRootSettings(
                    id="replacement-root",
                    path=str(root),
                    label="Replacement",
                    policy="automatic",
                )
            ]
        )
    )
    changed_importer = LegacyCatalogImporter(store, changed_resolver, _CoverReader())

    with pytest.raises(StaleRevisionError, match="library roots"):
        await changed_importer.apply(
            "root-change",
            expected_source_revision=plan.source_revision,
            now=101,
        )

    assert await store.row_count("local_albums") == 0


@pytest.mark.asyncio
async def test_active_unresolved_reference_blocks_apply(tmp_path: Path) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    source = tmp_path / "source.db"
    _create_source(source, root)
    with sqlite3.connect(source) as connection:
        connection.execute(
            "INSERT INTO user_favorites VALUES ('alice', 'album', 'missing', 5)"
        )
    store, importer = _importer(source, root)

    plan, report = await importer.prepare("migration-blocked", now=100)

    assert report.state == "blocked"
    assert any("favorite reference" in blocker for blocker in report.blockers)
    with pytest.raises(ValidationError):
        await importer.apply(
            "migration-blocked",
            expected_source_revision=plan.source_revision,
            now=101,
        )


@pytest.mark.asyncio
async def test_embedded_art_discovery_is_bounded_to_identified_albums(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    source = tmp_path / "source.db"
    _create_source(source, root)
    with sqlite3.connect(source) as connection:
        connection.execute("DELETE FROM library_album_meta")
        connection.execute("UPDATE library_albums SET cover_url = NULL")
    cover_reader = _CoverReader(b"embedded-cover")
    _, importer = _importer(source, root, cover_reader)

    plan, report = await importer.prepare("migration-art", now=100)

    identified = next(
        bundle for bundle in plan.bundles if bundle.album_identity is not None
    )
    assert report.embedded_art_reads == 1
    assert cover_reader.paths == [Path(identified.membership.tracks[0].file_path)]
    assert identified.artwork.source == "embedded"
    assert identified.artwork.source_locator == identified.membership.tracks[0].id


@pytest.mark.asyncio
async def test_identified_album_keeps_reproducible_provider_artwork_key(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    source = tmp_path / "source.db"
    _create_source(source, root)
    with sqlite3.connect(source) as connection:
        connection.execute("DELETE FROM library_album_meta")
        connection.execute("UPDATE library_albums SET cover_url = NULL")
    _, importer = _importer(source, root, _CoverReader())

    plan, _ = await importer.prepare("migration-provider-art", now=100)

    identified = next(
        bundle for bundle in plan.bundles if bundle.album_identity is not None
    )
    assert identified.artwork is not None
    assert identified.artwork.source == "provider"
    assert identified.artwork.source_locator == RG
    assert identified.artwork.cover_url is None


def test_importer_provider_is_singleton_and_only_builds_in_isolated_composition(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    store = object()
    resolver = object()
    cover_reader = object()
    monkeypatch.setattr(cache_providers, "get_native_library_store", lambda: store)
    monkeypatch.setattr(
        service_providers, "get_library_policy_resolver", lambda: resolver
    )
    monkeypatch.setattr(service_providers, "get_audio_tagger", lambda: cover_reader)
    service_providers.get_legacy_catalog_importer.cache_clear()

    first = service_providers.get_legacy_catalog_importer()
    second = service_providers.get_legacy_catalog_importer()

    assert first is second
    assert first._store is store
    assert first._resolver is resolver
    assert first._cover_reader is cover_reader
    service_providers.get_legacy_catalog_importer.cache_clear()
