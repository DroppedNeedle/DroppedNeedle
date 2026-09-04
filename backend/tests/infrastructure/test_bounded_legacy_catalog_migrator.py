import sqlite3
import threading
from pathlib import Path

import pytest

from api.v1.schemas.library_policies import LibraryRootSettings, TypedLibrarySettings
from core.exceptions import StaleRevisionError
from infrastructure.persistence.native_library_store import NativeLibraryStore
from services.native.bounded_legacy_catalog_migrator import (
    BoundedLegacyCatalogMigrator,
)
from services.native.legacy_catalog_importer import _valid_mbid
from services.native.library_policy_resolver import LibraryPolicyResolver
from tests.infrastructure.test_legacy_catalog_importer import (
    ARTIST_1,
    RECORDING_1,
    RG,
    TRACK_1,
    TRACK_2,
    _create_source,
)


def _migrator(
    database: Path,
    root: Path,
    progress: list[str],
    *,
    batch_size: int = 1,
) -> tuple[NativeLibraryStore, BoundedLegacyCatalogMigrator]:
    store = NativeLibraryStore(database, threading.Lock())
    resolver = LibraryPolicyResolver(
        TypedLibrarySettings(
            library_roots=[
                LibraryRootSettings(
                    id="root-1",
                    path=str(root),
                    label="Music",
                    policy="automatic",
                )
            ]
        )
    )
    return store, BoundedLegacyCatalogMigrator(
        store,
        resolver,
        emit_progress=progress.append,
        batch_size=batch_size,
    )


def _insert_legacy_library_file(
    connection: sqlite3.Connection,
    *,
    file_id: str,
    path: Path,
    title: str,
    track_number: int,
    release_group_mbid: str | None,
    recording_mbid: str | None = None,
    album_title: str | None = "Unidentified Album",
    album_artist_name: str = "Local Artist",
    is_compilation: int = 0,
) -> None:
    connection.execute(
        "INSERT INTO library_files "
        "(id, release_group_mbid, release_mbid, recording_mbid, disc_number, "
        "track_number, track_title, artist_name, album_artist_name, album_title, "
        "file_path, file_size_bytes, file_mtime, duration_seconds, file_format, "
        "source, is_compilation, tagged_at, imported_at) "
        "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (
            file_id,
            release_group_mbid,
            None,
            recording_mbid,
            1,
            track_number,
            title,
            "Local Artist",
            album_artist_name,
            album_title,
            str(path),
            1_000 + track_number,
            20.0 + track_number,
            180.0,
            "flac",
            "manual_review",
            is_compilation,
            21.0,
            20.0,
        ),
    )


def test_legacy_mbid_validation_requires_canonical_unpadded_value() -> None:
    canonical = "88d17133-abbc-42db-9526-4e2c1db60336"

    assert _valid_mbid(canonical)
    assert not _valid_mbid(canonical.replace("-", ""))
    assert not _valid_mbid(f" {canonical} ")


@pytest.mark.asyncio
async def test_bounded_migration_indexes_review_path_resolution(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    store, _migrator_instance = _migrator(database, root, [])

    await store.prepare_bounded_legacy_migration()

    with sqlite3.connect(database) as connection:
        plan = connection.execute(
            "EXPLAIN QUERY PLAN SELECT id FROM local_tracks WHERE file_path = ?",
            (str(root / "Compilation" / "01.flac"),),
        ).fetchall()
    assert any("idx_bounded_migration_local_track_path" in str(row) for row in plan)

    await store.finish_bounded_legacy_migration()


@pytest.mark.asyncio
async def test_bounded_migration_preserves_catalog_references_and_reports_progress(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    progress: list[str] = []
    store, migrator = _migrator(database, root, progress)

    outcome = await migrator.migrate("bounded-migration", now=100)

    assert outcome.blocker_count == 0
    assert outcome.report.state == "applied"
    assert (outcome.report.identified_albums, outcome.report.identified_tracks) == (
        1,
        2,
    )
    assert (outcome.report.local_only_albums, outcome.report.local_only_tracks) == (
        2,
        2,
    )
    assert outcome.invariants == {
        "foreign_key_violations": 0,
        "orphan_tracks": 0,
        "duplicate_paths": 0,
        "unresolved_provenance": 0,
        "unresolved_references": 0,
    }
    assert await store.row_count("local_tracks") == 4
    assert await store.row_count("library_user_favorites") == 3
    assert await store.row_count("library_play_history") == 1
    assert await store.row_count("library_playlist_tracks") == 2
    assert await store.row_count("library_album_release_pins") == 1
    assert await store.row_count("library_compat_bookmarks") == 1
    assert await store.row_count("library_compat_play_queue_items") == 2
    assert await store.row_count("library_compat_id_map") == 3
    assert await store.row_count("library_reference_tombstones") == 1
    assert await store.resolve_migrated_reference("favorite", f"alice:album:{RG}") == (
        "local_album",
        await store.resolve_album_alias(RG),
    )
    assert await store.resolve_migrated_reference(
        "compat_bookmark", f"alice:{TRACK_1}"
    ) == ("local_track", TRACK_1)
    assert progress[0] == "[upgrade] Checking catalog compatibility: 0/2 (0%)."
    assert "[upgrade] Checking catalog compatibility: 2/2 (100%)." in progress
    assert "[upgrade] Migrating identified catalog tracks: 2/2 (100%)." in progress
    assert "[upgrade] Validating migrated catalog." in progress
    assert any(item.endswith("14/14 (100%).") for item in progress)
    assert progress[-1] == "[upgrade] Recording migration completion marker."
    with sqlite3.connect(database) as connection:
        temporary_objects = connection.execute(
            "SELECT name FROM sqlite_master WHERE name IN "
            "('idx_bounded_legacy_library_files', "
            "'idx_bounded_migration_local_track_path', "
            "'library_migration_file_staging', "
            "'library_migration_review_staging')"
        ).fetchall()
    assert temporary_objects == []


@pytest.mark.asyncio
async def test_bounded_migration_keeps_identityless_active_files_local_and_playable(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    first_id = "99999999-9999-4999-8999-999999999991"
    second_id = "99999999-9999-4999-8999-999999999992"
    local_directory = root / "Unidentified Album"
    with sqlite3.connect(database) as connection:
        _insert_legacy_library_file(
            connection,
            file_id=first_id,
            path=local_directory / "01.flac",
            title="Local First",
            track_number=1,
            release_group_mbid=None,
        )
        _insert_legacy_library_file(
            connection,
            file_id=second_id,
            path=local_directory / "02.flac",
            title="Local Second",
            track_number=2,
            release_group_mbid=None,
        )
        connection.execute(
            "INSERT INTO playlist_tracks VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                "playlist-track-local-only",
                "playlist-1",
                2,
                "Local First",
                "Local Artist",
                "Unidentified Album",
                "malformed-release-group",
                "malformed-artist",
                first_id,
                None,
                "local",
                '["local"]',
                "flac",
                1,
                1,
                180,
                "c",
                None,
                first_id,
            ),
        )
        connection.execute(
            "INSERT INTO playlist_tracks VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                "playlist-track-local-only-conflict",
                "playlist-1",
                3,
                "Local First",
                "Local Artist",
                "Unidentified Album",
                RG,
                ARTIST_1,
                first_id,
                None,
                "local",
                '["local"]',
                "flac",
                1,
                1,
                180,
                "d",
                None,
                first_id,
            ),
        )
    progress: list[str] = []
    store, migrator = _migrator(database, root, progress, batch_size=1)

    outcome = await migrator.migrate("bounded-identityless", now=100)

    assert outcome.report.state == "applied"
    assert outcome.blocker_count == 0
    assert (outcome.report.identified_albums, outcome.report.identified_tracks) == (
        1,
        2,
    )
    assert (outcome.report.local_only_albums, outcome.report.local_only_tracks) == (
        3,
        4,
    )
    counts = {
        count.kind: count
        for count in outcome.report.reference_counts
        if count.user_id is None
    }
    assert (counts["library_file"].source, counts["library_file"].mapped) == (4, 4)
    with sqlite3.connect(database) as connection:
        tracks = connection.execute(
            "SELECT id, local_album_id FROM local_tracks WHERE id IN (?, ?) ORDER BY id",
            (first_id, second_id),
        ).fetchall()
        identities = connection.execute(
            "SELECT COUNT(*) FROM local_album_external_identities "
            "WHERE local_album_id = ?",
            (tracks[0][1],),
        ).fetchone()[0]
        reviews = connection.execute(
            "SELECT local_track_id, state, reason_code "
            "FROM library_identification_reviews WHERE local_track_id IN (?, ?) "
            "ORDER BY local_track_id",
            (first_id, second_id),
        ).fetchall()
        playlist_reference = connection.execute(
            "SELECT local_track_id, reference_tombstone_id "
            "FROM library_playlist_tracks WHERE id = ?",
            ("playlist-track-local-only",),
        ).fetchone()
        conflicting_playlist_reference = connection.execute(
            "SELECT local_track_id, reference_tombstone_id "
            "FROM library_playlist_tracks WHERE id = ?",
            ("playlist-track-local-only-conflict",),
        ).fetchone()
    assert tracks == [(first_id, tracks[0][1]), (second_id, tracks[0][1])]
    assert identities == 0
    assert reviews == [
        (first_id, "needs_review", "legacy_missing_release_group_id"),
        (second_id, "needs_review", "legacy_missing_release_group_id"),
    ]
    assert playlist_reference == (first_id, None)
    assert conflicting_playlist_reference[0] is None
    assert conflicting_playlist_reference[1] is not None
    assert (
        "[upgrade] Migrating local-only catalog tracks: 2/2 (100%)." in progress
    )


@pytest.mark.asyncio
async def test_bounded_local_only_migration_rolls_up_disc_directories_across_batches(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    first_id = "99999999-9999-4999-8999-999999999981"
    second_id = "99999999-9999-4999-8999-999999999982"
    with sqlite3.connect(database) as connection:
        _insert_legacy_library_file(
            connection,
            file_id=first_id,
            path=root / "Multi Disc" / "CD1" / "01.flac",
            title="Disc One",
            track_number=1,
            release_group_mbid=None,
        )
        _insert_legacy_library_file(
            connection,
            file_id=second_id,
            path=root / "Multi Disc" / "Disc 2" / "01.flac",
            title="Disc Two",
            track_number=1,
            release_group_mbid=None,
        )
    store, migrator = _migrator(database, root, [], batch_size=1)

    outcome = await migrator.migrate("bounded-multidisc-local", now=100)

    assert outcome.report.state == "applied"
    with sqlite3.connect(database) as connection:
        albums = connection.execute(
            "SELECT local_album_id FROM local_tracks WHERE id IN (?, ?) ORDER BY id",
            (first_id, second_id),
        ).fetchall()
    assert albums == [(albums[0][0],), (albums[0][0],)]


@pytest.mark.asyncio
async def test_bounded_local_only_migration_joins_missing_tag_to_unambiguous_group(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    tagged_id = "99999999-9999-4999-8999-999999999983"
    untagged_id = "99999999-9999-4999-8999-999999999984"
    directory = root / "Mixed Tags"
    with sqlite3.connect(database) as connection:
        _insert_legacy_library_file(
            connection,
            file_id=tagged_id,
            path=directory / "01.flac",
            title="Tagged",
            track_number=1,
            release_group_mbid=None,
            album_title="One Album",
        )
        _insert_legacy_library_file(
            connection,
            file_id=untagged_id,
            path=directory / "02.flac",
            title="Untagged",
            track_number=2,
            release_group_mbid=None,
            album_title=None,
        )
    store, migrator = _migrator(database, root, [], batch_size=1)

    outcome = await migrator.migrate("bounded-missing-album-tag", now=100)

    assert outcome.report.state == "applied"
    with sqlite3.connect(database) as connection:
        tracks = connection.execute(
            "SELECT t.id, t.local_album_id, a.title, t.album_title, "
            "t.tag_album_title, t.metadata_incomplete FROM local_tracks t "
            "JOIN local_albums a ON a.id = t.local_album_id "
            "WHERE t.id IN (?, ?) ORDER BY t.id",
            (tagged_id, untagged_id),
        ).fetchall()
    assert tracks == [
        (tagged_id, tracks[0][1], "One Album", "One Album", "One Album", 0),
        (untagged_id, tracks[0][1], "One Album", "One Album", "", 1),
    ]


@pytest.mark.asyncio
async def test_bounded_migration_maps_unambiguous_malformed_album_reference(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    file_id = "99999999-9999-4999-8999-999999999985"
    malformed_release_group = "legacy-release-group"
    with sqlite3.connect(database) as connection:
        _insert_legacy_library_file(
            connection,
            file_id=file_id,
            path=root / "Malformed Album" / "01.flac",
            title="Local Track",
            track_number=1,
            release_group_mbid=malformed_release_group,
        )
        connection.execute(
            "INSERT INTO user_favorites VALUES ('alice', 'album', ?, 4)",
            (malformed_release_group,),
        )
    store, migrator = _migrator(database, root, [], batch_size=1)

    outcome = await migrator.migrate("bounded-malformed-album-reference", now=100)

    assert outcome.report.state == "applied"
    with sqlite3.connect(database) as connection:
        track = connection.execute(
            "SELECT local_album_id FROM local_tracks WHERE id = ?", (file_id,)
        ).fetchone()
        alias = connection.execute(
            "SELECT local_album_id, kind FROM local_album_aliases WHERE alias = ?",
            (malformed_release_group,),
        ).fetchone()
        favorite = connection.execute(
            "SELECT item_id FROM library_user_favorites "
            "WHERE user_id = 'alice' AND item_kind = 'album' AND item_id = ?",
            (track[0],),
        ).fetchone()
    assert alias == (track[0], "compat_migration")
    assert favorite == (track[0],)


@pytest.mark.asyncio
async def test_bounded_local_only_migration_uses_unicode_safe_tag_and_path_fallbacks(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    whitespace_id = "99999999-9999-4999-8999-999999999986"
    tagged_id = "99999999-9999-4999-8999-999999999987"
    empty_id = "99999999-9999-4999-8999-999999999988"
    with sqlite3.connect(database) as connection:
        _insert_legacy_library_file(
            connection,
            file_id=whitespace_id,
            path=root / "Unicode Tags" / "01.flac",
            title="Whitespace Album",
            track_number=1,
            release_group_mbid=None,
            album_title="\u00a0",
        )
        _insert_legacy_library_file(
            connection,
            file_id=tagged_id,
            path=root / "Unicode Tags" / "02.flac",
            title="Tagged Album",
            track_number=2,
            release_group_mbid=None,
            album_title="Real Album",
        )
        _insert_legacy_library_file(
            connection,
            file_id=empty_id,
            path=root / "Folder Fallback" / "filename-title.flac",
            title="",
            track_number=1,
            release_group_mbid=None,
            album_title=None,
        )
    store, migrator = _migrator(database, root, [], batch_size=1)

    outcome = await migrator.migrate("bounded-local-fallbacks", now=100)

    assert outcome.report.state == "applied"
    with sqlite3.connect(database) as connection:
        mixed = connection.execute(
            "SELECT t.id, t.local_album_id, a.title, t.album_title, "
            "t.metadata_incomplete FROM local_tracks t "
            "JOIN local_albums a ON a.id = t.local_album_id "
            "WHERE t.id IN (?, ?) ORDER BY t.id",
            (whitespace_id, tagged_id),
        ).fetchall()
        fallback = connection.execute(
            "SELECT t.title, t.album_title, a.title, t.tag_album_title, "
            "t.metadata_incomplete FROM local_tracks t "
            "JOIN local_albums a ON a.id = t.local_album_id WHERE t.id = ?",
            (empty_id,),
        ).fetchone()
    assert mixed == [
        (whitespace_id, mixed[0][1], "Real Album", "Real Album", 1),
        (tagged_id, mixed[0][1], "Real Album", "Real Album", 0),
    ]
    assert fallback == (
        "filename-title",
        "Folder Fallback",
        "Folder Fallback",
        "",
        1,
    )


@pytest.mark.asyncio
async def test_bounded_migration_refuses_ambiguous_malformed_album_reference(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    malformed_release_group = "shared-legacy-release"
    with sqlite3.connect(database) as connection:
        for suffix in ("1", "2"):
            _insert_legacy_library_file(
                connection,
                file_id=f"99999999-9999-4999-8999-99999999999{suffix}",
                path=root / f"Ambiguous {suffix}" / "01.flac",
                title=f"Track {suffix}",
                track_number=1,
                release_group_mbid=malformed_release_group,
                album_title=f"Album {suffix}",
            )
        connection.execute(
            "INSERT INTO user_favorites VALUES ('alice', 'album', ?, 4)",
            (malformed_release_group,),
        )
    store, migrator = _migrator(database, root, [], batch_size=1)

    outcome = await migrator.migrate("bounded-ambiguous-malformed", now=100)

    assert outcome.report.state == "blocked"
    assert outcome.blocker_reason_counts == {"favorite_unresolved": 1}
    with sqlite3.connect(database) as connection:
        aliases = connection.execute(
            "SELECT COUNT(*) FROM local_album_aliases WHERE alias = ?",
            (malformed_release_group,),
        ).fetchone()[0]
    assert aliases == 0


@pytest.mark.asyncio
async def test_bounded_migration_omits_invalid_optional_recording_identity(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    with sqlite3.connect(database) as connection:
        connection.execute(
            "UPDATE library_files SET recording_mbid = 'not-a-recording-mbid' "
            "WHERE id = ?",
            (TRACK_1,),
        )
    store, migrator = _migrator(database, root, [])

    outcome = await migrator.migrate("bounded-invalid-recording", now=100)

    assert outcome.report.state == "applied"
    assert outcome.blocker_count == 0
    with sqlite3.connect(database) as connection:
        identity = connection.execute(
            "SELECT recording_mbid FROM local_track_external_identities "
            "WHERE local_track_id = ?",
            (TRACK_1,),
        ).fetchone()
        review = connection.execute(
            "SELECT state, reason_code FROM library_identification_reviews "
            "WHERE local_track_id = ? AND reason_code = ?",
            (TRACK_1, "legacy_invalid_recording_id"),
        ).fetchone()
    assert identity is None
    assert review == ("needs_review", "legacy_invalid_recording_id")


@pytest.mark.asyncio
async def test_bounded_migration_does_not_treat_dashless_artist_id_as_mbid(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    synthetic_artist_id = "d4ee74d98c7a6f053a0ebffd0ed5fccb"
    with sqlite3.connect(database) as connection:
        connection.execute(
            "UPDATE library_files SET artist_mbid = ? WHERE id = ?",
            (synthetic_artist_id, TRACK_1),
        )
        connection.execute(
            "DELETE FROM user_favorites WHERE item_kind = 'artist' AND item_id = ?",
            (ARTIST_1,),
        )
    _store, migrator = _migrator(database, root, [])

    outcome = await migrator.migrate("bounded-synthetic-artist", now=100)

    assert outcome.report.state == "applied", outcome.blocker_reason_counts
    with sqlite3.connect(database) as connection:
        track_artist = connection.execute(
            "SELECT artist.id, identity.provider_artist_id "
            "FROM local_track_artists credit "
            "JOIN local_artists artist ON artist.id = credit.local_artist_id "
            "LEFT JOIN local_artist_external_identities identity "
            "ON identity.local_artist_id = artist.id "
            "WHERE credit.local_track_id = ? AND credit.position = 0",
            (TRACK_1,),
        ).fetchone()
        synthetic_identity = connection.execute(
            "SELECT local_artist_id FROM local_artist_external_identities "
            "WHERE provider_artist_id = ?",
            (synthetic_artist_id,),
        ).fetchone()
    assert track_artist is not None
    assert track_artist[1] is None
    assert synthetic_identity is None


@pytest.mark.asyncio
async def test_bounded_migration_reports_outside_root_before_catalog_apply(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    with sqlite3.connect(database) as connection:
        _insert_legacy_library_file(
            connection,
            file_id="99999999-9999-4999-8999-999999999993",
            path=tmp_path / "Former Mount" / "outside.flac",
            title="Outside",
            track_number=1,
            release_group_mbid=RG,
        )
    progress: list[str] = []
    store, migrator = _migrator(database, root, progress)

    async def unexpected_apply(*_args, **_kwargs):
        raise AssertionError("catalog Apply must not begin after a failed preflight")

    monkeypatch.setattr(store, "apply_legacy_catalog_bundle", unexpected_apply)

    outcome = await migrator.migrate("bounded-outside-root", now=100)

    assert outcome.report.state == "blocked"
    assert outcome.blocker_count == 1
    assert outcome.blocker_reason_counts == {"library_file_outside_roots": 1}
    counts = {
        count.kind: count
        for count in outcome.report.reference_counts
        if count.user_id is None
    }
    assert counts["library_file"].unresolved == 1
    assert progress[-1] == (
        "[upgrade] Library path preflight stopped before migration: "
        "1 saved record is outside the configured library roots."
    )
    assert not any("Migrating identified catalog tracks" in item for item in progress)


@pytest.mark.asyncio
async def test_bounded_migration_reports_outside_review_path_before_catalog_apply(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    with sqlite3.connect(database) as connection:
        connection.execute(
            "UPDATE manual_review_queue SET file_path = ? WHERE id = 1",
            (str(tmp_path / "Former Mount" / "review.flac"),),
        )
    progress: list[str] = []
    store, migrator = _migrator(database, root, progress)

    async def unexpected_apply(*_args, **_kwargs):
        raise AssertionError("catalog Apply must not begin after a failed preflight")

    monkeypatch.setattr(store, "apply_legacy_catalog_bundle", unexpected_apply)

    outcome = await migrator.migrate("bounded-outside-review", now=100)

    assert outcome.report.state == "blocked"
    assert outcome.blocker_count == 1
    assert outcome.blocker_reason_counts == {"review_row_outside_roots": 1}
    counts = {
        count.kind: count
        for count in outcome.report.reference_counts
        if count.user_id is None
    }
    assert counts["review_row"].unresolved == 1
    assert progress[-1].startswith("[upgrade] Library path preflight stopped")
    assert not any("Migrating identified catalog tracks" in item for item in progress)


@pytest.mark.asyncio
async def test_bounded_migration_retains_provider_era_and_removed_jellyfin_references(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
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
    store, migrator = _migrator(database, root, [])

    outcome = await migrator.migrate("bounded-provider-era", now=100)

    assert outcome.blocker_count == 0
    with sqlite3.connect(database) as connection:
        history = connection.execute(
            "SELECT local_track_id FROM library_play_history WHERE id = ?",
            ("provider-era-history",),
        ).fetchone()
        mappings = connection.execute(
            "SELECT jf_id, kind, internal_id FROM library_compat_id_map "
            "WHERE jf_id IN (?, ?, ?, ?, ?) ORDER BY jf_id",
            ("d" * 32, "e" * 32, "f" * 32, "g" * 32, "h" * 32),
        ).fetchall()
    assert history == (TRACK_1,)
    assert mappings == [
        ("d" * 32, "library", "music"),
        ("e" * 32, "track", "removed-track"),
        ("f" * 32, "genre", "Electronic"),
        ("g" * 32, "playlist", "playlist-1"),
        ("h" * 32, "playlist", "removed-playlist"),
    ]
    counts = {
        count.kind: count
        for count in outcome.report.reference_counts
        if count.user_id is None
    }
    assert counts["history"].source == counts["history"].mapped == 2
    assert counts["playlist_track"].tombstoned == 2
    assert counts["jellyfin_id_map"].tombstoned == 2


@pytest.mark.asyncio
async def test_bounded_migration_retains_ambiguous_history_unlinked(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    with sqlite3.connect(database) as connection:
        connection.execute(
            "UPDATE library_files SET track_title = 'First', artist_name = 'Artist One' "
            "WHERE id != ?",
            (TRACK_1,),
        )
        connection.execute(
            "INSERT INTO play_history VALUES (?,?,?,?,?,?,?,?,?,?)",
            (
                "ambiguous-history",
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
    store, migrator = _migrator(database, root, [])

    outcome = await migrator.migrate("bounded-retained", now=100)

    assert outcome.report.state == "applied"
    assert outcome.blocker_count == 0
    counts = {
        count.kind: count
        for count in outcome.report.reference_counts
        if count.user_id is None
    }
    assert counts["history"].unresolved == 0
    assert counts["history"].retained == 1
    with sqlite3.connect(database) as connection:
        retained = connection.execute(
            "SELECT local_track_id, local_album_id, local_artist_id, track_name, "
            "artist_name, album_name FROM library_play_history WHERE id = ?",
            ("ambiguous-history",),
        ).fetchone()
    assert retained == (
        None,
        None,
        None,
        "First",
        "Artist One",
        "Compilation",
    )
    assert await store.row_count("library_migration_markers") == 1
    assert await store.validate_migration_references() == 0


@pytest.mark.asyncio
async def test_bounded_migration_disambiguates_shared_recording_by_release_group(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    other_release_group = "99999999-9999-4999-8999-999999999999"
    with sqlite3.connect(database) as connection:
        connection.execute(
            "UPDATE library_files SET recording_mbid = ?, "
            "release_group_mbid = ?, track_title = 'First', artist_name = 'Artist One' "
            "WHERE id = ?",
            (RECORDING_1, other_release_group, TRACK_2),
        )
        connection.execute(
            "INSERT INTO play_history VALUES (?,?,?,?,?,?,?,?,?,?)",
            (
                "shared-recording-history",
                "alice",
                "First",
                "Artist One",
                "Compilation",
                RECORDING_1,
                RG,
                180000,
                None,
                "2025-01-01T00:00:00Z",
            ),
        )
    store, migrator = _migrator(database, root, [])

    outcome = await migrator.migrate("bounded-shared-recording", now=100)

    assert outcome.blocker_count == 0
    counts = {
        count.kind: count
        for count in outcome.report.reference_counts
        if count.user_id is None
    }
    assert counts["history"].mapped == 2
    assert counts["history"].retained == 0
    with sqlite3.connect(database) as connection:
        resolved = connection.execute(
            "SELECT local_track_id FROM library_play_history WHERE id = ?",
            ("shared-recording-history",),
        ).fetchone()
    assert resolved == (TRACK_1,)


@pytest.mark.asyncio
async def test_bounded_migration_disambiguates_shared_recording_by_text(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    with sqlite3.connect(database) as connection:
        connection.execute(
            "UPDATE library_files SET recording_mbid = ? WHERE id = ?",
            (RECORDING_1, TRACK_2),
        )
        connection.execute(
            "INSERT INTO play_history VALUES (?,?,?,?,?,?,?,?,?,?)",
            (
                "shared-recording-text-history",
                "alice",
                "First",
                "Artist One",
                "Compilation",
                RECORDING_1,
                None,
                180000,
                None,
                "2025-01-01T00:00:00Z",
            ),
        )
    store, migrator = _migrator(database, root, [])

    outcome = await migrator.migrate("bounded-shared-recording-text", now=100)

    assert outcome.blocker_count == 0
    with sqlite3.connect(database) as connection:
        resolved = connection.execute(
            "SELECT local_track_id FROM library_play_history WHERE id = ?",
            ("shared-recording-text-history",),
        ).fetchone()
    assert resolved == (TRACK_1,)


@pytest.mark.asyncio
async def test_bounded_migration_is_idempotent(tmp_path: Path) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    with sqlite3.connect(database) as connection:
        connection.execute("DELETE FROM library_album_meta")
        connection.execute("UPDATE library_albums SET cover_url = NULL")
    store, first = _migrator(database, root, [], batch_size=2)

    initial = await first.migrate("bounded-repeat", now=100)
    counts = {
        table: await store.row_count(table)
        for table in (
            "local_albums",
            "local_tracks",
            "library_migration_provenance",
            "library_reference_tombstones",
        )
    }
    revision = await store.get_catalog_revision()
    _, second = _migrator(database, root, [], batch_size=1)

    repeated = await second.migrate("bounded-repeat", now=101)

    assert initial.report.state == repeated.report.state == "applied"
    assert initial.report == repeated.report
    assert {table: await store.row_count(table) for table in counts} == counts
    assert await store.get_catalog_revision() == revision
    with sqlite3.connect(database) as connection:
        artwork = connection.execute(
            "SELECT cover_url, source, source_locator FROM local_album_artwork "
            "WHERE local_album_id = ?",
            (await store.resolve_album_alias(RG),),
        ).fetchone()
    assert artwork == (None, "provider", RG)

    with sqlite3.connect(database) as connection:
        connection.execute(
            "UPDATE library_files SET track_title = 'Changed' WHERE id = ?",
            (TRACK_1,),
        )
    _, changed = _migrator(database, root, [], batch_size=1)
    with pytest.raises(StaleRevisionError, match="migration input"):
        await changed.migrate("bounded-repeat", now=102)


@pytest.mark.asyncio
async def test_bounded_migration_resumes_after_a_committed_batch(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    store, interrupted = _migrator(database, root, [], batch_size=1)
    apply_bundle = store.apply_legacy_catalog_bundle
    calls = 0

    async def apply_then_interrupt(*args, **kwargs):
        nonlocal calls
        applied = await apply_bundle(*args, **kwargs)
        calls += 1
        if calls == 1:
            raise RuntimeError("simulated process interruption")
        return applied

    monkeypatch.setattr(store, "apply_legacy_catalog_bundle", apply_then_interrupt)
    with pytest.raises(RuntimeError, match="simulated process interruption"):
        await interrupted.migrate("bounded-resume", now=100)

    _, resumed = _migrator(database, root, [], batch_size=1)
    outcome = await resumed.migrate("bounded-resume", now=101)

    assert outcome.report.state == "applied"
    assert outcome.blocker_count == 0
    assert await store.row_count("local_tracks") == 4
    assert await store.row_count("library_migration_markers") == 1


@pytest.mark.asyncio
async def test_bounded_migration_resumes_after_a_committed_review_album(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    store, interrupted = _migrator(database, root, [], batch_size=1)
    apply_bundle = store.apply_legacy_catalog_bundle
    interrupted_review = False

    async def apply_then_interrupt(*args, **kwargs):
        nonlocal interrupted_review
        applied = await apply_bundle(*args, **kwargs)
        bundle = args[0]
        if bundle.album_identity is None and not interrupted_review:
            interrupted_review = True
            raise RuntimeError("simulated review interruption")
        return applied

    monkeypatch.setattr(store, "apply_legacy_catalog_bundle", apply_then_interrupt)
    with pytest.raises(RuntimeError, match="simulated review interruption"):
        await interrupted.migrate("bounded-review-resume", now=100)

    _, resumed = _migrator(database, root, [], batch_size=1)
    outcome = await resumed.migrate("bounded-review-resume", now=101)

    assert outcome.report.state == "applied"
    assert outcome.blocker_count == 0
    assert (outcome.report.local_only_albums, outcome.report.local_only_tracks) == (
        2,
        2,
    )
    assert await store.row_count("local_tracks") == 4
    assert await store.row_count("library_migration_markers") == 1


@pytest.mark.asyncio
async def test_bounded_migration_chunks_tracks_within_one_review_album(
    tmp_path: Path,
) -> None:
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    second_local_track = root / "Local Album" / "02.flac"
    with sqlite3.connect(database) as connection:
        connection.execute(
            "INSERT INTO manual_review_queue VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                5,
                str(second_local_track),
                "Another Local Song",
                "Local Artist",
                "Local Album",
                2025,
                2,
                1,
                "flac",
                195.0,
                310,
                None,
                None,
                "[]",
                "text_match",
                12.5,
                None,
                None,
            ),
        )
    store, migrator = _migrator(database, root, [], batch_size=1)

    outcome = await migrator.migrate("bounded-review-chunks", now=100)

    assert outcome.report.state == "applied"
    assert (outcome.report.local_only_albums, outcome.report.local_only_tracks) == (
        2,
        3,
    )
    assert await store.row_count("local_tracks") == 5


@pytest.mark.asyncio
async def test_double_migration_converges_catalog_exactly(tmp_path: Path) -> None:
    """Two full migrate() runs over one seeded mixed legacy database converge:
    the second run takes the completed-report short-circuit, does no phase
    work, and leaves every counted table identical to the first run."""
    root = tmp_path / "Music"
    compilation = root / "Compilation"
    compilation.mkdir(parents=True)
    (compilation / "01.flac").write_bytes(b"a" * 100)
    (compilation / "02.flac").write_bytes(b"b" * 200)
    database = tmp_path / "library.db"
    _create_source(database, root)
    with sqlite3.connect(database) as connection:
        _insert_legacy_library_file(
            connection,
            file_id="99999999-9999-4999-8999-000000000010",
            path=root / "Local Only" / "01.flac",
            title="Local Only Song",
            track_number=1,
            release_group_mbid=None,
        )

    def snapshot() -> dict[str, int]:
        tables = (
            "local_albums",
            "local_tracks",
            "local_album_aliases",
            "local_artist_aliases",
            "library_migration_provenance",
            "library_user_favorites",
            "library_play_history",
            "library_playlist_tracks",
            "library_identification_reviews",
        )
        with sqlite3.connect(database) as connection:
            return {
                table: int(
                    connection.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0]
                )
                for table in tables
            }

    first_store, first_migrator = _migrator(database, root, [])
    first = await first_migrator.migrate("double-run", now=100)
    assert first.report.state == "applied"
    after_first = snapshot()
    assert after_first["local_tracks"] > 0
    assert after_first["library_migration_provenance"] > 0

    second_progress: list[str] = []
    _, second_migrator = _migrator(database, root, second_progress)
    second = await second_migrator.migrate("double-run", now=200)

    # Completed-report short-circuit: no phase work, zero blockers/skips.
    assert second.report == first.report
    assert second.blocker_count == 0
    assert second.skipped_counts == {}
    assert second.phase_timings_ms == {}
    assert not any(
        "Checking catalog compatibility" in item for item in second_progress
    )
    assert snapshot() == after_first


@pytest.mark.asyncio
async def test_first_run_skips_legacy_file_already_in_catalog(
    tmp_path: Path,
) -> None:
    """GH-380: first-run migrate() must skip legacy rows already owned."""
    root = tmp_path / "Music"
    root.mkdir()
    database = tmp_path / "library.db"
    _create_source(database, root)
    owned_path = root / "Local Owned" / "01.flac"
    store = NativeLibraryStore(database, threading.Lock())
    resolver = LibraryPolicyResolver(
        TypedLibrarySettings(
            library_roots=[
                LibraryRootSettings(
                    id="root-1",
                    path=str(root),
                    label="Music",
                    policy="automatic",
                )
            ]
        )
    )
    with sqlite3.connect(database) as connection:
        _insert_legacy_library_file(
            connection,
            file_id="local-owned-1",
            path=owned_path,
            title="Owned Song",
            track_number=1,
            release_group_mbid=None,
        )
        connection.execute(
            "INSERT OR IGNORE INTO local_artists (id, display_name, folded_name, "
            "kind, created_at, updated_at) VALUES "
            "('pre-existing-artist', 'Artist', 'artist', 'group', 1, 1)"
        )
        connection.execute(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, "
            "title_folded, album_artist_id, grouping_source, created_at, "
            "updated_at) VALUES ('pre-existing-album', 'root-1', "
            "'fk-pre-existing', 'Owned', 'owned', 'pre-existing-artist', "
            "'automatic', 1, 1)"
        )
        connection.execute(
            "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, "
            "relative_path, path_hash, file_size_bytes, file_mtime_ns, "
            "stat_revision, title, title_folded, album_title, "
            "album_title_folded, availability, ingest_source, imported_at, "
            "membership_source, applied_policy, file_format) VALUES "
            "('pre-existing-track', 'pre-existing-album', 'root-1', ?, ?, "
            "'hash-owned', 100, 200, 'stat', 'Owned Song', 'owned song', "
            "'Owned', 'owned', 'indexed', 'download', 1, 'automatic', "
            "'automatic', 'flac')",
            (str(owned_path), "Local Owned/01.flac"),
        )
    migrator = BoundedLegacyCatalogMigrator(
        store,
        resolver,
        emit_progress=lambda _message: None,
        batch_size=1,
        skip_unmappable_paths=True,
    )
    outcome = await migrator.migrate("bounded-first-run-owned", now=100)
    assert outcome.report.state == "applied"
    assert outcome.skipped_counts.get("scan_owned_library_file", 0) == 1
    assert "local-owned-1" in migrator._scan_owned_file_ids
    with sqlite3.connect(database) as connection:
        collisions = connection.execute(
            "SELECT COUNT(*) FROM local_tracks WHERE file_path = ?",
            (str(owned_path),),
        ).fetchone()[0]
    assert collisions == 1
