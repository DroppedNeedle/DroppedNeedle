import asyncio
import sqlite3
import threading
from pathlib import Path
from unittest.mock import AsyncMock

import msgspec
import pytest
from starlette.requests import Request

from core.exception_handlers import (
    revision_overflow_error_handler,
    stale_revision_error_handler,
)
from core.exceptions import ConflictError, RevisionOverflowError, StaleRevisionError
from api.v1.schemas.library_policies import LibraryRootSettings, TypedLibrarySettings
from infrastructure.persistence.native_library_store import (
    MAX_REVISION,
    UNKNOWN_ARTIST_ID,
    VARIOUS_ARTISTS_ID,
    NativeLibraryStore,
)
from services.native.target_library_policy_service import TargetLibraryPolicyService
from models.identification import (
    CandidateEvidence,
    IdentificationAttempt,
    IdentificationEvidenceRecord,
)
from models.library_work import (
    IdentificationJob,
    MigrationProvenance,
    OperationJob,
    OperationWorkItem,
    RepairFinding,
    ReviewDecision,
    ScanInventoryItem,
    ScanRun,
    ScanScope,
)
from models.local_catalog import (
    CatalogMembership,
    LocalAlbum,
    LocalAlbumAlias,
    LocalAlbumExternalIdentity,
    LocalArtist,
    LocalArtistAlias,
    LocalArtistCredit,
    LocalArtistExternalIdentity,
    LocalArtworkAssociation,
    LocalTrack,
    LocalTrackExternalIdentity,
)


def _seed_auth(path: Path) -> None:
    with sqlite3.connect(path) as connection:
        connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
        connection.executemany(
            "INSERT INTO auth_users(id) VALUES (?)", [("admin",), ("worker",)]
        )


def test_candidate_identity_lookups_use_normalized_indexes(db_path: Path) -> None:
    NativeLibraryStore(db_path, threading.Lock())
    with sqlite3.connect(db_path) as connection:
        album_plan = connection.execute(
            "EXPLAIN QUERY PLAN SELECT release_group_mbid "
            "FROM local_album_external_identities "
            "WHERE lower(release_group_mbid) IN (?)",
            ("album-1",),
        ).fetchall()
        release_plan = connection.execute(
            "EXPLAIN QUERY PLAN SELECT release_mbid "
            "FROM local_album_external_identities "
            "WHERE lower(release_mbid) = lower(?)",
            ("release-1",),
        ).fetchall()
        artist_plan = connection.execute(
            "EXPLAIN QUERY PLAN SELECT provider_artist_id "
            "FROM local_artist_external_identities "
            "WHERE lower(provider_artist_id) IN (?)",
            ("artist-1",),
        ).fetchall()

    assert any("idx_local_album_identity_rg_lower" in row[3] for row in album_plan)
    assert any(
        "idx_local_album_identity_release_lower" in row[3] for row in release_plan
    )
    assert any(
        "idx_local_artist_identity_provider_lower" in row[3] for row in artist_plan
    )


@pytest.fixture
def db_path(tmp_path: Path) -> Path:
    path = tmp_path / "library.db"
    _seed_auth(path)
    return path


@pytest.fixture
def store(db_path: Path) -> NativeLibraryStore:
    return NativeLibraryStore(db_path, threading.Lock())


def _artist(artist_id: str = "artist-1", name: str = "Artist") -> LocalArtist:
    return LocalArtist(
        id=artist_id,
        display_name=name,
        folded_name=name.casefold(),
        kind="person",
        created_at=1,
        updated_at=1,
    )


def _membership(
    suffix: str = "1", *, compilation: bool = False, with_track: bool = True
) -> CatalogMembership:
    artist = _artist(f"artist-{suffix}", f"Artist {suffix}")
    album_artist = VARIOUS_ARTISTS_ID if compilation else artist.id
    album_artist_name = "Various Artists" if compilation else artist.display_name
    album = LocalAlbum(
        id=f"album-{suffix}",
        root_id="root-1",
        grouping_key=f"group-{suffix}",
        title=f"Album {suffix}",
        album_artist_id=album_artist,
        album_artist_name=album_artist_name,
        is_compilation=compilation,
        created_at=1,
        updated_at=1,
    )
    tracks = []
    credits: dict[str, list[LocalArtistCredit]] = {}
    if with_track:
        track = LocalTrack(
            id=f"track-{suffix}",
            local_album_id=album.id,
            root_id="root-1",
            file_path=f"/music/{suffix}.flac",
            relative_path=f"{suffix}.flac",
            path_hash=f"hash-{suffix}",
            file_size_bytes=100,
            file_mtime_ns=200,
            stat_revision=f"stat-{suffix}",
            title=f"Track {suffix}",
            artist_name=artist.display_name,
            album_title=album.title,
            album_artist_name=album_artist_name,
            file_format="flac",
            imported_at=1,
        )
        tracks = [track]
        credits[track.id] = [LocalArtistCredit(local_artist_id=artist.id, position=0)]
    return CatalogMembership(
        album=album,
        artists=[artist],
        tracks=tracks,
        track_credits=credits,
    )


def _attempt(attempt_id: str, album_id: str = "album-1") -> IdentificationAttempt:
    return IdentificationAttempt(
        id=attempt_id,
        local_album_id=album_id,
        input_tag_revision="tag-1",
        input_policy_revision="policy-1",
        input_file_revision="file-1",
        matcher_version="matcher-1",
        state="completed",
        terminal_reason_code="needs_review",
        candidate_count=1,
        started_at=1,
        completed_at=2,
    )


def _evidence(evidence_id: str, attempt_id: str) -> IdentificationEvidenceRecord:
    return IdentificationEvidenceRecord(
        id=evidence_id,
        attempt_id=attempt_id,
        candidate_key="candidate-1",
        evidence=CandidateEvidence(release_group_mbid="rg-1", album_title="Album 1"),
        created_at=2,
    )


def _scalar(path: Path, sql: str, parameters: tuple = ()) -> int | str | None:
    with sqlite3.connect(path) as connection:
        row = connection.execute(sql, parameters).fetchone()
        return row[0] if row is not None else None


def test_domain_types_allow_local_only_catalog_and_nullable_provider_fields() -> None:
    membership = _membership()
    identity = LocalAlbumExternalIdentity(
        local_album_id=membership.album.id,
        release_group_mbid="rg-1",
    )

    assert membership.album.id == "album-1"
    assert identity.release_mbid is None
    assert identity.attempt_id is None
    assert (
        LocalArtworkAssociation(
            local_album_id="album-1", cover_url=None, source="embedded"
        ).source_locator
        is None
    )


@pytest.mark.asyncio
async def test_schema_repairs_release_alias_stored_as_release_group_id(
    db_path: Path,
) -> None:
    lock = threading.Lock()
    first = NativeLibraryStore(db_path, lock)
    await first.create_catalog_membership(_membership())
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "CREATE TABLE mbid_resolution_map ("
            "source_mbid_lower TEXT PRIMARY KEY, source_mbid TEXT NOT NULL, "
            "release_group_mbid TEXT)"
        )
        connection.execute(
            "INSERT INTO mbid_resolution_map VALUES (?, ?, ?)",
            ("release-edition", "release-edition", "canonical-rg"),
        )
        connection.execute(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, decision_source, "
            "selected_at) VALUES (?, 'musicbrainz', ?, 'embedded', 1)",
            ("album-1", "release-edition"),
        )

    repaired = NativeLibraryStore(db_path, lock)
    NativeLibraryStore(db_path, lock)

    with sqlite3.connect(db_path) as connection:
        stored_identity = connection.execute(
            "SELECT release_group_mbid, release_mbid, row_revision "
            "FROM local_album_external_identities WHERE local_album_id = 'album-1'"
        ).fetchone()
        revision = connection.execute(
            "SELECT value FROM library_catalog_revision WHERE singleton = 1"
        ).fetchone()[0]
        alias = connection.execute(
            "SELECT local_album_id, kind FROM local_album_aliases "
            "WHERE alias = 'release-edition'"
        ).fetchone()
    assert stored_identity == ("canonical-rg", "release-edition", 2)
    assert revision == 2
    assert alias == ("album-1", "compat_migration")
    assert len(await repaired.get_target_album_tracks("release-edition")) == 1


@pytest.mark.asyncio
async def test_schema_is_idempotent_and_contains_complete_target_surface(
    db_path: Path,
) -> None:
    lock = threading.Lock()
    first = NativeLibraryStore(db_path, lock)
    second = NativeLibraryStore(db_path, lock)

    required = {
        "local_artists",
        "local_albums",
        "local_tracks",
        "local_artist_aliases",
        "local_album_aliases",
        "local_album_artwork",
        "audio_fingerprint_outcomes",
        "library_identification_attempts",
        "library_identification_evidence",
        "library_identification_reviews",
        "library_identification_jobs",
        "library_operation_jobs",
        "library_operation_work",
        "library_catalog_actions",
        "library_policy_state",
        "library_policy_transitions",
        "library_scan_runs",
        "library_scan_inventory",
        "library_scan_grouping_contexts",
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
    }
    assert required <= await first.table_names()
    assert await second.foreign_keys_enabled() is True
    assert await first.get_catalog_revision() == 0
    assert await first.get_stream_revision("scan") == 0
    assert _scalar(db_path, "SELECT COUNT(*) FROM local_artists") == 2


def test_bulk_preview_staging_columns_upgrade_idempotently(db_path: Path) -> None:
    lock = threading.Lock()
    NativeLibraryStore(db_path, lock)
    with sqlite3.connect(db_path) as connection:
        for column in (
            "subject_count",
            "cursor_review_id",
            "cursor_updated_at",
            "summary_json",
            "state",
        ):
            connection.execute(
                f"ALTER TABLE library_bulk_review_previews DROP COLUMN {column}"
            )
        for column in ("staging_cursor", "staging_state"):
            connection.execute(
                f"ALTER TABLE library_bulk_review_snapshots DROP COLUMN {column}"
            )

    NativeLibraryStore(db_path, lock)
    NativeLibraryStore(db_path, lock)

    with sqlite3.connect(db_path) as connection:
        preview_columns = {
            row[1]
            for row in connection.execute(
                "PRAGMA table_info('library_bulk_review_previews')"
            )
        }
        snapshot_columns = {
            row[1]
            for row in connection.execute(
                "PRAGMA table_info('library_bulk_review_snapshots')"
            )
        }
    assert {
        "state",
        "summary_json",
        "cursor_updated_at",
        "cursor_review_id",
        "subject_count",
    } <= preview_columns
    assert {"staging_state", "staging_cursor"} <= snapshot_columns


@pytest.mark.asyncio
async def test_committed_catalog_and_reference_writes_invalidate_consumers(
    db_path: Path,
) -> None:
    invalidator = AsyncMock()
    store = NativeLibraryStore(db_path, threading.Lock(), invalidator)

    await store.create_catalog_membership(_membership())
    await store.add_target_favorite("admin", "album", "album-1", 2)
    await store.get_target_track("track-1")

    assert invalidator.await_count == 2


@pytest.mark.asyncio
async def test_rolled_back_catalog_write_does_not_invalidate_consumers(
    db_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    invalidator = AsyncMock()
    store = NativeLibraryStore(db_path, threading.Lock(), invalidator)

    def fail(_connection: sqlite3.Connection) -> int:
        raise RuntimeError("injected")

    monkeypatch.setattr(store, "_bump_catalog", fail)
    with pytest.raises(RuntimeError, match="injected"):
        await store.create_catalog_membership(_membership())

    invalidator.assert_not_awaited()
    assert _scalar(db_path, "SELECT COUNT(*) FROM local_albums") == 0


@pytest.mark.asyncio
async def test_committed_write_is_not_reported_failed_when_cache_invalidation_fails(
    db_path: Path,
) -> None:
    invalidator = AsyncMock(side_effect=RuntimeError("cache unavailable"))
    store = NativeLibraryStore(db_path, threading.Lock(), invalidator)

    await store.create_catalog_membership(_membership())

    assert _scalar(db_path, "SELECT COUNT(*) FROM local_albums") == 1
    invalidator.assert_awaited_once_with()


@pytest.mark.asyncio
async def test_compilation_uses_stable_various_artist_and_ordered_credits(
    store: NativeLibraryStore, db_path: Path
) -> None:
    membership = _membership(compilation=True)
    membership.album_credits = [
        LocalArtistCredit(
            local_artist_id=VARIOUS_ARTISTS_ID,
            position=0,
            credited_name="Various Artists",
        ),
        LocalArtistCredit(
            local_artist_id="artist-1", position=1, credited_name="Artist 1"
        ),
    ]
    await store.create_catalog_membership(membership)

    with sqlite3.connect(db_path) as connection:
        rows = connection.execute(
            "SELECT position, local_artist_id FROM local_album_artists "
            "WHERE local_album_id = 'album-1' ORDER BY position"
        ).fetchall()
    assert rows == [(0, VARIOUS_ARTISTS_ID), (1, "artist-1")]
    assert (
        _scalar(
            db_path,
            "SELECT COUNT(*) FROM local_album_external_identities WHERE local_album_id = 'album-1'",
        )
        == 0
    )


@pytest.mark.asyncio
async def test_target_genre_projection_and_release_pins_ignore_legacy_authority(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership())
    with sqlite3.connect(db_path) as connection:
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute(
            "UPDATE local_tracks SET genre = 'Straße Pop', genre_folded = 'strasse pop' "
            "WHERE id = 'track-1'"
        )
        connection.execute(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, decision_source, selected_at) "
            "VALUES ('album-1', 'musicbrainz', 'target-rg', 'manual', 2)"
        )
        connection.execute(
            "INSERT INTO local_artist_external_identities "
            "(local_artist_id, provider, provider_artist_id, decision_source, selected_at) "
            "VALUES ('artist-1', 'musicbrainz', 'target-artist', 'manual', 2)"
        )
        connection.executemany(
            "INSERT INTO artist_genres VALUES (?, ?, ?)",
            [
                ("target-artist", "target-artist", '["Ambient"]'),
                ("legacy-artist", "legacy-artist", '["Ambient"]'),
            ],
        )
        connection.executemany(
            "INSERT INTO artist_genre_lookup VALUES (?, 'ambient')",
            [("target-artist",), ("legacy-artist",)],
        )
        connection.execute(
            "INSERT INTO album_release_pins VALUES "
            "('target-rg', 'legacy-release', 'admin', 'legacy-time')"
        )

    artists = await store.get_target_artists_by_genre("STRASSE POP", limit=50)
    albums = await store.get_target_albums_by_genre("STRASSE POP", limit=50)
    top_genres = await store.get_target_top_genres(limit=20)
    by_genre = await store.get_target_artists_for_genres(["STRASSE POP"])
    listed_albums, album_count = await store.list_target_albums(genre="STRASSE POP")
    listed_tracks, track_count = await store.list_target_tracks(genre="STRASSE POP")
    listed_genres = await store.list_target_genres()
    underrepresented = await store.get_target_underrepresented_genres(
        ["STRASSE POP"], threshold=2
    )

    assert [row["mbid"] for row in artists] == ["target-artist"]
    assert [row["mbid"] for row in albums] == ["target-rg"]
    assert top_genres == [("strasse pop", 1)]
    assert by_genre == {"strasse pop": ["target-artist"]}
    assert album_count == 1
    assert [row["release_group_mbid"] for row in listed_albums] == ["album-1"]
    assert track_count == 1
    assert [row["id"] for row in listed_tracks] == ["track-1"]
    assert listed_genres == [{"genre": "Straße Pop", "song_count": 1, "album_count": 1}]
    assert underrepresented == []
    assert await store.get_target_album_release_pin("target-rg") is None

    await store.set_target_album_release_pin(
        "target-rg", "target-release", "admin", "target-time"
    )
    assert await store.get_target_album_release_pin("album-1") == "target-release"
    assert (
        _scalar(
            db_path,
            "SELECT release_mbid FROM album_release_pins "
            "WHERE release_group_mbid = 'target-rg'",
        )
        == "legacy-release"
    )
    assert (
        _scalar(
            db_path,
            "SELECT release_mbid FROM library_album_release_pins "
            "WHERE local_album_id = 'album-1'",
        )
        == "target-release"
    )
    assert await store.clear_target_album_release_pin("target-rg") is True
    assert await store.get_target_album_release_pin("target-rg") is None
    assert (
        _scalar(
            db_path,
            "SELECT display_name FROM local_artists WHERE id = ?",
            (UNKNOWN_ARTIST_ID,),
        )
        == "Unknown Artist"
    )


@pytest.mark.asyncio
async def test_target_release_pins_reject_ambiguous_provider_album_identity(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership("pin-a"))
    await store.create_catalog_membership(_membership("pin-b"))
    with sqlite3.connect(db_path) as connection:
        connection.executemany(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, decision_source, selected_at) "
            "VALUES (?, 'musicbrainz', 'shared-rg', 'manual', 2)",
            [("album-pin-a",), ("album-pin-b",)],
        )

    await store.set_target_album_release_pin(
        "album-pin-a", "release-a", "admin", "target-time"
    )
    assert await store.get_target_album_release_pin("album-pin-a") == "release-a"

    with pytest.raises(ConflictError, match="multiple local albums"):
        await store.get_target_album_release_pin("shared-rg")
    with pytest.raises(ConflictError, match="multiple local albums"):
        await store.set_target_album_release_pin(
            "shared-rg", "wrong-release", "admin", "target-time"
        )
    with pytest.raises(ConflictError, match="multiple local albums"):
        await store.clear_target_album_release_pin("shared-rg")

    assert await store.get_target_album_release_pin("album-pin-a") == "release-a"


@pytest.mark.asyncio
async def test_target_release_pins_ignore_empty_historical_provider_match(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership("active"))
    await store.create_catalog_membership(_membership("history", with_track=False))
    with sqlite3.connect(db_path) as connection:
        connection.executemany(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, decision_source, selected_at) "
            "VALUES (?, 'musicbrainz', 'shared-rg', 'manual', 2)",
            [("album-active",), ("album-history",)],
        )

    await store.set_target_album_release_pin(
        "shared-rg", "release-active", "admin", "target-time"
    )

    assert await store.get_target_album_release_pin("shared-rg") == "release-active"
    assert await store.clear_target_album_release_pin("shared-rg") is True
    assert await store.get_target_album_release_pin("shared-rg") is None


def test_foreign_keys_checks_and_uniqueness_are_enforced(
    store: NativeLibraryStore, db_path: Path
) -> None:
    del store
    with sqlite3.connect(db_path) as connection:
        connection.execute("PRAGMA foreign_keys=ON")
        with pytest.raises(sqlite3.IntegrityError):
            connection.execute(
                "INSERT INTO local_album_aliases VALUES ('old', 'missing', 'merged_album', 1)"
            )
        with pytest.raises(sqlite3.IntegrityError):
            connection.execute(
                "INSERT INTO library_identification_reviews "
                "(id, state, reason_code, input_revision, created_at, updated_at) "
                "VALUES ('review', 'needs_review', 'reason', 'input', 1, 1)"
            )
        with pytest.raises(sqlite3.IntegrityError):
            connection.execute(
                "INSERT INTO library_identification_jobs "
                "(id, kind, state, priority, enqueue_sequence, input_revision, dedupe_key, "
                "not_before, created_at, updated_at) "
                "VALUES ('job', 'automatic', 'queued', 1, 1, 'input', 'dedupe', 0, 1, 1)"
            )


@pytest.mark.asyncio
async def test_restrict_set_null_and_cascade_delete_behaviors(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership())
    await store.create_review(
        ReviewDecision(
            id="review-1",
            local_album_id="album-1",
            input_revision="input-1",
            created_at=1,
            updated_at=1,
        )
    )
    await store.create_scan_run(
        ScanRun(id="scan-1", kind="incremental", trigger="manual", queued_at=1)
    )
    await store.add_scan_inventory_batch(
        "scan-1",
        [
            ScanInventoryItem(
                root_id="root-1",
                relative_path="1.flac",
                absolute_path="/music/1.flac",
                file_size_bytes=100,
                file_mtime_ns=200,
                stat_revision="stat-1",
                effective_policy="automatic",
                comparison_result="unchanged",
                local_track_id="track-1",
            )
        ],
        expected_run_revision=1,
        updated_at=2,
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute("PRAGMA foreign_keys=ON")
        with pytest.raises(sqlite3.IntegrityError):
            connection.execute("DELETE FROM local_albums WHERE id = 'album-1'")
        connection.execute("DELETE FROM library_scan_runs WHERE id = 'scan-1'")
        connection.execute(
            "INSERT INTO library_catalog_actions "
            "(id, actor_user_id, action_kind, local_album_id, before_json, after_json, created_at) "
            "VALUES ('action-1', 'admin', 'test', 'album-1', '{}', '{}', 1)"
        )
        connection.execute("DELETE FROM auth_users WHERE id = 'admin'")
    assert (
        _scalar(
            db_path,
            "SELECT COUNT(*) FROM library_scan_inventory WHERE run_id = 'scan-1'",
        )
        == 0
    )
    assert (
        _scalar(
            db_path,
            "SELECT actor_user_id FROM library_catalog_actions WHERE id = 'action-1'",
        )
        is None
    )


@pytest.mark.asyncio
async def test_provider_attach_detach_and_aliases_preserve_all_local_ids(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership())
    await store.attach_artist_identity_with_aliases(
        LocalArtistExternalIdentity(
            local_artist_id="artist-1",
            provider_artist_id="artist-mbid",
            selected_at=2,
        ),
        [
            LocalArtistAlias(
                alias="legacy-artist",
                local_artist_id="artist-1",
                kind="legacy_artist",
                created_at=2,
            )
        ],
        expected_artist_revision=1,
    )
    album_revision, _ = await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-1", release_group_mbid="rg-1", selected_at=2
        ),
        expected_album_revision=1,
    )
    track_revision, _ = await store.attach_track_identity(
        LocalTrackExternalIdentity(
            local_track_id="track-1", recording_mbid="recording-1", selected_at=2
        ),
        expected_track_revision=1,
    )
    album_revision, _ = await store.add_album_aliases(
        "album-1",
        [
            LocalAlbumAlias(
                alias="legacy-rg",
                local_album_id="album-1",
                kind="legacy_release_group",
                created_at=2,
            )
        ],
        expected_album_revision=album_revision,
        updated_at=3,
    )
    await store.detach_album_identity(
        "album-1",
        expected_album_revision=album_revision,
        expected_identity_revision=1,
        updated_at=4,
    )
    await store.detach_track_identity(
        "track-1",
        expected_track_revision=track_revision,
        expected_identity_revision=1,
    )

    assert (await store.get_local_album("album-1"))["id"] == "album-1"
    assert (await store.get_local_track("track-1"))["local_album_id"] == "album-1"
    assert (
        _scalar(
            db_path,
            "SELECT local_artist_id FROM local_artist_aliases WHERE alias = 'legacy-artist'",
        )
        == "artist-1"
    )
    assert (
        _scalar(
            db_path,
            "SELECT local_album_id FROM local_album_aliases WHERE alias = 'legacy-rg'",
        )
        == "album-1"
    )


@pytest.mark.asyncio
async def test_revisions_are_scoped_monotonic_and_stale_safe(
    store: NativeLibraryStore,
) -> None:
    assert await store.create_catalog_membership(_membership("1")) == 1
    assert await store.create_catalog_membership(_membership("2")) == 2
    untouched = await store.get_local_album("album-2")
    changed_revision, catalog_revision = await store.set_artwork(
        LocalArtworkAssociation(
            local_album_id="album-1", cover_url="cover", source="embedded", updated_at=2
        ),
        expected_album_revision=1,
    )

    assert changed_revision == 2
    assert catalog_revision == 3
    assert (await store.get_local_album("album-2"))["row_revision"] == untouched[
        "row_revision"
    ]
    with pytest.raises(StaleRevisionError):
        await store.set_artwork(
            LocalArtworkAssociation(
                local_album_id="album-1",
                cover_url="other",
                source="manual",
                updated_at=3,
            ),
            expected_album_revision=1,
        )


@pytest.mark.asyncio
async def test_genre_artwork_candidates_fold_membership_and_revision_changes(
    store: NativeLibraryStore, db_path: Path
) -> None:
    first = _membership("genre-1")
    second = _membership("genre-2")
    first.tracks[0].genre = "Röck"
    second.tracks[0].genre = "Rock"
    await store.create_catalog_membership(first)
    await store.create_catalog_membership(second)
    await store.set_artwork(
        LocalArtworkAssociation(
            local_album_id=first.album.id,
            cover_url="cached",
            source="manual",
            source_locator="first.bin",
            updated_at=2,
        ),
        expected_album_revision=1,
    )
    await store.set_artwork(
        LocalArtworkAssociation(
            local_album_id=second.album.id,
            cover_url="cached",
            source="manual",
            source_locator="second.bin",
            updated_at=2,
        ),
        expected_album_revision=1,
    )

    genres = await store.list_target_genres()
    before = await store.list_genre_artwork_candidates(["rÖCK"])

    assert len(genres) == 1
    assert genres[0]["song_count"] == 2
    assert {row["album_id"] for row in before["rÖCK"]["candidates"]} == {
        first.album.id,
        second.album.id,
    }
    revision = before["rÖCK"]["revision"]

    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET availability = 'missing' WHERE id = ?",
            (second.tracks[0].id,),
        )

    after = await store.list_genre_artwork_candidates(["Rock"])
    assert [row["album_id"] for row in after["Rock"]["candidates"]] == [first.album.id]
    assert after["Rock"]["revision"] > revision


@pytest.mark.asyncio
async def test_genre_artwork_revisions_target_old_new_artwork_and_retirement(
    store: NativeLibraryStore, db_path: Path
) -> None:
    membership = _membership("genre-revision")
    membership.tracks[0].genre = "Rock"
    await store.create_catalog_membership(membership)
    initial = await store.list_genre_artwork_candidates(["Rock", "Jazz"])
    rock_revision = initial["Rock"]["revision"]
    jazz_revision = initial["Jazz"]["revision"]

    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET genre = 'Jazz', genre_folded = 'jazz' WHERE id = ?",
            (membership.tracks[0].id,),
        )
    moved = await store.list_genre_artwork_candidates(["Rock", "Jazz"])
    assert moved["Rock"]["revision"] > rock_revision
    assert moved["Jazz"]["revision"] > jazz_revision

    await store.set_artwork(
        LocalArtworkAssociation(
            local_album_id=membership.album.id,
            cover_url=None,
            source="manual",
            source_locator="cover.bin",
            updated_at=2,
        ),
        expected_album_revision=1,
    )
    with_artwork = await store.list_genre_artwork_candidates(["Jazz"])
    assert with_artwork["Jazz"]["revision"] > moved["Jazz"]["revision"]

    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "DELETE FROM local_album_artwork WHERE local_album_id = ?",
            (membership.album.id,),
        )
    without_artwork = await store.list_genre_artwork_candidates(["Jazz"])
    assert without_artwork["Jazz"]["revision"] > with_artwork["Jazz"]["revision"]
    assert without_artwork["Jazz"]["candidates"] == []
    assert await store.get_cached_local_artwork_context(membership.album.id, 1) is None

    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_albums SET retired_into_album_id = id WHERE id = ?",
            (membership.album.id,),
        )
    retired = await store.list_genre_artwork_candidates(["Jazz"])
    assert retired["Jazz"]["revision"] > without_artwork["Jazz"]["revision"]
    assert retired["Jazz"]["candidates"] == []


@pytest.mark.asyncio
async def test_catalog_overflow_refuses_and_rolls_back_visible_change(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership())
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_catalog_revision SET value = ?", (MAX_REVISION,)
        )
    with pytest.raises(RevisionOverflowError):
        await store.set_artwork(
            LocalArtworkAssociation(
                local_album_id="album-1",
                cover_url="cover",
                source="embedded",
                updated_at=2,
            ),
            expected_album_revision=1,
        )

    assert (await store.get_local_album("album-1"))["row_revision"] == 1
    assert _scalar(db_path, "SELECT COUNT(*) FROM local_album_artwork") == 0


@pytest.mark.asyncio
async def test_row_and_stream_overflow_raise_without_partial_updates(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership())
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_albums SET row_revision = ? WHERE id = 'album-1'",
            (MAX_REVISION,),
        )
    with pytest.raises(RevisionOverflowError):
        await store.attach_album_identity(
            LocalAlbumExternalIdentity(
                local_album_id="album-1", release_group_mbid="rg-1", selected_at=2
            ),
            expected_album_revision=MAX_REVISION,
        )
    assert _scalar(db_path, "SELECT COUNT(*) FROM local_album_external_identities") == 0

    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_albums SET row_revision = 1 WHERE id = 'album-1'"
        )
        connection.execute(
            "UPDATE library_event_stream_revisions SET value = ? "
            "WHERE stream_kind = 'identification'",
            (MAX_REVISION,),
        )
    await store.enqueue_identification_job(
        IdentificationJob(id="job-1", dedupe_key="dedupe-1", local_album_id="album-1")
    )
    with pytest.raises(RevisionOverflowError):
        await store.claim_identification_job("worker", now=1, lease_seconds=10)
    assert (
        _scalar(
            db_path, "SELECT state FROM library_identification_jobs WHERE id = 'job-1'"
        )
        == "queued"
    )


@pytest.mark.asyncio
async def test_revision_failures_have_specific_safe_api_codes() -> None:
    request = Request(
        {"type": "http", "method": "POST", "path": "/target", "headers": []}
    )
    stale = await stale_revision_error_handler(
        request, StaleRevisionError("The library item changed.")
    )
    overflow = await revision_overflow_error_handler(
        request, RevisionOverflowError("secret path and counter")
    )

    assert stale.status_code == 409
    assert b'"code":"STALE_REVISION"' in stale.body
    assert overflow.status_code == 500
    assert b'"code":"REVISION_OVERFLOW"' in overflow.body
    assert b"secret path and counter" not in overflow.body


@pytest.mark.asyncio
async def test_identification_activity_snapshot_and_revisioned_controls(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership())
    await store.create_catalog_membership(_membership("2"))
    await store.enqueue_identification_job(
        IdentificationJob(
            id="job-waiting",
            dedupe_key="automatic:album-1:one",
            local_album_id="album-1",
            created_at=10,
        )
    )
    await store.enqueue_identification_job(
        IdentificationJob(
            id="job-failed",
            dedupe_key="automatic:album-2:two",
            local_album_id="album-2",
            created_at=8,
        )
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_identification_jobs SET state = 'failed', "
            "last_failure_code = 'PROVIDER_TEMPORARY', updated_at = 12, terminal_at = 12 "
            "WHERE id = 'job-failed'"
        )

    snapshot = await store.get_identification_activity_snapshot()
    assert snapshot["counts"] == {"failed": 1, "queued": 1}
    assert snapshot["started_at"] == 10
    assert snapshot["failure_event_id"] == "job-failed"
    assert snapshot["failure_at"] == 12
    assert snapshot["foreground_operation_count"] == 0
    assert snapshot["active_priority"] == 100
    assert snapshot["kept_local_count"] == 0

    await store.create_operation_with_work(
        OperationJob(id="foreground-operation", kind="repair", created_at=13),
        [
            OperationWorkItem(
                ordinal=0,
                local_album_id="album-1",
                expected_subject_revision=1,
                expected_input_revision="input-1",
                action="repair",
                idempotency_key="foreground-album-1",
            )
        ],
    )
    assert (await store.get_identification_activity_snapshot())[
        "foreground_operation_count"
    ] == 1

    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_operation_jobs SET state = 'ready' "
            "WHERE id = 'foreground-operation'"
        )
    assert (await store.get_identification_activity_snapshot())[
        "foreground_operation_count"
    ] == 0

    assert (
        await store.pause_identification_queue(
            requested_by_user_id="admin", requested_at=13, expected_revision=1
        )
        == 2
    )
    with pytest.raises(StaleRevisionError):
        await store.resume_identification_queue(resumed_at=14, expected_revision=1)
    assert (
        await store.resume_identification_queue(resumed_at=14, expected_revision=2) == 3
    )


@pytest.mark.asyncio
async def test_attempts_and_evidence_are_immutable_and_corrections_use_new_ids(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership())
    await store.create_review(
        ReviewDecision(
            id="review-1",
            local_album_id="album-1",
            input_revision="input-1",
            created_at=1,
            updated_at=1,
        )
    )
    first = _attempt("attempt-1")
    await store.replace_review_attempt(
        "review-1",
        expected_review_revision=1,
        attempt=first,
        evidence=[_evidence("evidence-1", first.id)],
        updated_at=2,
    )
    second = _attempt("attempt-2")
    await store.replace_review_attempt(
        "review-1",
        expected_review_revision=2,
        attempt=second,
        evidence=[_evidence("evidence-2", second.id)],
        updated_at=3,
    )

    with sqlite3.connect(db_path) as connection:
        with pytest.raises(sqlite3.IntegrityError):
            connection.execute(
                "UPDATE library_identification_attempts SET state = 'changed' "
                "WHERE id = 'attempt-1'"
            )
        with pytest.raises(sqlite3.IntegrityError):
            connection.execute(
                "UPDATE library_identification_evidence SET compacted = 1 "
                "WHERE id = 'evidence-1'"
            )
    assert _scalar(db_path, "SELECT COUNT(*) FROM library_identification_attempts") == 2
    assert (
        _scalar(
            db_path,
            "SELECT attempt_id FROM library_identification_reviews WHERE id = 'review-1'",
        )
        == "attempt-2"
    )


@pytest.mark.asyncio
async def test_catalog_membership_rolls_back_every_table_on_failure(
    store: NativeLibraryStore, db_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    def fail(_connection: sqlite3.Connection) -> int:
        raise RuntimeError("injected")

    monkeypatch.setattr(store, "_bump_catalog", fail)
    with pytest.raises(RuntimeError, match="injected"):
        await store.create_catalog_membership(_membership())

    assert (
        _scalar(db_path, "SELECT COUNT(*) FROM local_artists WHERE kind = 'person'")
        == 0
    )
    assert _scalar(db_path, "SELECT COUNT(*) FROM local_albums") == 0
    assert _scalar(db_path, "SELECT COUNT(*) FROM local_tracks") == 0
    assert _scalar(db_path, "SELECT value FROM library_catalog_revision") == 0


@pytest.mark.asyncio
async def test_identity_attach_rolls_back_subject_identity_and_revision(
    store: NativeLibraryStore, db_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    await store.create_catalog_membership(_membership())

    def fail(_connection: sqlite3.Connection) -> int:
        raise RuntimeError("injected")

    monkeypatch.setattr(store, "_bump_catalog", fail)
    with pytest.raises(RuntimeError, match="injected"):
        await store.attach_album_identity(
            LocalAlbumExternalIdentity(
                local_album_id="album-1", release_group_mbid="rg-1", selected_at=2
            ),
            expected_album_revision=1,
        )
    assert (await store.get_local_album("album-1"))["row_revision"] == 1
    assert _scalar(db_path, "SELECT COUNT(*) FROM local_album_external_identities") == 0


@pytest.mark.asyncio
async def test_alias_and_review_transactions_roll_back_all_subject_changes(
    store: NativeLibraryStore, db_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    await store.create_catalog_membership(_membership())
    duplicate_alias = LocalArtistAlias(
        alias="same",
        local_artist_id="artist-1",
        kind="legacy_artist",
        created_at=2,
    )
    with pytest.raises(sqlite3.IntegrityError):
        await store.attach_artist_identity_with_aliases(
            LocalArtistExternalIdentity(
                local_artist_id="artist-1",
                provider_artist_id="artist-mbid",
                selected_at=2,
            ),
            [duplicate_alias, duplicate_alias],
            expected_artist_revision=1,
        )
    assert (
        _scalar(db_path, "SELECT row_revision FROM local_artists WHERE id = 'artist-1'")
        == 1
    )
    assert (
        _scalar(db_path, "SELECT COUNT(*) FROM local_artist_external_identities") == 0
    )
    assert _scalar(db_path, "SELECT COUNT(*) FROM local_artist_aliases") == 0

    await store.create_review(
        ReviewDecision(
            id="review-1",
            local_album_id="album-1",
            input_revision="input-1",
            created_at=1,
            updated_at=1,
        )
    )

    def fail(_connection: sqlite3.Connection) -> int:
        raise RuntimeError("injected")

    monkeypatch.setattr(store, "_bump_catalog", fail)
    with pytest.raises(RuntimeError, match="injected"):
        await store.decide_review(
            "review-1",
            expected_review_revision=1,
            state="excluded",
            reason_code="manual_exclusion",
            decided_by_user_id="admin",
            decided_at=3,
        )
    assert (
        _scalar(
            db_path,
            "SELECT state FROM library_identification_reviews WHERE id = 'review-1'",
        )
        == "needs_review"
    )
    assert (
        _scalar(db_path, "SELECT availability FROM local_tracks WHERE id = 'track-1'")
        == "indexed"
    )
    assert _scalar(db_path, "SELECT value FROM library_catalog_revision") == 1


@pytest.mark.asyncio
async def test_scan_batch_rolls_back_inventory_counter_and_cursors(
    store: NativeLibraryStore, db_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    await store.create_scan_run(
        ScanRun(id="scan-1", kind="incremental", trigger="manual", queued_at=1)
    )

    def fail(_connection: sqlite3.Connection, _stream: str) -> int:
        raise RuntimeError("injected")

    monkeypatch.setattr(store, "_bump_stream", fail)
    with pytest.raises(RuntimeError, match="injected"):
        await store.add_scan_inventory_batch(
            "scan-1",
            [
                ScanInventoryItem(
                    root_id="root-1",
                    relative_path="1.flac",
                    absolute_path="/music/1.flac",
                    file_size_bytes=1,
                    file_mtime_ns=1,
                    stat_revision="stat-1",
                    effective_policy="automatic",
                    comparison_result="new",
                )
            ],
            expected_run_revision=1,
            updated_at=2,
        )
    assert _scalar(db_path, "SELECT COUNT(*) FROM library_scan_inventory") == 0
    assert (
        _scalar(
            db_path, "SELECT row_revision FROM library_scan_runs WHERE id = 'scan-1'"
        )
        == 1
    )
    assert (
        _scalar(
            db_path,
            "SELECT value FROM library_event_stream_revisions WHERE stream_kind = 'scan'",
        )
        == 0
    )


@pytest.mark.asyncio
async def test_discovery_generation_ignores_and_boundedly_cleans_old_rows(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_scan_run(
        ScanRun(id="scan-generation", kind="incremental", trigger="manual", queued_at=1)
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_scan_run_scopes "
            "(run_id,scope_sequence,root_id,relative_path,effective_policy,"
            "policy_revision) VALUES ('scan-generation',0,'root-1','.',"
            "'automatic','policy-1')"
        )
        connection.commit()
    await store.add_scan_inventory_batch(
        "scan-generation",
        [
            ScanInventoryItem(
                root_id="root-1",
                relative_path=f"{index}.flac",
                absolute_path=f"/music/{index}.flac",
                file_size_bytes=1,
                file_mtime_ns=1,
                stat_revision="1:1",
                effective_policy="automatic",
                comparison_result="new",
            )
            for index in range(2)
        ],
        expected_run_revision=1,
        updated_at=2,
        discovery_generation=1,
    )

    await store.prepare_scan_discovery_resume("scan-generation")

    assert (
        await store.get_scan_inventory_batch(
            "scan-generation", processing_state="pending", limit=10
        )
        == []
    )
    assert await store.cleanup_stale_scan_inventory("scan-generation", limit=1) == 1
    assert _scalar(db_path, "SELECT COUNT(*) FROM library_scan_inventory") == 1
    assert await store.cleanup_stale_scan_inventory("scan-generation", limit=1) == 1
    assert _scalar(db_path, "SELECT COUNT(*) FROM library_scan_inventory") == 0


@pytest.mark.asyncio
async def test_schema_backfills_inventory_scope_for_persisted_subdirectory_scan(
    db_path: Path,
) -> None:
    lock = threading.Lock()
    store = NativeLibraryStore(db_path, lock)
    await store.create_scan_run(
        ScanRun(
            id="scan-subdirectory", kind="incremental", trigger="manual", queued_at=1
        )
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_scan_run_scopes "
            "(run_id,scope_sequence,root_id,relative_path,effective_policy,"
            "policy_revision) VALUES ('scan-subdirectory',0,'root-1','Artist/Album',"
            "'automatic','policy-1')"
        )
        connection.execute(
            "INSERT INTO library_scan_inventory "
            "(run_id,root_id,relative_path,absolute_path,file_size_bytes,file_mtime_ns,"
            "stat_revision,policy_revision,effective_policy,comparison_result) "
            "VALUES ('scan-subdirectory','root-1','Artist/Album/01.flac',"
            "'/music/Artist/Album/01.flac',1,1,'1:1','policy-1','automatic','new')"
        )
        connection.execute(
            "ALTER TABLE library_scan_inventory DROP COLUMN scope_relative_path"
        )

    NativeLibraryStore(db_path, lock)

    with sqlite3.connect(db_path) as connection:
        stored_scope = connection.execute(
            "SELECT scope_relative_path FROM library_scan_inventory "
            "WHERE run_id = 'scan-subdirectory'"
        ).fetchone()
    assert stored_scope == ("Artist/Album",)


@pytest.mark.asyncio
async def test_identification_claim_is_atomic_and_active_dedupe_is_unique(
    store: NativeLibraryStore,
) -> None:
    await store.create_catalog_membership(_membership())
    first_id = await store.enqueue_identification_job(
        IdentificationJob(
            id="job-1", dedupe_key="album-1:input-1", local_album_id="album-1"
        )
    )
    second_id = await store.enqueue_identification_job(
        IdentificationJob(
            id="job-2", dedupe_key="album-1:input-1", local_album_id="album-1"
        )
    )
    claims = await asyncio.gather(
        store.claim_identification_job("worker-a", now=1, lease_seconds=30),
        store.claim_identification_job("worker-b", now=1, lease_seconds=30),
    )

    assert first_id == second_id == "job-1"
    assert len([claim for claim in claims if claim is not None]) == 1
    assert await store.get_stream_revision("identification") == 1


@pytest.mark.asyncio
async def test_terminal_automatic_identification_is_reused_until_input_changes(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership())
    first = IdentificationJob(
        id="job-1",
        dedupe_key="automatic:album-1:input-1",
        local_album_id="album-1",
        input_revision="input-1",
    )
    assert await store.enqueue_identification_job(first) == "job-1"
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_identification_jobs SET state = 'needs_review', terminal_at = 2 "
            "WHERE id = 'job-1'"
        )
        connection.commit()

    repeated_id, repeated_created = await store.enqueue_identification_job_result(
        msgspec.structs.replace(first, id="job-2", created_at=3)
    )
    changed_id, changed_created = await store.enqueue_identification_job_result(
        msgspec.structs.replace(
            first,
            id="job-3",
            dedupe_key="automatic:album-1:input-2",
            input_revision="input-2",
            created_at=4,
        )
    )

    assert (repeated_id, repeated_created) == ("job-1", False)
    assert (changed_id, changed_created) == ("job-3", True)


@pytest.mark.asyncio
async def test_identification_heartbeat_recovery_and_completion_are_atomic(
    store: NativeLibraryStore, db_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    await store.create_catalog_membership(_membership())
    await store.enqueue_identification_job(
        IdentificationJob(id="job-1", dedupe_key="dedupe-1", local_album_id="album-1")
    )
    claim = await store.claim_identification_job("worker", now=1, lease_seconds=10)
    assert claim is not None
    assert await store.heartbeat_identification_job(
        "job-1", "worker", now=2, lease_seconds=10
    )
    assert await store.recover_expired_identification_leases(now=5) == 0
    assert await store.recover_expired_identification_leases(now=20) == 1
    claim = await store.claim_identification_job("worker", now=21, lease_seconds=10)

    def fail(_connection: sqlite3.Connection, _stream: str) -> int:
        raise RuntimeError("injected")

    monkeypatch.setattr(store, "_bump_stream", fail)
    attempt = _attempt("attempt-1")
    with pytest.raises(RuntimeError, match="injected"):
        await store.complete_identification_job(
            "job-1",
            worker_id="worker",
            expected_job_revision=claim["row_revision"],
            attempt=attempt,
            evidence=[_evidence("evidence-1", attempt.id)],
            terminal_state="needs_review",
            completed_at=22,
        )
    assert _scalar(db_path, "SELECT COUNT(*) FROM library_identification_attempts") == 0
    assert _scalar(db_path, "SELECT COUNT(*) FROM library_identification_evidence") == 0
    assert (
        _scalar(
            db_path, "SELECT state FROM library_identification_jobs WHERE id = 'job-1'"
        )
        == "running"
    )


@pytest.mark.asyncio
async def test_operation_materialization_claim_heartbeat_recovery_and_work_completion(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership())
    await store.create_operation_with_work(
        OperationJob(id="operation-1", kind="repair", created_at=1),
        [
            OperationWorkItem(
                ordinal=0,
                local_album_id="album-1",
                expected_subject_revision=1,
                expected_input_revision="input-1",
                action="repair",
                idempotency_key="album-1",
            )
        ],
    )
    claim = await store.claim_operation_job("worker", now=1, lease_seconds=10)
    assert claim is not None
    assert await store.heartbeat_operation_job(
        "operation-1", "worker", now=2, lease_seconds=10
    )
    assert await store.recover_expired_operation_leases(now=5) == 0
    work = await store.claim_operation_work("operation-1", "worker", now=3)
    assert work is not None
    work_revision, job_revision, stream_revision = await store.complete_operation_work(
        "operation-1",
        0,
        worker_id="worker",
        expected_work_revision=work["row_revision"],
        state="succeeded",
        result_json="{}",
        failure_code=None,
        completed_at=4,
    )
    assert (work_revision, job_revision, stream_revision) == (3, 4, 2)
    assert (
        _scalar(
            db_path,
            "SELECT completed_count FROM library_operation_jobs WHERE id = 'operation-1'",
        )
        == 1
    )


@pytest.mark.asyncio
async def test_operation_completion_rolls_back_work_job_and_stream_revisions(
    store: NativeLibraryStore, db_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    await store.create_catalog_membership(_membership())
    await store.create_operation_with_work(
        OperationJob(id="operation-1", kind="repair", created_at=1),
        [
            OperationWorkItem(
                ordinal=0,
                local_album_id="album-1",
                expected_subject_revision=1,
                expected_input_revision="input-1",
                action="repair",
                idempotency_key="album-1",
            )
        ],
    )
    await store.claim_operation_job("worker", now=1, lease_seconds=10)
    work = await store.claim_operation_work("operation-1", "worker", now=2)

    def fail(_connection: sqlite3.Connection, _stream: str) -> int:
        raise RuntimeError("injected")

    monkeypatch.setattr(store, "_bump_stream", fail)
    with pytest.raises(RuntimeError, match="injected"):
        await store.complete_operation_work(
            "operation-1",
            0,
            worker_id="worker",
            expected_work_revision=work["row_revision"],
            state="succeeded",
            result_json="{}",
            failure_code=None,
            completed_at=3,
        )
    assert (
        _scalar(
            db_path,
            "SELECT state FROM library_operation_work WHERE job_id = 'operation-1' AND ordinal = 0",
        )
        == "running"
    )
    assert (
        _scalar(
            db_path,
            "SELECT completed_count FROM library_operation_jobs WHERE id = 'operation-1'",
        )
        == 0
    )
    assert (
        _scalar(
            db_path,
            "SELECT value FROM library_event_stream_revisions WHERE stream_kind = 'operation'",
        )
        == 1
    )


@pytest.mark.asyncio
async def test_operation_materialization_and_repair_findings_roll_back_as_units(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership())
    duplicate = OperationWorkItem(
        ordinal=0,
        local_album_id="album-1",
        expected_subject_revision=1,
        expected_input_revision="input-1",
        action="repair",
        idempotency_key="album-1",
    )
    with pytest.raises(sqlite3.IntegrityError):
        await store.create_operation_with_work(
            OperationJob(id="operation-bad", kind="repair", created_at=1),
            [duplicate, duplicate],
        )
    assert (
        _scalar(
            db_path,
            "SELECT COUNT(*) FROM library_operation_jobs WHERE id = 'operation-bad'",
        )
        == 0
    )

    await store.create_operation_with_work(
        OperationJob(id="operation-good", kind="repair", created_at=1), []
    )
    finding = RepairFinding(
        id="finding-1",
        local_album_id="album-1",
        expected_album_revision=1,
        finding_code="unsafe_identity",
        confidence="high",
    )
    with pytest.raises(sqlite3.IntegrityError):
        await store.add_repair_findings(
            "operation-good", [finding, finding], updated_at=2
        )
    assert (
        _scalar(db_path, "SELECT COUNT(*) FROM library_identity_repair_findings") == 0
    )


@pytest.mark.asyncio
async def test_migration_provenance_repeats_and_refuses_changed_sources(
    store: NativeLibraryStore,
) -> None:
    provenance = MigrationProvenance(
        source_kind="favorite",
        source_key="alice:album:legacy",
        target_kind="local_album",
        target_id="album-1",
        source_revision="source-1",
        imported_at=1,
    )

    assert await store.record_migration_provenance(provenance) is True
    assert await store.record_migration_provenance(provenance) is False
    with pytest.raises(StaleRevisionError):
        await store.record_migration_provenance(
            MigrationProvenance(
                source_kind=provenance.source_kind,
                source_key=provenance.source_key,
                target_kind=provenance.target_kind,
                target_id=provenance.target_id,
                source_revision="source-2",
                imported_at=2,
            )
        )


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("sql", "expected_index"),
    [
        (
            "SELECT * FROM local_tracks WHERE root_id = ? AND relative_path = ?",
            "sqlite_autoindex_local_tracks_2",
        ),
        (
            "SELECT * FROM local_tracks WHERE root_id = ? AND relative_path >= ? AND relative_path < ? ORDER BY relative_path, id",
            "sqlite_autoindex_local_tracks_2",
        ),
        (
            "SELECT * FROM local_tracks WHERE local_album_id = ? ORDER BY disc_number, track_number, id",
            "idx_local_tracks_album_order",
        ),
        (
            "SELECT * FROM library_identification_jobs WHERE state = ? AND not_before <= ? ORDER BY priority, enqueue_sequence",
            "idx_identification_jobs_claim",
        ),
        (
            "SELECT id FROM library_identification_jobs WHERE local_album_id = ? AND kind = ? AND state = 'queued' ORDER BY enqueue_sequence LIMIT 1",
            "idx_identification_jobs_album_active",
        ),
        (
            "SELECT id FROM library_identification_jobs WHERE local_track_id = ? AND kind = ? AND state = 'queued' ORDER BY enqueue_sequence LIMIT 1",
            "idx_identification_jobs_track_active",
        ),
        (
            "SELECT * FROM library_scan_inventory WHERE run_id = ? AND processing_state = ? ORDER BY root_id, relative_path",
            "idx_scan_inventory_processing",
        ),
        (
            "SELECT * FROM audio_fingerprint_outcomes WHERE local_track_id = ? AND stat_revision = ? AND fingerprinter_version = ?",
            "sqlite_autoindex_audio_fingerprint_outcomes_2",
        ),
        (
            "SELECT * FROM local_album_aliases WHERE local_album_id = ?",
            "idx_album_alias_target",
        ),
        (
            "SELECT * FROM local_album_external_identities WHERE release_group_mbid = ?",
            "idx_local_album_identity_rg",
        ),
        (
            "SELECT * FROM local_tracks WHERE genre_folded = ? AND availability = ?",
            "idx_local_tracks_genre_artwork",
        ),
        (
            "SELECT 1 FROM library_identification_reviews "
            "WHERE local_track_id = ? AND reason_code LIKE 'legacy_%'",
            "idx_library_reviews_track_reason",
        ),
        (
            "SELECT 1 FROM library_compat_play_queue_items "
            "WHERE user_id = SUBSTR(?, 1, INSTR(?, ':') - 1) "
            "AND item_index = CAST(SUBSTR(?, INSTR(?, ':') + 1) AS INTEGER) "
            "AND ? = user_id || ':' || item_index AND local_track_id = ?",
            "sqlite_autoindex_library_compat_play_queue_items_1",
        ),
    ],
)
async def test_named_query_shapes_use_expected_indexes(
    store: NativeLibraryStore, sql: str, expected_index: str
) -> None:
    parameters = tuple("x" for _ in range(sql.count("?")))
    plan = await store.explain_query_plan(sql, parameters)
    assert any(expected_index in detail for detail in plan), plan


@pytest.mark.asyncio
async def test_policy_scope_wildcards_are_matched_literally(
    store: NativeLibraryStore, db_path: Path
) -> None:
    literal = _membership("literal")
    literal.tracks[0].relative_path = "scope%_literal/track.flac"
    literal.tracks[0].file_path = "/music/scope%_literal/track.flac"
    sibling = _membership("sibling")
    sibling.tracks[0].relative_path = "scopeXXliteral/track.flac"
    sibling.tracks[0].file_path = "/music/scopeXXliteral/track.flac"
    await store.create_catalog_membership(literal)
    await store.create_catalog_membership(sibling)
    scope = ScanScope(
        root_id="root-1",
        relative_path="scope%_literal",
        policy_revision="policy-2",
    )

    assert await store.estimate_scan_scope([scope]) == 1
    assert await store.get_policy_scope_counts([("root-1", "scope%_literal")]) == {
        ("root-1", "scope%_literal"): (1, 1)
    }
    assert await store.get_policy_scope_total_counts([scope, scope]) == (1, 1)
    result = await store.apply_desired_policy(
        root_id="root-1",
        relative_prefix="scope%_literal",
        policy_revision="policy-2",
        policy="excluded",
        updated_at=2,
    )
    assert result["changed"] == 1
    with sqlite3.connect(db_path) as connection:
        rows = connection.execute(
            "SELECT relative_path, desired_policy_revision FROM local_tracks "
            "ORDER BY relative_path"
        ).fetchall()
    assert rows == [
        ("scope%_literal/track.flac", "policy-2"),
        ("scopeXXliteral/track.flac", ""),
    ]


@pytest.mark.asyncio
async def test_policy_scope_aggregates_collapse_overlaps_and_count_availability_exactly(
    store: NativeLibraryStore, db_path: Path
) -> None:
    root_track = _membership("root")
    root_track.tracks[0].relative_path = "Artist/root.flac"
    root_track.tracks[0].file_path = "/music/Artist/root.flac"
    excluded_track = _membership("excluded")
    excluded_track.tracks[0].relative_path = "Artist/Live/excluded.flac"
    excluded_track.tracks[0].file_path = "/music/Artist/Live/excluded.flac"
    missing_track = _membership("missing")
    missing_track.tracks[0].relative_path = "Artist/Live/missing.flac"
    missing_track.tracks[0].file_path = "/music/Artist/Live/missing.flac"
    removed_root_track = _membership("removed")
    removed_root_track.album.root_id = "removed-root"
    removed_root_track.tracks[0].root_id = "removed-root"
    removed_root_track.tracks[0].relative_path = "gone.flac"
    removed_root_track.tracks[0].file_path = "/old/gone.flac"
    for membership in (
        root_track,
        excluded_track,
        missing_track,
        removed_root_track,
    ):
        await store.create_catalog_membership(membership)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET availability = 'excluded' WHERE id = 'track-excluded'"
        )
        connection.execute(
            "UPDATE local_tracks SET availability = 'missing' WHERE id = 'track-missing'"
        )

    counts = await store.get_policy_scope_counts(
        [
            ("root-1", "."),
            ("root-1", "Artist"),
            ("root-1", "Artist/Live"),
            ("removed-root", "."),
        ]
    )
    assert counts == {
        ("root-1", "."): (1, 2),
        ("root-1", "Artist"): (1, 2),
        ("root-1", "Artist/Live"): (0, 1),
        ("removed-root", "."): (1, 1),
    }
    scopes = [
        ScanScope(root_id="root-1", relative_path=".", policy_revision="policy-2"),
        ScanScope(
            root_id="root-1",
            relative_path="Artist/Live",
            policy_revision="policy-2",
        ),
        ScanScope(
            root_id="removed-root", relative_path=".", policy_revision="policy-2"
        ),
    ]
    assert await store.get_policy_scope_total_counts(scopes) == (2, 3)
    nested_scopes = [
        ScanScope(root_id="root-1", relative_path="Artist", policy_revision="policy-2"),
        ScanScope(
            root_id="root-1",
            relative_path="Artist/Live",
            policy_revision="policy-2",
        ),
        ScanScope(
            root_id="removed-root", relative_path=".", policy_revision="policy-2"
        ),
    ]
    assert await store.get_policy_scope_total_counts(nested_scopes) == (2, 3)


@pytest.mark.asyncio
async def test_pending_policy_preserves_frozen_scope_paths(
    store: NativeLibraryStore,
) -> None:
    scope = ScanScope(
        root_id="removed-root",
        scope_id="removed-root",
        relative_path=".",
        root_path="/old/music",
        effective_policy="excluded",
        policy_revision="policy-2",
    )

    await store.record_pending_policy(
        policy_revision="policy-2",
        scopes=[scope],
        changed_track_count=3,
        cancelled_work_count=1,
        updated_at=2,
    )
    pending = await store.get_pending_policy()

    assert pending is not None
    assert pending["pending_scope_ids"] == ["removed-root"]
    assert pending["pending_scopes"] == [scope]


@pytest.mark.asyncio
async def test_policy_transition_journal_is_durable_and_idempotent(
    store: NativeLibraryStore,
    db_path: Path,
) -> None:
    scope = ScanScope(
        root_id="root-1",
        scope_id="root-1",
        relative_path=".",
        root_path="/music",
        effective_policy="excluded",
        policy_revision="policy-2",
    )
    secret = "do-not-store-this-acoustid-secret"
    safe_settings = TargetLibraryPolicyService._settings_json(
        TypedLibrarySettings(
            library_roots=[
                LibraryRootSettings(
                    id="root-1", path="/music", label="Music", policy="excluded"
                )
            ],
            acoustid_api_key=secret,
        )
    )
    prepare = {
        "previous_policy_revision": "policy-1",
        "proposed_policy_revision": "policy-2",
        "previous_settings_json": '{"library_roots":[]}',
        "proposed_settings_json": safe_settings,
        "scopes": [scope],
        "prepared_at": 1,
    }

    await store.prepare_policy_transition(**prepare)
    await store.prepare_policy_transition(**prepare)
    prepared = await store.get_policy_transition()
    assert prepared is not None
    assert prepared["state"] == "prepared"
    assert prepared["scopes"] == [scope]
    with sqlite3.connect(db_path) as connection:
        stored = connection.execute(
            "SELECT previous_settings_json, proposed_settings_json "
            "FROM library_policy_transitions WHERE singleton = 1"
        ).fetchone()
    assert secret not in "".join(stored)

    result = await store.commit_policy_transition(
        proposed_policy_revision="policy-2", updated_at=2
    )
    transition = await store.get_policy_transition()
    pending = await store.get_pending_policy()
    assert result == {"changed": 0, "cancelled": 0}
    assert transition is not None and transition["state"] == "completed"
    assert pending is not None
    assert pending["desired_policy_revision"] == "policy-2"
