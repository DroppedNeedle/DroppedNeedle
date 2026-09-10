import asyncio
import hashlib
import json
import sqlite3
import threading
import uuid
from pathlib import Path
from unittest.mock import AsyncMock

import msgspec
import pytest
from starlette.requests import Request

from core.exception_handlers import (
    revision_overflow_error_handler,
    stale_revision_error_handler,
)
from core.exceptions import ConflictError, ResourceNotFoundError, RevisionOverflowError, StaleRevisionError
from api.v1.schemas.library_management import LibraryManagementProfile, NamingScriptSettings
from api.v1.schemas.library_policies import LibraryRootSettings, TypedLibrarySettings
from infrastructure.persistence.native_library_store import (
    MAX_REVISION,
    UNKNOWN_ARTIST_ID,
    VARIOUS_ARTISTS_ID,
    NativeLibraryStore,
)
from services.native.identification_revisions import (
    album_identity_revision,
    album_input_revisions,
)
from services.native.target_library_policy_service import TargetLibraryPolicyService
from models.audio import AudioInfo, AudioTag
from models.audio_metadata import DesiredAudioDocument
from models.identification import (
    CandidateEvidence,
    IdentificationAttempt,
    IdentificationEvidenceRecord,
    TrackEvidence,
)
from models.library_management import LibraryManagementImportBundle, LibraryManagementImportFile
from models.library_management_planning import PinnedLibraryManagementProfile
from models.library_work import (
    IdentificationJob,
    MigrationProvenance,
    OperationJob,
    OperationWorkItem,
    RepairFinding,
    ReviewDecision,
    ScanFailureRecord,
    ScanInventoryItem,
    ScanRun,
    ScanScope,
    ScannedTrackWrite,
)
from repositories.musicbrainz_base import MbSourceContext
FINGERPRINTER_VERSION = "fpcalc-acoustid-v1"
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
async def test_schema_repairs_relationship_anchored_synthetic_artist_duplicates(
    store: NativeLibraryStore,
    db_path: Path,
) -> None:
    canonical_id = "artist-canonical"
    local_id = "artist-local"
    synthetic_id = "artist-synthetic"
    unanchored_id = "artist-unanchored"
    guest_id = "artist-guest"
    unrelated_id = "artist-unrelated"
    canonical_mbid = "88d17133-abbc-42db-9526-4e2c1db60336"
    synthetic_mbid = "d4ee74d98c7a6f053a0ebffd0ed5fccb"
    unanchored_mbid = "b" * 32

    def membership(
        artist_id: str,
        suffix: str,
        *,
        embedded_album_artist_mbid: str | None = None,
    ) -> CatalogMembership:
        artist = _artist(artist_id, "Shared Artist")
        album = LocalAlbum(
            id=f"album-{suffix}",
            root_id="root-1",
            grouping_key=f"group-{suffix}",
            title=f"Album {suffix}",
            album_artist_id=artist_id,
            album_artist_name=artist.display_name,
            created_at=1,
            updated_at=1,
        )
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
            album_artist_name=artist.display_name,
            embedded_album_artist_mbid=embedded_album_artist_mbid,
            file_format="flac",
            imported_at=1,
        )
        return CatalogMembership(
            album=album,
            artists=[artist],
            tracks=[track],
            track_credits={
                track.id: [LocalArtistCredit(local_artist_id=artist_id, position=0)]
            },
        )

    await store.create_catalog_membership(membership(canonical_id, "canonical"))
    await store.create_catalog_membership(
        membership(
            local_id,
            "local",
            embedded_album_artist_mbid=canonical_mbid,
        )
    )
    await store.create_catalog_membership(membership(unrelated_id, "unrelated"))
    await store.create_catalog_membership(membership(unanchored_id, "unanchored"))
    with sqlite3.connect(db_path) as connection:
        connection.executemany(
            "INSERT INTO local_artists "
            "(id, display_name, folded_name, normalized_name, kind, created_at, updated_at) "
            "VALUES (?, ?, ?, ?, 'person', 1, 1)",
            [
                (synthetic_id, "Shared Artist", "shared artist", "shared artist"),
                (guest_id, "Guest Credit", "guest credit", "guest credit"),
            ],
        )
        connection.executemany(
            "INSERT INTO local_artist_external_identities "
            "(local_artist_id, provider, provider_artist_id, decision_source, selected_at) "
            "VALUES (?, 'musicbrainz', ?, 'legacy_import', 1)",
            [
                (canonical_id, canonical_mbid),
                (synthetic_id, synthetic_mbid),
                (unanchored_id, unanchored_mbid),
                (guest_id, "a" * 32),
            ],
        )
        connection.execute(
            "UPDATE local_track_artists SET local_artist_id = ? "
            "WHERE local_track_id = 'track-canonical'",
            (synthetic_id,),
        )
        connection.execute(
            "INSERT INTO local_track_artists "
            "(local_track_id, position, local_artist_id, role) "
            "VALUES ('track-canonical', 1, ?, 'guest')",
            (guest_id,),
        )
        connection.execute(
            "INSERT INTO local_artist_aliases VALUES (?, ?, 'legacy_artist', 1)",
            (synthetic_mbid, synthetic_id),
        )
        left, right = sorted((canonical_id, local_id))
        connection.execute(
            "INSERT INTO local_artist_merge_candidates "
            "(id, left_artist_id, right_artist_id, reason_code, created_at, updated_at) "
            "VALUES ('candidate-1', ?, ?, 'SHARED_PROVIDER_IDENTITY', 1, 1)",
            (left, right),
        )
        # N-03a: every-credit provider proof for both repair retirees, so the
        # gated repair still merges this group exactly as before the gate.
        connection.executemany(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, release_mbid, "
            "decision_source, selected_at) VALUES (?, 'musicbrainz', ?, ?, 'automatic', 1)",
            [
                (
                    "album-canonical",
                    "11111111-1111-4111-8111-111111111111",
                    "11111111-1111-4111-8111-111111111111",
                ),
                (
                    "album-local",
                    "22222222-2222-4222-8222-222222222222",
                    "22222222-2222-4222-8222-222222222222",
                ),
            ],
        )
        connection.executemany(
            "INSERT INTO local_track_external_identities "
            "(local_track_id, provider, recording_mbid, release_mbid, "
            "release_track_mbid, decision_source, selected_at) "
            "VALUES (?, 'musicbrainz', ?, ?, ?, 'automatic', 1)",
            [
                (
                    "track-canonical",
                    "33333333-3333-4333-8333-333333333333",
                    "11111111-1111-4111-8111-111111111111",
                    "44444444-4444-4444-8444-444444444444",
                ),
                (
                    "track-local",
                    "55555555-5555-4555-8555-555555555555",
                    "22222222-2222-4222-8222-222222222222",
                    "66666666-6666-4666-8666-666666666666",
                ),
            ],
        )
        connection.executemany(
            "INSERT INTO library_artist_credit_proofs "
            "(subject_kind, subject_id, local_album_id, local_track_id, credit_position, "
            "source_local_artist_id, local_artist_id, artist_mbid, canonical_name, "
            "credited_name, sort_name, join_phrase, release_mbid, release_track_mbid, "
            "album_identity_revision, track_identity_revision, evidence_hash, "
            "created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, "
            "'Shared Artist', 'Shared Artist', '', '', ?, ?, 1, ?, ?, 1, 1)",
            [
                (
                    "album",
                    "album-local",
                    "album-local",
                    None,
                    0,
                    local_id,
                    canonical_id,
                    canonical_mbid,
                    "22222222-2222-4222-8222-222222222222",
                    None,
                    None,
                    "0" * 64,
                ),
                (
                    "track",
                    "track-local",
                    "album-local",
                    "track-local",
                    0,
                    local_id,
                    canonical_id,
                    canonical_mbid,
                    "22222222-2222-4222-8222-222222222222",
                    "66666666-6666-4666-8666-666666666666",
                    1,
                    "0" * 64,
                ),
                (
                    "track",
                    "track-canonical",
                    "album-canonical",
                    "track-canonical",
                    0,
                    synthetic_id,
                    canonical_id,
                    canonical_mbid,
                    "11111111-1111-4111-8111-111111111111",
                    "44444444-4444-4444-8444-444444444444",
                    1,
                    "0" * 64,
                ),
            ],
        )
        connection.execute(
            "INSERT INTO library_user_favorites VALUES " "('admin', 'artist', ?, 1)",
            (local_id,),
        )
        connection.execute(
            "INSERT INTO library_play_history "
            "(id, user_id, local_track_id, local_album_id, local_artist_id, "
            "track_name, artist_name, played_at) VALUES "
            "('history-1', 'admin', 'track-local', 'album-local', ?, "
            "'Track local', 'Shared Artist', '2026-07-23T00:00:00Z')",
            (local_id,),
        )
        connection.execute(
            "INSERT INTO library_playlists "
            "(id, name, created_at, updated_at, user_id) "
            "VALUES ('playlist-1', 'Test', '1', '1', 'admin')"
        )
        connection.execute(
            "INSERT INTO library_playlist_tracks "
            "(id, playlist_id, position, track_name, artist_name, album_name, "
            "source_type, created_at, local_artist_id) "
            "VALUES ('playlist-track-1', 'playlist-1', 0, 'Track local', "
            "'Shared Artist', 'Album local', 'local', '1', ?)",
            (local_id,),
        )
        connection.execute(
            "INSERT INTO library_compat_id_map VALUES ('compat-artist', 'artist', ?)",
            (local_id,),
        )
        connection.execute(
            "INSERT INTO library_migration_provenance "
            "(source_kind, source_key, target_kind, target_id, source_revision, imported_at) "
            "VALUES ('native_artist_alias', ?, 'local_artist', ?, 'revision-1', 1)",
            (synthetic_mbid, synthetic_id),
        )
        connection.execute(
            "INSERT INTO library_scan_runs "
            "(id, kind, trigger, state, phase, aggregate_scope, queued_at, updated_at) "
            "VALUES ('scan-1', 'incremental', 'manual', 'reconciling', 'reconciling', "
            "'root-1', 1, 1)"
        )
        connection.execute(
            "INSERT INTO library_scan_grouping_contexts "
            "(run_id, root_id, relative_directory) VALUES ('scan-1', 'root-1', '.')"
        )
        connection.execute(
            "INSERT INTO library_scan_grouping_groups "
            "(run_id, root_id, relative_directory, grouping_token, grouping_key, title, "
            "album_artist_name, reason_code, local_artist_id) "
            "VALUES ('scan-1', 'root-1', '.', 'token-1', 'group-1', 'Album local', "
            "'Shared Artist', 'AUTOMATIC_GROUPING', ?)",
            (local_id,),
        )
        connection.executemany(
            "INSERT INTO local_entity_source_links "
            "(id, local_artist_id, provider, external_entity_type, external_id, "
            "canonical_url, decision_source, verified_at, created_at, updated_at) "
            "VALUES (?, ?, 'discogs', 'artist', ?, ?, 'manual', 1, 1, 1)",
            [
                (
                    "source-canonical",
                    canonical_id,
                    "shared-source",
                    "https://www.discogs.com/artist/shared-source",
                ),
                (
                    "source-local-duplicate",
                    local_id,
                    "shared-source",
                    "https://www.discogs.com/artist/shared-source",
                ),
                (
                    "source-local-unique",
                    local_id,
                    "unique-source",
                    "https://www.discogs.com/artist/unique-source",
                ),
            ],
        )

    repaired = NativeLibraryStore(db_path, threading.Lock())
    revision_after_repair = await repaired.get_catalog_revision()
    NativeLibraryStore(db_path, threading.Lock())
    listed_artists, listed_total = await repaired.list_target_artists(
        search="Shared Artist"
    )

    with sqlite3.connect(db_path) as connection:
        active_shared = connection.execute(
            "SELECT id FROM local_artists WHERE normalized_name = 'shared artist' "
            "AND retired_into_artist_id IS NULL"
        ).fetchall()
        retired = dict(
            connection.execute(
                "SELECT id, retired_into_artist_id FROM local_artists "
                "WHERE id IN (?, ?)",
                (local_id, synthetic_id),
            ).fetchall()
        )
        album_artists = {
            row[0]
            for row in connection.execute(
                "SELECT album_artist_id FROM local_albums "
                "WHERE id IN ('album-canonical', 'album-local')"
            )
        }
        track_artists = {
            row[0]
            for row in connection.execute(
                "SELECT local_artist_id FROM local_track_artists "
                "WHERE local_track_id IN ('track-canonical', 'track-local') "
                "AND role = 'primary'"
            )
        }
        aliases = dict(
            connection.execute(
                "SELECT alias, local_artist_id FROM local_artist_aliases "
                "WHERE alias IN (?, ?, ?)",
                (synthetic_mbid, synthetic_id, local_id),
            ).fetchall()
        )
        references = (
            connection.execute(
                "SELECT item_id FROM library_user_favorites WHERE user_id = 'admin'"
            ).fetchone()[0],
            connection.execute(
                "SELECT local_artist_id FROM library_play_history "
                "WHERE id = 'history-1'"
            ).fetchone()[0],
            connection.execute(
                "SELECT local_artist_id FROM library_playlist_tracks "
                "WHERE id = 'playlist-track-1'"
            ).fetchone()[0],
            connection.execute(
                "SELECT internal_id FROM library_compat_id_map "
                "WHERE jf_id = 'compat-artist'"
            ).fetchone()[0],
            connection.execute(
                "SELECT target_id FROM library_migration_provenance "
                "WHERE source_key = ?",
                (synthetic_mbid,),
            ).fetchone()[0],
        )
        guest_identity = connection.execute(
            "SELECT provider_artist_id FROM local_artist_external_identities "
            "WHERE local_artist_id = ?",
            (guest_id,),
        ).fetchone()
        unanchored = connection.execute(
            "SELECT artist.retired_into_artist_id, identity.provider_artist_id "
            "FROM local_artists artist "
            "LEFT JOIN local_artist_external_identities identity "
            "ON identity.local_artist_id = artist.id WHERE artist.id = ?",
            (unanchored_id,),
        ).fetchone()
        candidate_state = connection.execute(
            "SELECT state FROM local_artist_merge_candidates WHERE id = 'candidate-1'"
        ).fetchone()[0]
        actions = connection.execute(
            "SELECT action_kind FROM library_catalog_actions "
            "WHERE reason_code = 'LEGACY_SYNTHETIC_ARTIST_IDENTITY' "
            "ORDER BY action_kind"
        ).fetchall()
        grouping_artist = connection.execute(
            "SELECT local_artist_id FROM library_scan_grouping_groups "
            "WHERE run_id = 'scan-1' AND grouping_token = 'token-1'"
        ).fetchone()[0]
        source_links = connection.execute(
            "SELECT id, local_artist_id FROM local_entity_source_links ORDER BY id"
        ).fetchall()
    assert {row[0] for row in active_shared} == {
        canonical_id,
        unrelated_id,
        unanchored_id,
    }
    assert listed_total == 3
    assert {artist["artist_mbid"] for artist in listed_artists} == {
        canonical_id,
        unrelated_id,
        unanchored_id,
    }
    assert retired == {local_id: canonical_id, synthetic_id: canonical_id}
    assert album_artists == {canonical_id}
    assert track_artists == {canonical_id}
    assert aliases == {
        synthetic_mbid: canonical_id,
        synthetic_id: canonical_id,
        local_id: canonical_id,
    }
    assert references == (canonical_id,) * 5
    assert guest_identity is None
    assert unanchored == (None, None)
    assert candidate_state == "resolved"
    assert actions == [
        ("detach_artist_identity",),
        ("detach_artist_identity",),
        ("merge_artist",),
    ]
    assert grouping_artist == canonical_id
    assert source_links == [
        ("source-canonical", canonical_id),
        ("source-local-unique", canonical_id),
    ]
    assert await repaired.get_catalog_revision() == revision_after_repair


@pytest.mark.asyncio
async def test_legacy_repair_without_provider_proof_opens_candidate(
    store: NativeLibraryStore,
    db_path: Path,
) -> None:
    """N-03a: a legacy repair group without every-credit provider proof opens
    merge candidates instead of retiring (the repair path is outside the
    F-IDENT-01 option-B exception, so the every-credit rule governs)."""
    survivor_mbid = "9b0a4f2e-1c3d-4a5b-8c6d-7e8f9a0b1c2d"
    survivor = _artist("artist-gated-survivor", "Gated Artist")
    retiree = _artist("artist-gated-retiree", "Gated Artist")
    zero_credit = _artist("artist-gated-zero-credit", "Gated Artist")
    await store.create_catalog_membership(
        CatalogMembership(
            album=LocalAlbum(
                id="album-gated",
                root_id="root-1",
                grouping_key="group-gated",
                title="Gated Album",
                album_artist_id=survivor.id,
                album_artist_name=survivor.display_name,
                created_at=1,
                updated_at=1,
            ),
            artists=[survivor, retiree, zero_credit],
            tracks=[
                LocalTrack(
                    id="track-gated-anchored",
                    local_album_id="album-gated",
                    root_id="root-1",
                    file_path="/music/gated-1.flac",
                    relative_path="gated-1.flac",
                    path_hash="hash-gated-1",
                    file_size_bytes=100,
                    file_mtime_ns=200,
                    stat_revision="stat-gated-1",
                    title="Gated Track",
                    artist_name=survivor.display_name,
                    album_title="Gated Album",
                    album_artist_name=survivor.display_name,
                    file_format="flac",
                    imported_at=1,
                ),
                LocalTrack(
                    id="track-gated-excluded",
                    local_album_id="album-gated",
                    root_id="root-1",
                    file_path="/music/gated-2.flac",
                    relative_path="gated-2.flac",
                    path_hash="hash-gated-2",
                    file_size_bytes=100,
                    file_mtime_ns=200,
                    stat_revision="stat-gated-2",
                    title="Gated B-side",
                    artist_name=survivor.display_name,
                    album_title="Gated Album",
                    album_artist_name=survivor.display_name,
                    file_format="flac",
                    imported_at=1,
                ),
            ],
            track_credits={
                "track-gated-anchored": [
                    LocalArtistCredit(
                        local_artist_id="artist-gated-retiree", position=0
                    )
                ],
                "track-gated-excluded": [
                    LocalArtistCredit(
                        local_artist_id="artist-gated-zero-credit", position=0
                    )
                ],
            },
        )
    )
    await store.create_catalog_membership(
        CatalogMembership(
            album=LocalAlbum(
                id="album-gated-embedded",
                root_id="root-1",
                grouping_key="group-gated-embedded",
                title="Gated Embedded Album",
                album_artist_id="artist-gated-embedded",
                album_artist_name="Gated Artist",
                created_at=1,
                updated_at=1,
            ),
            artists=[_artist("artist-gated-embedded", "Gated Artist")],
            tracks=[
                LocalTrack(
                    id="track-gated-embedded",
                    local_album_id="album-gated-embedded",
                    root_id="root-1",
                    file_path="/music/gated-3.flac",
                    relative_path="gated-3.flac",
                    path_hash="hash-gated-3",
                    file_size_bytes=100,
                    file_mtime_ns=200,
                    stat_revision="stat-gated-3",
                    title="Gated Embedded Track",
                    artist_name="Gated Artist",
                    album_title="Gated Embedded Album",
                    album_artist_name="Gated Artist",
                    embedded_album_artist_mbid=survivor_mbid,
                    file_format="flac",
                    imported_at=1,
                ),
            ],
            track_credits={
                "track-gated-embedded": [
                    LocalArtistCredit(
                        local_artist_id="artist-gated-embedded", position=0
                    )
                ]
            },
        )
    )
    with sqlite3.connect(db_path) as connection:
        connection.executemany(
            "INSERT INTO local_artist_external_identities "
            "(local_artist_id, provider, provider_artist_id, decision_source, selected_at) "
            "VALUES (?, 'musicbrainz', ?, 'legacy_import', 1)",
            [
                (survivor.id, survivor_mbid),
                (retiree.id, "c" * 32),
                (zero_credit.id, "d" * 32),
            ],
        )
        connection.execute(
            "UPDATE local_tracks SET availability = 'excluded' "
            "WHERE id = 'track-gated-excluded'"
        )

    repaired = NativeLibraryStore(db_path, threading.Lock())
    revision_after_repair = await repaired.get_catalog_revision()
    NativeLibraryStore(db_path, threading.Lock())

    with sqlite3.connect(db_path) as connection:
        retired = dict(
            connection.execute(
                "SELECT id, retired_into_artist_id FROM local_artists "
                "WHERE id IN (?, ?, ?, ?)",
                (
                    survivor.id,
                    "artist-gated-retiree",
                    "artist-gated-zero-credit",
                    "artist-gated-embedded",
                ),
            ).fetchall()
        )
        candidates = {
            (left, right): (reason, state)
            for left, right, reason, state in connection.execute(
                "SELECT left_artist_id, right_artist_id, reason_code, state "
                "FROM local_artist_merge_candidates "
                "WHERE left_artist_id = ? OR right_artist_id = ?",
                (survivor.id, survivor.id),
            ).fetchall()
        }
        lingering_identities = connection.execute(
            "SELECT COUNT(*) FROM local_artist_external_identities "
            "WHERE local_artist_id IN (?, ?) AND provider = 'musicbrainz'",
            ("artist-gated-retiree", "artist-gated-zero-credit"),
        ).fetchone()[0]
        merges = connection.execute(
            "SELECT COUNT(*) FROM library_catalog_actions "
            "WHERE action_kind = 'merge_artist' "
            "AND reason_code = 'LEGACY_SYNTHETIC_ARTIST_IDENTITY'"
        ).fetchone()[0]
    expected_candidates = {}
    for retiree in (
        "artist-gated-retiree",
        "artist-gated-zero-credit",
        "artist-gated-embedded",
    ):
        left, right = sorted((survivor.id, retiree))
        expected_candidates[(left, right)] = ("FOLDED_NAME_COLLISION", "open")
    assert retired == {
        survivor.id: None,
        "artist-gated-retiree": None,
        "artist-gated-zero-credit": None,
        "artist-gated-embedded": None,
    }
    assert candidates == expected_candidates
    assert lingering_identities == 0
    assert merges == 0
    assert await repaired.get_catalog_revision() == revision_after_repair


@pytest.mark.asyncio
async def test_legacy_repair_with_complete_provider_proof_retires(
    store: NativeLibraryStore,
    db_path: Path,
) -> None:
    """N-03a: a legacy repair group whose retiree carries every-credit
    provider proof for the survivor MBID still retires as before the gate."""
    survivor_mbid = "7f3a9c1e-2b4d-4e6f-9a0b-1c2d3e4f5a6b"
    release_mbid = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
    release_track_mbid = "ffffffff-1111-4222-8333-444444444444"
    survivor = _artist("artist-proven-survivor", "Proven Artist")
    retiree_name = _artist("artist-proven-retiree", "Proven Artist")
    await store.create_catalog_membership(
        CatalogMembership(
            album=LocalAlbum(
                id="album-proven",
                root_id="root-1",
                grouping_key="group-proven",
                title="Proven Album",
                album_artist_id=survivor.id,
                album_artist_name=survivor.display_name,
                created_at=1,
                updated_at=1,
            ),
            artists=[survivor, retiree_name],
            tracks=[
                LocalTrack(
                    id="track-proven",
                    local_album_id="album-proven",
                    root_id="root-1",
                    file_path="/music/proven.flac",
                    relative_path="proven.flac",
                    path_hash="hash-proven",
                    file_size_bytes=100,
                    file_mtime_ns=200,
                    stat_revision="stat-proven",
                    title="Proven Track",
                    artist_name=survivor.display_name,
                    album_title="Proven Album",
                    album_artist_name=survivor.display_name,
                    file_format="flac",
                    imported_at=1,
                ),
            ],
            track_credits={
                "track-proven": [
                    LocalArtistCredit(
                        local_artist_id=retiree_name.id, position=0
                    )
                ]
            },
        )
    )
    with sqlite3.connect(db_path) as connection:
        connection.executemany(
            "INSERT INTO local_artist_external_identities "
            "(local_artist_id, provider, provider_artist_id, decision_source, selected_at) "
            "VALUES (?, 'musicbrainz', ?, 'legacy_import', 1)",
            [
                (survivor.id, survivor_mbid),
                (retiree_name.id, "e" * 32),
            ],
        )
        connection.execute(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, release_mbid, "
            "decision_source, selected_at) VALUES (?, 'musicbrainz', ?, ?, 'automatic', 1)",
            ("album-proven", release_mbid, release_mbid),
        )
        connection.execute(
            "INSERT INTO local_track_external_identities "
            "(local_track_id, provider, recording_mbid, release_mbid, "
            "release_track_mbid, decision_source, selected_at) "
            "VALUES (?, 'musicbrainz', ?, ?, ?, 'automatic', 1)",
            ("track-proven", release_mbid, release_mbid, release_track_mbid),
        )
        connection.execute(
            "INSERT INTO library_artist_credit_proofs "
            "(subject_kind, subject_id, local_album_id, local_track_id, credit_position, "
            "source_local_artist_id, local_artist_id, artist_mbid, canonical_name, "
            "credited_name, sort_name, join_phrase, release_mbid, release_track_mbid, "
            "album_identity_revision, track_identity_revision, evidence_hash, "
            "created_at, updated_at) "
            "VALUES ('track', 'track-proven', 'album-proven', 'track-proven', 0, ?, ?, ?, "
            "'Proven Artist', 'Proven Artist', '', '', ?, ?, 1, 1, ?, 1, 1)",
            (
                retiree_name.id,
                survivor.id,
                survivor_mbid,
                release_mbid,
                release_track_mbid,
                "0" * 64,
            ),
        )

    NativeLibraryStore(db_path, threading.Lock())
    NativeLibraryStore(db_path, threading.Lock())

    with sqlite3.connect(db_path) as connection:
        retired = connection.execute(
            "SELECT retired_into_artist_id FROM local_artists WHERE id = ?",
            (retiree_name.id,),
        ).fetchone()[0]
        candidate = connection.execute(
            "SELECT 1 FROM local_artist_merge_candidates "
            "WHERE (left_artist_id = ? AND right_artist_id = ?) "
            "OR (left_artist_id = ? AND right_artist_id = ?)",
            (survivor.id, retiree_name.id, retiree_name.id, survivor.id),
        ).fetchone()
        merge_actions = connection.execute(
            "SELECT COUNT(*) FROM library_catalog_actions "
            "WHERE action_kind = 'merge_artist' "
            "AND reason_code = 'LEGACY_SYNTHETIC_ARTIST_IDENTITY'"
        ).fetchone()[0]
    assert retired == survivor.id
    assert candidate is None
    assert merge_actions == 1


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
        "library_scan_failures",
        "library_scan_management_candidates",
        "library_scan_management_staging",
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


def test_scan_management_candidate_schema_upgrade_is_idempotent(
    db_path: Path,
) -> None:
    lock = threading.Lock()
    NativeLibraryStore(db_path, lock)
    with sqlite3.connect(db_path) as connection:
        connection.execute("DROP TABLE library_scan_management_candidates")
        connection.execute("DROP TABLE library_scan_management_staging")
        connection.execute("DROP INDEX idx_scan_inventory_management_candidates")

    NativeLibraryStore(db_path, lock)
    NativeLibraryStore(db_path, lock)

    with sqlite3.connect(db_path) as connection:
        tables = {
            row[0]
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type='table'"
            )
        }
        indexes = {
            row[0]
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type='index'"
            )
        }
    assert "library_scan_management_candidates" in tables
    assert "library_scan_management_staging" in tables
    assert {
        "idx_scan_inventory_management_candidates",
        "idx_scan_management_candidates_due",
    } <= indexes


@pytest.mark.asyncio
async def test_scan_failures_round_trip_pagination_and_dedupe(
    store: NativeLibraryStore,
) -> None:
    await store.create_scan_run(
        ScanRun(id="scan-fail", kind="incremental", trigger="manual", queued_at=1)
    )
    records = [
        ScanFailureRecord(
            root_id="root-1",
            relative_path=f"dir-{ordinal}",
            failure_code="WALK_EACCES",
            recorded_at=100 + ordinal,
            failure_detail=f"detail {ordinal}",
            phase="discovering",
        )
        for ordinal in range(3)
    ]
    await store.record_scan_failures("scan-fail", records)
    await store.record_scan_failures("scan-fail", [records[0]])

    first_page, cursor = await store.list_scan_run_failures("scan-fail", limit=2)

    assert [item.relative_path for item in first_page] == ["dir-0", "dir-1"]
    assert first_page[0].failure_code == "WALK_EACCES"
    assert first_page[0].failure_detail == "detail 0"
    assert first_page[0].phase == "discovering"
    assert first_page[0].recorded_at == 100
    assert cursor is not None

    second_page, next_cursor = await store.list_scan_run_failures(
        "scan-fail", limit=2, cursor_rowid=cursor
    )
    assert [item.relative_path for item in second_page] == ["dir-2"]
    assert next_cursor is None


@pytest.mark.asyncio
async def test_scan_failures_cascade_with_their_run(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_scan_run(
        ScanRun(id="scan-cascade", kind="incremental", trigger="manual", queued_at=1)
    )
    await store.record_scan_failures(
        "scan-cascade",
        [
            ScanFailureRecord(
                root_id="root-1",
                relative_path="dir",
                failure_code="WALK_TIMEOUT",
                recorded_at=100,
            )
        ],
    )

    with sqlite3.connect(db_path) as connection:
        connection.execute("PRAGMA foreign_keys = ON")
        connection.execute("DELETE FROM library_scan_runs WHERE id = 'scan-cascade'")
        remaining = connection.execute(
            "SELECT COUNT(*) FROM library_scan_failures"
        ).fetchone()[0]

    assert remaining == 0


@pytest.mark.asyncio
async def test_commit_scan_index_batch_records_failure_rows(
    store: NativeLibraryStore,
) -> None:
    await store.create_scan_run(
        ScanRun(id="scan-index", kind="incremental", trigger="manual", queued_at=1)
    )

    # NEW-SCAN-04: indexing failures carry their safe detail through the batch.
    await store.commit_scan_index_batch(
        "scan-index",
        writes=[],
        states={},
        failures=[
            ScanFailureRecord(
                root_id="root-1",
                relative_path="broken.flac",
                failure_code="TAG_READ_TIMEOUT",
                recorded_at=1.5,
                failure_detail=(
                    "The tag read exceeded its 30.0s deadline. A kernel-blocked "
                    "read is bounded by the timeout but the underlying syscall "
                    "may still be running."
                ),
                phase="indexing",
            )
        ],
        increments={},
        updated_at=2.0,
    )

    items, next_cursor = await store.list_scan_run_failures("scan-index")
    assert next_cursor is None
    assert [
        (item.root_id, item.relative_path, item.failure_code, item.phase)
        for item in items
    ] == [("root-1", "broken.flac", "TAG_READ_TIMEOUT", "indexing")]
    assert items[0].recorded_at == 1.5
    assert "30.0s deadline" in items[0].failure_detail
    assert "kernel-blocked" in items[0].failure_detail


@pytest.mark.asyncio
async def test_cleanup_terminal_scan_inventory_prunes_failure_rows(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_scan_run(
        ScanRun(id="scan-prune", kind="incremental", trigger="manual", queued_at=1)
    )
    await store.record_scan_failures(
        "scan-prune",
        [
            ScanFailureRecord(
                root_id="root-1",
                relative_path="dir",
                failure_code="WALK_EACCES",
                recorded_at=100,
            )
        ],
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_scan_runs SET terminal_at = 10, "
            "inventory_cleanup_pending = 1, state = 'failed' WHERE id = 'scan-prune'"
        )

    while True:
        _run_id, _deleted, done = await store.cleanup_terminal_scan_inventory()
        if done:
            break

    items, _cursor = await store.list_scan_run_failures("scan-prune")
    assert items == []


@pytest.mark.asyncio
async def test_scan_management_candidates_follow_album_and_run_lifetimes(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_scan_run(
        ScanRun(id="scan-candidate", kind="incremental", trigger="manual", queued_at=1)
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute(
            "INSERT INTO local_albums "
            "(id,root_id,grouping_key,title,title_folded,album_artist_id,"
            "grouping_source,created_at,updated_at) "
            "VALUES ('album-candidate','root-1','candidate','Candidate','candidate',"
            "?,'automatic',1,1)",
            (UNKNOWN_ARTIST_ID,),
        )
        connection.execute(
            "INSERT INTO library_scan_management_candidates "
            "(run_id,local_album_id,next_attempt_at) "
            "VALUES ('scan-candidate','album-candidate',1)"
        )
        connection.execute(
            "INSERT INTO library_scan_management_staging(run_id,staged_at) "
            "VALUES ('scan-candidate',1)"
        )

        connection.execute("DELETE FROM local_albums WHERE id='album-candidate'")
        candidates_after_album = connection.execute(
            "SELECT COUNT(*) FROM library_scan_management_candidates"
        ).fetchone()[0]
        staging_after_album = connection.execute(
            "SELECT COUNT(*) FROM library_scan_management_staging"
        ).fetchone()[0]

        connection.execute("DELETE FROM library_scan_runs WHERE id='scan-candidate'")
        staging_after_run = connection.execute(
            "SELECT COUNT(*) FROM library_scan_management_staging"
        ).fetchone()[0]

    assert candidates_after_album == 0
    assert staging_after_album == 1
    assert staging_after_run == 0


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
async def test_target_release_pins_ignore_fileless_provider_ghosts(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership("ghost-a", with_track=False))
    await store.create_catalog_membership(_membership("ghost-b", with_track=False))
    with sqlite3.connect(db_path) as connection:
        connection.executemany(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, decision_source, selected_at) "
            "VALUES (?, 'musicbrainz', 'ghost-rg', 'manual', 2)",
            [("album-ghost-a",), ("album-ghost-b",)],
        )

    assert await store.get_target_album_release_pin("ghost-rg") is None
    with pytest.raises(ResourceNotFoundError, match="not in the local library"):
        await store.set_target_album_release_pin(
            "ghost-rg", "ghost-release", "admin", "target-time"
        )
    assert await store.clear_target_album_release_pin("ghost-rg") is False



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
async def test_identification_snapshot_counts_attention_and_deferral_reasons(
    store: NativeLibraryStore, db_path: Path
) -> None:
    subjects = {
        "job-provider-queued": "album-1",
        "job-provider-paused": "album-2",
        "job-subject-queued": "album-3",
        "job-attention-cap": "album-4",
        "job-attention-subject": "album-5",
        "job-failed-other": "album-6",
    }
    for suffix in ("1", "2", "3", "4", "5", "6"):
        await store.create_catalog_membership(_membership(suffix))
    for job_id, album_id in subjects.items():
        await store.enqueue_identification_job(
            IdentificationJob(
                id=job_id,
                dedupe_key=f"automatic:{album_id}:rev",
                local_album_id=album_id,
                created_at=10,
            )
        )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_identification_jobs SET last_failure_code = "
            "'PROVIDER_TEMPORARILY_UNAVAILABLE' WHERE id IN "
            "('job-provider-queued', 'job-provider-paused')"
        )
        connection.execute(
            "UPDATE library_identification_jobs SET state = 'paused' "
            "WHERE id = 'job-provider-paused'"
        )
        connection.execute(
            "UPDATE library_identification_jobs SET last_failure_code = "
            "'SUBJECT_NOT_AVAILABLE' WHERE id = 'job-subject-queued'"
        )
        connection.execute(
            "UPDATE library_identification_jobs SET state = 'failed', terminal_at = 12, "
            "last_failure_code = 'MAX_DEFERRALS_EXCEEDED' WHERE id = 'job-attention-cap'"
        )
        connection.execute(
            "UPDATE library_identification_jobs SET state = 'failed', terminal_at = 13, "
            "last_failure_code = 'SUBJECT_NOT_AVAILABLE' "
            "WHERE id = 'job-attention-subject'"
        )
        connection.execute(
            "UPDATE library_identification_jobs SET state = 'failed', terminal_at = 14, "
            "last_failure_code = 'UNRELATED_CODE' WHERE id = 'job-failed-other'"
        )

    snapshot = await store.get_identification_activity_snapshot(now=10)
    assert snapshot["attention_count"] == 2
    assert snapshot["deferred_reason_counts"] == {
        "PROVIDER_TEMPORARILY_UNAVAILABLE": 2,
        "SUBJECT_NOT_AVAILABLE": 1,
    }
    assert snapshot["deferred_count"] == 3


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
        connection.execute(
            "UPDATE library_identification_jobs SET "
            "last_failure_code = 'UNEXPECTED_ERROR', attempt_count = 3, "
            "updated_at = 11 WHERE id = 'job-waiting'"
        )

    snapshot = await store.get_identification_activity_snapshot(now=12)
    assert snapshot["counts"] == {"failed": 1, "queued": 1}
    assert snapshot["started_at"] == 10
    assert snapshot["failure_event_id"] == "job-failed"
    assert snapshot["failure_at"] == 12
    assert snapshot["foreground_operation_count"] == 0
    assert snapshot["active_priority"] == 100
    assert snapshot["kept_local_count"] == 0
    assert snapshot["deferred_jobs"] == [
        {
            "job_id": "job-waiting",
            "local_album_id": "album-1",
            "album_title": "Album 1",
            "artist_name": "Artist 1",
            "last_failure_code": "UNEXPECTED_ERROR",
            "attempt_count": 3,
            "not_before": 0.0,
            "updated_at": 11.0,
        }
    ]

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
    assert (await store.get_identification_activity_snapshot(now=13))[
        "foreground_operation_count"
    ] == 1

    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_operation_jobs SET state = 'ready' "
            "WHERE id = 'foreground-operation'"
        )
    assert (await store.get_identification_activity_snapshot(now=13))[
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


def test_identification_jobs_attention_cause_ratchet_is_idempotent(
    db_path: Path,
) -> None:
    lock = threading.Lock()
    NativeLibraryStore(db_path, lock)
    NativeLibraryStore(db_path, lock)
    NativeLibraryStore(db_path, lock)

    with sqlite3.connect(db_path) as connection:
        columns = {
            str(row[1])
            for row in connection.execute(
                "PRAGMA table_info(library_identification_jobs)"
            )
        }
    assert "attention_cause" in columns


@pytest.mark.asyncio
async def test_terminal_fail_identification_job_surfaces_one_review_row(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership())

    async def fail_album_job(
        job_id: str, kind: str, failure_code: str, now: float
    ) -> None:
        await store.enqueue_identification_job(
            IdentificationJob(
                id=job_id,
                local_album_id="album-1",
                kind=kind,
                dedupe_key=f"{kind}:album-1:one",
                input_revision="one",
                priority=20,
                created_at=now - 1,
            )
        )
        claimed = await store.claim_identification_job(
            "worker", now=now, lease_seconds=60
        )
        assert claimed is not None
        await store.terminal_fail_identification_job(
            str(claimed["id"]),
            worker_id="worker",
            expected_job_revision=int(claimed["row_revision"]),
            failure_code=failure_code,
            attention_cause=failure_code,
            now=now,
        )

    await fail_album_job("job-auto", "automatic", "MAX_DEFERRALS_EXCEEDED", 3)
    with sqlite3.connect(db_path) as connection:
        reviews = connection.execute(
            "SELECT state, reason_code, attempt_id, input_revision, local_track_id "
            "FROM library_identification_reviews"
        ).fetchall()
    assert reviews == [("needs_review", "MAX_DEFERRALS_EXCEEDED", None, "one", None)]

    # A second terminal failure for the same album + input revision dedupes.
    await fail_album_job("job-retry", "review_retry", "SUBJECT_NOT_AVAILABLE", 6)
    with sqlite3.connect(db_path) as connection:
        count = connection.execute(
            "SELECT COUNT(*) FROM library_identification_reviews"
        ).fetchone()[0]
    assert count == 1

    # Track-scoped terminal failures never create review rows.
    await store.enqueue_identification_job(
        IdentificationJob(
            id="job-track",
            local_track_id="track-1",
            kind="automatic",
            dedupe_key="automatic:track-1:one",
            input_revision="one",
            priority=20,
            created_at=7,
        )
    )
    claimed = await store.claim_identification_job("worker", now=8, lease_seconds=60)
    assert claimed is not None
    await store.terminal_fail_identification_job(
        str(claimed["id"]),
        worker_id="worker",
        expected_job_revision=int(claimed["row_revision"]),
        failure_code="MAX_DEFERRALS_EXCEEDED",
        attention_cause="MAX_DEFERRALS_EXCEEDED",
        now=9,
    )
    with sqlite3.connect(db_path) as connection:
        count = connection.execute(
            "SELECT COUNT(*) FROM library_identification_reviews"
        ).fetchone()[0]
    assert count == 1


@pytest.mark.asyncio
async def test_dismiss_review_resolves_without_touching_tracks_and_cancels_jobs(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await store.create_catalog_membership(_membership())
    # A capped automatic job surfaces its own review row with attention markers.
    await store.enqueue_identification_job(
        IdentificationJob(
            id="job-capped",
            local_album_id="album-1",
            kind="automatic",
            dedupe_key="automatic:album-1:one",
            input_revision="one",
            priority=20,
            created_at=1,
        )
    )
    claimed = await store.claim_identification_job("worker", now=2, lease_seconds=60)
    assert claimed is not None
    await store.terminal_fail_identification_job(
        str(claimed["id"]),
        worker_id="worker",
        expected_job_revision=int(claimed["row_revision"]),
        failure_code="MAX_DEFERRALS_EXCEEDED",
        attention_cause="UNEXPECTED_ERROR",
        now=3,
    )
    # An unrelated queued job for the same album is cancelled by the decision.
    await store.enqueue_identification_job(
        IdentificationJob(
            id="job-auto",
            local_album_id="album-1",
            kind="automatic",
            dedupe_key="automatic:album-1:two",
            input_revision="two",
            priority=20,
            created_at=4,
        )
    )
    with sqlite3.connect(db_path) as connection:
        review_row = connection.execute(
            "SELECT id FROM library_identification_reviews WHERE local_album_id = 'album-1'"
        ).fetchone()
    assert review_row is not None
    review_id = str(review_row[0])
    catalog_revision = await store.get_catalog_revision()
    assert (await store.get_identification_activity_snapshot(now=4))["attention_count"] == 1

    result = await store.apply_review_decision(
        review_id,
        action="dismiss",
        actor_user_id="admin",
        expected_review_revision=1,
        expected_catalog_revision=catalog_revision,
        expected_identity_revision=None,
        action_id="action-dismiss",
        idempotency_key=None,
        now=5,
    )

    assert result["review"]["state"] == "resolved"
    assert result["review"]["reason_code"] == "DISMISS"
    with sqlite3.connect(db_path) as connection:
        review = connection.execute(
            "SELECT state, reason_code, decided_by_user_id, decided_at "
            "FROM library_identification_reviews WHERE id = ?",
            (review_id,),
        ).fetchone()
        track = connection.execute(
            "SELECT availability, manual_excluded FROM local_tracks WHERE id = 'track-1'"
        ).fetchone()
        queued = connection.execute(
            "SELECT state, last_failure_code FROM library_identification_jobs "
            "WHERE id = 'job-auto'"
        ).fetchone()
        capped = connection.execute(
            "SELECT state, last_failure_code, attention_cause "
            "FROM library_identification_jobs WHERE id = 'job-capped'"
        ).fetchone()
        audit = connection.execute(
            "SELECT action_kind, reason_code FROM library_catalog_actions "
            "WHERE id = 'action-dismiss'"
        ).fetchone()
    assert review == ("resolved", "DISMISS", "admin", 5)
    assert track == ("indexed", 0)
    assert queued == ("cancelled", "ADMIN_DECISION")
    # The capped job stays failed for audit but stops counting as attention.
    assert capped == ("failed", None, None)
    assert (await store.get_identification_activity_snapshot(now=5))["attention_count"] == 0
    assert audit == ("dismiss", "DISMISS")


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
    wake_revision = store.work_wakeups.revision("identification")
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
    assert store.work_wakeups.revision("identification") == wake_revision + 2
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
    wake_revision = store.work_wakeups.revision("operation")
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
    assert store.work_wakeups.revision("operation") == wake_revision + 1
    claim = await store.claim_operation_job("worker", now=1, lease_seconds=10)
    assert claim is not None
    assert await store.heartbeat_operation_job(
        "operation-1", "worker", now=2, lease_seconds=10
    )
    after_heartbeat = await store.get_operation_job("operation-1")
    assert after_heartbeat is not None
    assert after_heartbeat["row_revision"] == claim["row_revision"]
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
    assert (work_revision, job_revision, stream_revision) == (3, 3, 2)
    assert (
        _scalar(
            db_path,
            "SELECT completed_count FROM library_operation_jobs WHERE id = 'operation-1'",
        )
        == 1
    )


@pytest.mark.asyncio
async def test_claim_operation_job_defers_until_retry_not_before(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """A job deferred with next_attempt_at must not be re-claimed before it is
    due (the provider-defer hot-spin regression), and claiming clears it."""
    await store.create_catalog_membership(_membership())
    await store.create_operation_with_work(
        OperationJob(id="operation-deferred", kind="repair", created_at=1),
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
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_operation_jobs SET next_attempt_at = 50 "
            "WHERE id = 'operation-deferred'"
        )
    assert await store.claim_operation_job("worker", now=49, lease_seconds=10) is None
    claimed = await store.claim_operation_job("worker", now=50, lease_seconds=10)
    assert claimed is not None
    assert claimed["next_attempt_at"] is None


@pytest.mark.asyncio
async def test_start_repair_apply_wakes_sleeping_operation_worker(
    store: NativeLibraryStore, db_path: Path
) -> None:
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_operation_jobs "
            "(id, kind, state, created_at, updated_at) "
            "VALUES ('repair-apply-1', 'repair', 'ready', 1, 1)"
        )
        connection.execute(
            "INSERT INTO library_repair_snapshots "
            "(job_id, scope_json, target_matcher_version, created_at) "
            "VALUES ('repair-apply-1', '{}', 'matcher-1', 1)"
        )
        connection.commit()
    revision = store.work_wakeups.revision("operation")
    waiting = asyncio.create_task(
        store.work_wakeups.wait(
            "operation", after_revision=revision, timeout_seconds=1.0
        )
    )
    await asyncio.sleep(0)

    job = await store.start_repair_apply(
        "repair-apply-1", expected_row_revision=1, now=2
    )

    assert job["state"] == "queued"
    assert await waiting is True
    assert store.work_wakeups.revision("operation") == revision + 1


@pytest.mark.asyncio
async def test_deferred_repair_rotates_behind_other_queued_repairs(
    store: NativeLibraryStore,
) -> None:
    await store.create_catalog_membership(_membership())
    for ordinal, created_at in enumerate((1.0, 2.0), start=1):
        await store.create_operation_with_work(
            OperationJob(
                id=f"operation-{ordinal}", kind="repair", created_at=created_at
            ),
            [
                OperationWorkItem(
                    ordinal=0,
                    local_album_id="album-1",
                    expected_subject_revision=1,
                    expected_input_revision=f"input-{ordinal}",
                    action="repair",
                    idempotency_key=f"album-{ordinal}",
                )
            ],
        )

    first = await store.claim_operation_job(
        "worker", now=3, lease_seconds=10, kind="repair"
    )
    assert first is not None
    assert first["id"] == "operation-1"
    work = await store.claim_operation_work("operation-1", "worker", now=3)
    assert work is not None

    await store.defer_catalog_identity_hygiene_work(
        job_id="operation-1",
        ordinal=0,
        worker_id="worker",
        reason_code="SCAN_ACTIVE",
        now=4,
    )

    second = await store.claim_operation_job(
        "worker", now=5, lease_seconds=10, kind="repair"
    )
    assert second is not None
    assert second["id"] == "operation-2"


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


def test_operation_job_schema_construction_is_idempotent_with_retry_column(
    tmp_path: Path,
) -> None:
    """F-IDENT-03: the additive reidentification_attempt_count column exists on
    both fresh and repeated construction of the same database path."""
    db_file = tmp_path / "library.db"
    with sqlite3.connect(db_file) as connection:
        connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
        connection.execute("INSERT INTO auth_users VALUES ('admin')")

    first = NativeLibraryStore(db_file, threading.Lock())
    first._ensure_tables()
    second = NativeLibraryStore(db_file, threading.Lock())
    second._ensure_tables()

    with sqlite3.connect(db_file) as connection:
        columns = {
            row[1]
            for row in connection.execute(
                "PRAGMA table_info(library_operation_jobs)"
            ).fetchall()
        }
    assert "reidentification_attempt_count" in columns
    assert "next_attempt_at" in columns


@pytest.mark.asyncio
async def test_defer_reidentification_work_requires_the_running_lease(
    tmp_path: Path,
) -> None:
    """A defer under a stale worker lease fails closed without queueing work."""
    from core.exceptions import StaleRevisionError

    db_file = tmp_path / "library.db"
    with sqlite3.connect(db_file) as connection:
        connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
        connection.execute("INSERT INTO auth_users VALUES ('admin')")
    store = NativeLibraryStore(db_file, threading.Lock())

    with pytest.raises(StaleRevisionError):
        await store.defer_reidentification_work(
            "missing-job",
            0,
            worker_id="worker",
            reason_code="PROVIDER_TEMPORARILY_UNAVAILABLE",
            now=5.0,
            retry_not_before=125.0,
        )


@pytest.mark.asyncio
async def test_resolve_canonical_target_ids_covers_provider_alias_retired_and_ambiguous(
    store: NativeLibraryStore,
) -> None:
    """F-TARGETCATALOG-06: the batch resolver returns exactly one mapping per
    unambiguous identifier and omits ambiguous ones entirely."""
    await store.create_catalog_membership(_membership("1"))
    await store.create_catalog_membership(_membership("2"))
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-1",
            release_group_mbid="rg-shared",
            release_mbid="release-1",
            selected_at=2,
        ),
        expected_album_revision=1,
    )
    # album-2 shares the release-group identity of album-1 while staying an
    # active indexed album, so "rg-shared" is ambiguous across both.
    connection = sqlite3.connect(store.db_path)
    try:
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, decision_source, "
            "selected_at) VALUES ('album-2', 'musicbrainz', 'rg-shared', "
            "'manual', 3)"
        )
        connection.execute(
            "INSERT INTO local_album_aliases (alias, local_album_id, kind, "
            "created_at) VALUES ('alias-one', 'album-1', 'merged_album', 3)"
        )
        connection.execute(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, "
            "title_folded, album_artist_id, grouping_source, created_at, "
            "updated_at, retired_into_album_id) VALUES ('album-retired', "
            "'root-1', 'group-retired', 'Album Retired', 'album retired', "
            "(SELECT album_artist_id FROM local_albums WHERE id = 'album-1'), "
            "'automatic', 4, 4, 'album-1')"
        )
        connection.commit()
    finally:
        connection.close()

    resolved = await store.resolve_canonical_target_ids(
        "album",
        [
            "album-1",          # local ID
            "alias-one",        # alias
            "album-retired",    # retired ID -> survivor
            "release-1",        # provider release MBID
            "rg-shared",        # ambiguous across two active albums
            "missing-id",       # unknown
            "album-1",          # duplicate input
        ],
    )

    assert resolved["album-1"] == "album-1"
    assert resolved["alias-one"] == "album-1"
    assert resolved["album-retired"] == "album-1"
    assert resolved["release-1"] == "album-1"
    assert "rg-shared" not in resolved
    assert "missing-id" not in resolved


@pytest.mark.asyncio
async def test_get_target_album_tracks_batch_groups_indexed_rows(
    store: NativeLibraryStore,
) -> None:
    """F-TARGETCATALOG-06: one batch read groups indexed rows per canonical
    album; unavailable tracks are skipped and unknown IDs get empty lists."""
    await store.create_catalog_membership(_membership("1"))
    await store.create_catalog_membership(_membership("2"))

    grouped = await store.get_target_album_tracks_batch(
        ["album-1", "album-2", "album-missing"]
    )
    assert [row["id"] for row in grouped["album-1"]] == ["track-1"]
    assert [row["id"] for row in grouped["album-2"]] == ["track-2"]
    assert grouped["album-missing"] == []

    single = await store.get_target_album_tracks("album-2")
    assert [
        (row["id"], row["disc_number"], row["track_number"])
        for row in grouped["album-2"]
    ] == [(row["id"], row["disc_number"], row["track_number"]) for row in single]

    # Unavailable tracks stay out of the batch result.
    connection = sqlite3.connect(store.db_path)
    try:
        connection.execute(
            "UPDATE local_tracks SET availability = 'missing' "
            "WHERE id = 'track-2'"
        )
        connection.commit()
    finally:
        connection.close()
    grouped_after = await store.get_target_album_tracks_batch(["album-2"])
    assert grouped_after["album-2"] == []


@pytest.mark.asyncio
async def test_get_target_album_tracks_ignores_stale_release_group_alias(
    store: NativeLibraryStore,
) -> None:
    """A legacy RG alias with no indexed files must not mask the one active
    owner of that provider identity. Direct local IDs remain edition-scoped."""
    await store.create_catalog_membership(_membership("stale"))
    await store.create_catalog_membership(_membership("active"))
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET availability = 'missing' "
            "WHERE id = 'track-stale'"
        )
        connection.executemany(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, decision_source, "
            "selected_at) VALUES (?, 'musicbrainz', 'rg-shared', 'legacy_import', 2)",
            [("album-stale",), ("album-active",)],
        )
        connection.execute(
            "INSERT INTO local_album_aliases "
            "(alias, local_album_id, kind, created_at) "
            "VALUES ('rg-shared', 'album-stale', 'legacy_release_group', 2)"
        )
        connection.commit()

    resolved = await store.get_target_album_tracks("rg-shared")

    assert [row["id"] for row in resolved] == ["track-active"]
    assert await store.get_target_album_tracks("album-stale") == []
    unavailable = await store.get_target_album_tracks(
        "rg-shared", include_unavailable=True
    )
    assert [row["id"] for row in unavailable] == ["track-stale"]


@pytest.mark.asyncio
async def test_get_target_album_tracks_stale_alias_does_not_merge_ambiguous_editions(
    store: NativeLibraryStore,
) -> None:
    """An RG alone cannot choose between two active local editions."""
    for suffix in ("stale", "edition-a", "edition-b"):
        await store.create_catalog_membership(_membership(suffix))
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET availability = 'missing' "
            "WHERE id = 'track-stale'"
        )
        connection.executemany(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, release_mbid, "
            "decision_source, selected_at) "
            "VALUES (?, 'musicbrainz', 'rg-shared', ?, 'legacy_import', 2)",
            [
                ("album-stale", None),
                ("album-edition-a", "release-a"),
                ("album-edition-b", "release-b"),
            ],
        )
        connection.execute(
            "INSERT INTO local_album_aliases "
            "(alias, local_album_id, kind, created_at) "
            "VALUES ('rg-shared', 'album-stale', 'legacy_release_group', 2)"
        )
        connection.commit()

    assert await store.get_target_album_tracks("rg-shared") == []
    assert [
        row["id"] for row in await store.get_target_album_tracks("album-edition-a")
    ] == ["track-edition-a"]
    assert [
        row["id"] for row in await store.get_target_album_tracks("album-edition-b")
    ] == ["track-edition-b"]


@pytest.mark.asyncio
async def test_disabled_fingerprint_outcome_is_overwritable_but_matched_is_not(
    store: NativeLibraryStore,
) -> None:
    """F-041: 'disabled' rows must yield to newer outcomes so tracks locked
    out by an absent AcoustID key recover; matched stays terminal (F-048)."""
    from models.identification import FingerprintOutcome

    await _seed_track_for_outcomes(store)

    def outcome(state: str, *, attempt: int = 1) -> FingerprintOutcome:
        return FingerprintOutcome(
            id="outcome-1",
            local_track_id="track-1",
            stat_revision="stat-1",
            fingerprinter_version=FINGERPRINTER_VERSION,
            state=state,
            recording_mbid="rec-new" if state == "matched" else None,
            attempt_count=attempt,
            first_attempt_at=1,
            last_attempt_at=2,
        )

    first_revision = await store.record_fingerprint_outcome(outcome("disabled"))
    assert first_revision >= 1

    # disabled -> matched overwrites
    second_revision = await store.record_fingerprint_outcome(outcome("matched"))
    stored = await store.get_fingerprint_outcome(
        "track-1", "stat-1", FINGERPRINTER_VERSION
    )
    assert stored is not None and stored.state == "matched"
    assert second_revision > first_revision

    # matched -> matched is a terminal short-circuit (revision unchanged)
    third_revision = await store.record_fingerprint_outcome(
        outcome("matched", attempt=9)
    )
    assert third_revision == second_revision


@pytest.mark.asyncio
async def test_racing_outcome_writes_keep_attempt_count_monotonic(
    store: NativeLibraryStore,
) -> None:
    """F-046: interleaved non-terminal writes bump attempt_count via SQL so
    the stored count drifts monotonically even under last-write-wins."""
    import asyncio as _asyncio

    from models.identification import FingerprintOutcome

    await _seed_track_for_outcomes(store)
    base = dict(
        id="outcome-race",
        local_track_id="track-1",
        stat_revision="stat-1",
        fingerprinter_version=FINGERPRINTER_VERSION,
        state="deferred",
        failure_code="LOOKUP_PENDING",
        first_attempt_at=1,
        last_attempt_at=2,
        retry_after=None,
    )

    async def write(suffix: str) -> int:
        payload = dict(base)
        payload["id"] = f"outcome-{suffix}"
        return await store.record_fingerprint_outcome(FingerprintOutcome(**payload))

    await write("a")
    await _asyncio.gather(write("b"), write("c"))

    row = await store.get_fingerprint_outcome(
        "track-1", "stat-1", FINGERPRINTER_VERSION
    )
    assert row is not None
    assert row.attempt_count >= 3  # seed + two racing writes, never reset


@pytest.mark.asyncio
async def test_provider_reset_wipes_are_bounded_and_stale_gated(
    store: NativeLibraryStore,
) -> None:
    """F-056: provider-coded deferrals lose their attempt history at most
    TWICE per streak and only after the staleness window; afterwards only a
    far-future not_before is released."""
    await _seed_track_for_outcomes(store, album_id="album-1", track_id="track-reset", stat_revision="stat-reset")
    job = IdentificationJob(id="job-reset", dedupe_key="d-reset", local_album_id="album-1")
    await store.enqueue_identification_job(job)
    claimed = await store.claim_identification_job("worker", now=10, lease_seconds=60)
    assert claimed is not None
    await store.defer_identification_job(
        "job-reset",
        worker_id="worker",
        expected_job_revision=int(claimed["row_revision"]),
        failure_code="PROVIDER_TEMPORARILY_UNAVAILABLE",
        not_before=20,
        now=11,
    )

    # Fresh row inside the staleness window: untouched.
    assert (
        await store.reset_provider_identification_deferrals(
            now=100, staleness_seconds=7680
        )
        == 0
    )

    async def redeferrable_state() -> tuple[int, int]:
        with sqlite3.connect(store.db_path) as connection:
            row = connection.execute(
                "SELECT attempt_count, provider_reset_count FROM "
                "library_identification_jobs WHERE id='job-reset'"
            ).fetchone()
        return row

    # Age the row past the window: wipe #1 zeroes attempts.
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE library_identification_jobs SET updated_at = ? WHERE id='job-reset'",
            (100 - 8000,),
        )
    assert (
        await store.reset_provider_identification_deferrals(
            now=100, staleness_seconds=7680
        )
        == 1
    )
    attempt_count, reset_count = await redeferrable_state()
    assert (attempt_count, reset_count) == (0, 1)

    # Wipe #2 is still allowed.
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE library_identification_jobs SET state='queued', "
            "last_failure_code='PROVIDER_TEMPORARILY_UNAVAILABLE', updated_at=? "
            "WHERE id='job-reset'",
            (100 - 8000,),
        )
    await store.claim_identification_job("worker", now=110, lease_seconds=60)
    await store.defer_identification_job(
        "job-reset",
        worker_id="worker",
        expected_job_revision=_current_revision(store, "job-reset"),
        failure_code="PROVIDER_TEMPORARILY_UNAVAILABLE",
        not_before=200,
        now=111,
    )
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE library_identification_jobs SET updated_at=? WHERE id='job-reset'",
            (100 - 8000,),
        )
    assert (
        await store.reset_provider_identification_deferrals(
            now=100, staleness_seconds=7680
        )
        == 1
    )
    attempt_count, reset_count = await redeferrable_state()
    assert (attempt_count, reset_count) == (0, 2)

    # Third cycle: no more wipes - attempts are preserved going forward.
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE library_identification_jobs SET state='queued', "
            "last_failure_code='PROVIDER_TEMPORARILY_UNAVAILABLE', updated_at=? "
            "WHERE id='job-reset'",
            (100 - 8000,),
        )
    before = await redeferrable_state()
    touched = await store.reset_provider_identification_deferrals(
        now=100_000, staleness_seconds=7680
    )
    after = await redeferrable_state()
    assert after[1] == 2 and touched in (0, 1)


def _current_revision(store: NativeLibraryStore, job_id: str) -> int:
    with sqlite3.connect(store.db_path) as connection:
        return int(
            connection.execute(
                "SELECT row_revision FROM library_identification_jobs WHERE id=?",
                (job_id,),
            ).fetchone()[0]
        )


async def _seed_track_for_outcomes(
    store: NativeLibraryStore,
    *,
    album_id: str = "album-1",
    track_id: str = "track-1",
    stat_revision: str = "stat-1",
) -> None:
    from models.local_catalog import (
        CatalogMembership,
        LocalAlbum,
        LocalArtist,
        LocalArtistCredit,
        LocalTrack,
    )

    artist = LocalArtist(
        id=f"artist-{album_id}",
        display_name="Artist",
        folded_name="artist",
        normalized_name="artist",
        kind="group",
        created_at=1,
        updated_at=1,
    )
    album = LocalAlbum(
        id=album_id,
        root_id="root",
        grouping_key=f"group-{album_id}",
        title="Album",
        album_artist_id=artist.id,
        album_artist_name="Artist",
        created_at=1,
        updated_at=1,
    )
    track = LocalTrack(
        id=track_id,
        local_album_id=album.id,
        root_id="root",
        file_path=f"/music/{track_id}.flac",
        relative_path=f"{track_id}.flac",
        path_hash=f"hash-{track_id}",
        file_size_bytes=1,
        file_mtime_ns=2,
        stat_revision=stat_revision,
        tag_revision=f"tag-{track_id}",
        title="Track",
        artist_name="Artist",
        album_title="Album",
        album_artist_name="Artist",
        track_number=1,
        duration_seconds=180,
        file_format="flac",
        imported_at=1,
    )
    await store.create_catalog_membership(
        CatalogMembership(
            album=album,
            artists=[artist],
            tracks=[track],
            album_credits=[LocalArtistCredit(local_artist_id=artist.id, position=0)],
            track_credits={track.id: [LocalArtistCredit(local_artist_id=artist.id, position=0)]},
        )
    )


@pytest.mark.asyncio
async def test_begin_apply_rejects_settings_drift_in_transaction(
    store: NativeLibraryStore,
) -> None:
    """F-079: settings/policy drift is rejected by begin_library_management_apply
    itself (one clean rejection) instead of surfacing as per-bundle failures."""
    from core.exceptions import StaleRevisionError as _Stale

    job_id = "job-drift"
    with sqlite3.connect(store.db_path) as connection:
        current_catalog = int(
            connection.execute(
                "SELECT value FROM library_catalog_revision WHERE singleton=1"
            ).fetchone()[0]
        )
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "INSERT INTO library_operation_jobs "
            "(id, kind, state, lease_owner, expected_work_count, completed_count, "
            "succeeded_count, failed_count, skipped_count, control_request, "
            "reidentification_attempt_count, created_at, phase_timings_json, "
            "updated_at, row_revision, event_revision) VALUES "
            "(?, 'library_management', 'ready', 'worker', 1, 0, 0, 0, 0, 'none', 0, "
            "100, '{}', 100, 1, 0)",
            (job_id,),
        )
        connection.execute(
            "INSERT INTO library_management_job_snapshots "
            "(job_id, mode, origin, phase, selection_json, preview_token_hash, profile_revision, "
            "settings_revision, naming_revision, policy_revision, catalog_revision, "
            "profile_snapshot_json, intent_json, summary_json, warnings_json, "
            "created_at, updated_at, row_revision, preview_expires_at) VALUES "
            "(?, 'preview', 'manual', 'ready', '{}', 'token', 'p1', "
            "'settings-v1', 'n1', 'pol-v1', ?, '{}', '{}', '{}', '[]', "
            "100, 100, 1, 999999)",
            (job_id, current_catalog),
        )

    with pytest.raises(_Stale, match="settings changed before apply"):
        await store.begin_library_management_apply(
            job_id,
            preview_token_hash="token",
            expected_job_revision=1,
            idempotency_key="idem-1",
            now=150,
            current_settings_revision="settings-drifted",
            current_policy_revision="pol-v1",
        )

    with pytest.raises(_Stale, match="policy changed before apply"):
        await store.begin_library_management_apply(
            job_id,
            preview_token_hash="token",
            expected_job_revision=1,
            idempotency_key="idem-2",
            now=150,
            current_settings_revision="settings-v1",
            current_policy_revision="pol-drifted",
        )


@pytest.mark.asyncio
async def test_album_catalog_scope_ids_resolves_rg_and_credited_artists(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """Issue #369: scope ids must resolve without touching the track identity
    table (which has no provider_artist_id column). Album-level and
    track-level credited artists both contribute; uncredited identities do
    not; unknown albums return empty sets instead of raising."""
    await store.create_catalog_membership(_membership())
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO local_artists "
            "(id, display_name, folded_name, kind, created_at, updated_at) "
            "VALUES ('artist-guest', 'Guest', 'guest', 'person', 1, 1)"
        )
        connection.execute(
            "INSERT INTO local_artists "
            "(id, display_name, folded_name, kind, created_at, updated_at) "
            "VALUES ('artist-unrelated', 'Unrelated', 'unrelated', 'person', 1, 1)"
        )
        connection.execute(
            "INSERT INTO local_track_artists "
            "(local_track_id, position, local_artist_id, role, credited_name, "
            "join_phrase) VALUES ('track-1', 1, 'artist-guest', 'guest', "
            "'Guest', '')"
        )
        connection.execute(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, decision_source, "
            "selected_at) VALUES ('album-1', 'musicbrainz', 'rg-scope-1', "
            "'manual', 2)"
        )
        connection.executemany(
            "INSERT INTO local_artist_external_identities "
            "(local_artist_id, provider, provider_artist_id, decision_source, "
            "selected_at) VALUES (?, 'musicbrainz', ?, 'manual', 2)",
            [
                ("artist-1", "artist-scope-1"),
                ("artist-guest", "artist-scope-guest"),
                ("artist-unrelated", "artist-scope-unrelated"),
            ],
        )

    rg_ids, artist_ids = await store.album_catalog_scope_ids("album-1")

    assert rg_ids == {"rg-scope-1"}
    assert artist_ids == {"artist-scope-1", "artist-scope-guest"}


@pytest.mark.asyncio
async def test_album_catalog_scope_ids_missing_album_returns_empty_sets(
    store: NativeLibraryStore,
) -> None:
    """Issue #369: requesting an edition for a missing album must yield
    valid empty scope sets (lists still sweep; no entity keys to delete),
    not raise sqlite3.OperationalError."""
    assert await store.album_catalog_scope_ids("album-missing") == (set(), set())


def _identity_snapshot(db_path: Path) -> dict[str, list[tuple]]:
    with sqlite3.connect(db_path) as connection:
        albums = connection.execute(
            "SELECT * FROM local_album_external_identities ORDER BY local_album_id"
        ).fetchall()
        tracks = connection.execute(
            "SELECT * FROM local_track_external_identities ORDER BY local_track_id"
        ).fetchall()
        artists = connection.execute(
            "SELECT * FROM local_artist_external_identities ORDER BY local_artist_id"
        ).fetchall()
    return {
        "albums": [tuple(row) for row in albums],
        "tracks": [tuple(row) for row in tracks],
        "artists": [tuple(row) for row in artists],
    }


async def _finish_identified(
    store: NativeLibraryStore,
    db_path: Path,
    *,
    job_id: str,
    attempt_id: str,
    review_id: str,
    rg: str,
    release: str | None,
    artist_mbid: str | None,
    track_mbids: dict[str, str],
    now: float,
    outcome: str = "identified",
    reason: str = "SUPPORTED",
    album_id: str = "album-1",
) -> None:
    """Enqueue, claim, and finish one album identification job (store-level)."""
    suffix = album_id.rsplit("-", 1)[-1]
    await store.enqueue_identification_job(
        IdentificationJob(
            id=job_id,
            local_album_id=album_id,
            kind="automatic",
            dedupe_key=f"automatic:{album_id}:{attempt_id}",
            input_revision="revision",
            priority=20,
            created_at=now - 1,
        )
    )
    claimed = await store.claim_identification_job("worker", now=now, lease_seconds=60)
    assert claimed is not None
    context = await store.get_album_identification_context(album_id)
    assert context is not None
    evidence = CandidateEvidence(
        release_group_mbid=rg,
        release_mbid=release,
        album_title=f"Album {suffix}",
        album_artist_name=f"Artist {suffix}",
        artist_mbid=artist_mbid,
        album_title_classification="supported",
        album_artist_classification="supported" if artist_mbid else "unknown",
        track_evidence=[
            TrackEvidence(
                local_track_id=track_id,
                classification="supported",
                recording_mbid=recording_mbid,
            )
            for track_id, recording_mbid in track_mbids.items()
        ],
        reason_code=reason,
    )
    tag_revision, stat_revision, policy_revision = album_input_revisions(
        context["tracks"]
    )
    with sqlite3.connect(db_path) as connection:
        album_revision = connection.execute(
            "SELECT row_revision FROM local_albums WHERE id = ?", (album_id,)
        ).fetchone()[0]
    await store.finish_identification_job(
        str(claimed["id"]),
        worker_id="worker",
        expected_job_revision=int(claimed["row_revision"]),
        expected_album_revision=int(album_revision),
        expected_input_revision=":".join(
            (tag_revision, stat_revision, policy_revision)
        ),
        attempt=IdentificationAttempt(
            id=attempt_id,
            local_album_id=album_id,
            input_tag_revision="tag-1",
            input_policy_revision="policy-1",
            input_file_revision="file-1",
            input_identity_revision=album_identity_revision(
                context["identity"],
                [
                    track
                    for track in context["tracks"]
                    if track["availability"] == "indexed"
                ],
            ),
            matcher_version="matcher-1",
            state=outcome,
            terminal_reason_code=reason,
            selected_candidate_key="rg:release" if outcome == "identified" else None,
            candidate_count=1,
            started_at=now,
            completed_at=now,
        ),
        evidence=[
            IdentificationEvidenceRecord(
                id=f"{attempt_id}-evidence",
                attempt_id=attempt_id,
                candidate_key="rg:release",
                evidence=evidence,
                created_at=now,
            )
        ],
        outcome=outcome,
        review_id=review_id,
        completed_at=now,
    )


@pytest.mark.asyncio
async def test_c01_manual_track_identity_survives_automatic_reidentification(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """C-01/T1: a manual track pick must survive an automatic album re-id.

    The album write is kept (partial-write) while the protected track row
    stays byte-identical, and the skip is recorded on the attempt."""
    await store.create_catalog_membership(_membership())
    await store.attach_track_identity(
        LocalTrackExternalIdentity(
            local_track_id="track-1",
            recording_mbid="manual-recording",
            release_mbid="manual-release",
            release_track_mbid="manual-release-track",
            medium_position=1,
            release_track_position=1,
            decision_source="manual",
            selected_at=5,
        ),
        expected_track_revision=1,
    )
    before = _identity_snapshot(db_path)
    assert len(before["tracks"]) == 1

    await _finish_identified(
        store,
        db_path,
        job_id="job-t1",
        attempt_id="attempt-t1",
        review_id="review-t1",
        rg="auto-rg",
        release="auto-release",
        artist_mbid=None,
        track_mbids={"track-1": "auto-recording"},
        now=10,
    )

    after = _identity_snapshot(db_path)
    assert after["tracks"] == before["tracks"]
    with sqlite3.connect(db_path) as connection:
        album_row = connection.execute(
            "SELECT release_group_mbid, release_mbid, decision_source "
            "FROM local_album_external_identities WHERE local_album_id = 'album-1'"
        ).fetchone()
        flags_json = connection.execute(
            "SELECT degradation_flags_json FROM library_identification_attempts "
            "WHERE id = 'attempt-t1'"
        ).fetchone()[0]
    assert tuple(album_row) == ("auto-rg", "auto-release", "automatic")
    assert "skipped_protected_tracks:1" in json.loads(str(flags_json))


@pytest.mark.asyncio
async def test_c02_manual_artist_identity_survives_automatic_reidentification(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """C-02/T2: a manual artist pick must survive automatic re-ids touching
    that artist - skipped on MBID clash (cross-artist clashes keep their
    merge candidate), re-stamped without downgrade on identical MBID."""
    await store.create_catalog_membership(_membership())
    await store.create_catalog_membership(_membership("2"))
    await store.attach_artist_identity_with_aliases(
        LocalArtistExternalIdentity(
            local_artist_id="artist-1",
            provider_artist_id="manual-artist",
            decision_source="manual",
            selected_by_user_id="admin",
            selected_at=5,
        ),
        [],
        expected_artist_revision=1,
    )

    def artist_row() -> tuple | None:
        with sqlite3.connect(db_path) as connection:
            row = connection.execute(
                "SELECT provider_artist_id, decision_source, attempt_id, "
                "selected_by_user_id, selected_at, row_revision "
                "FROM local_artist_external_identities "
                "WHERE local_artist_id = 'artist-1'"
            ).fetchone()
            return tuple(row) if row is not None else None

    # Self clash: incoming MBID owned by nobody - the write is skipped.
    await _finish_identified(
        store,
        db_path,
        job_id="job-t2a",
        attempt_id="attempt-t2a",
        review_id="review-t2a",
        rg="auto-rg-a",
        release="auto-release-a",
        artist_mbid="auto-artist-clash",
        track_mbids={},
        now=10,
    )
    assert artist_row() == ("manual-artist", "manual", None, "admin", 5, 1)

    # Identical MBID: idempotent re-stamp without downgrade or revision bump.
    await _finish_identified(
        store,
        db_path,
        job_id="job-t2b",
        attempt_id="attempt-t2b",
        review_id="review-t2b",
        rg="auto-rg-b",
        release="auto-release-b",
        artist_mbid="manual-artist",
        track_mbids={},
        now=11,
    )
    assert artist_row() == ("manual-artist", "manual", "attempt-t2b", "admin", 11, 1)

    # Cross-artist clash: artist-2 owns the incoming MBID - the manual row is
    # preserved and a SHARED_PROVIDER_IDENTITY candidate is filed.
    await store.attach_artist_identity_with_aliases(
        LocalArtistExternalIdentity(
            local_artist_id="artist-2",
            provider_artist_id="auto-artist-shared",
            decision_source="automatic",
            selected_at=12,
        ),
        [],
        expected_artist_revision=1,
    )
    await _finish_identified(
        store,
        db_path,
        job_id="job-t2c",
        attempt_id="attempt-t2c",
        review_id="review-t2c",
        rg="auto-rg-c",
        release="auto-release-c",
        artist_mbid="auto-artist-shared",
        track_mbids={},
        now=13,
    )
    assert artist_row() == ("manual-artist", "manual", "attempt-t2b", "admin", 11, 1)
    with sqlite3.connect(db_path) as connection:
        candidates = connection.execute(
            "SELECT left_artist_id, right_artist_id, reason_code "
            "FROM local_artist_merge_candidates"
        ).fetchall()
    assert [tuple(row) for row in candidates] == [
        ("artist-1", "artist-2", "SHARED_PROVIDER_IDENTITY")
    ]


@pytest.mark.asyncio
async def test_c03_contradictory_retracts_same_attempt_automatic_artist(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """C-03/T3: a contradictory re-id retracts the automatic artist row
    stamped by the retracted identification (plus album+tracks as before)
    while other attempts' automatic rows and manual rows are untouched.

    Regression guard: binding the current (contradictory) attempt id is
    vacuous - it stamps no artist row - so no seeded row carries the
    contradictory attempt id, and the PRIOR attempt's artist row must be
    genuinely GONE (not merely re-stamped or left in place)."""
    await store.create_catalog_membership(_membership())
    await store.create_catalog_membership(_membership("2"))
    await store.create_catalog_membership(_membership("3"))
    await store.attach_artist_identity_with_aliases(
        LocalArtistExternalIdentity(
            local_artist_id="artist-3",
            provider_artist_id="manual-artist-c",
            decision_source="manual",
            selected_by_user_id="admin",
            selected_at=5,
        ),
        [],
        expected_artist_revision=1,
    )
    await _finish_identified(
        store,
        db_path,
        job_id="job-t3a",
        attempt_id="attempt-t3a",
        review_id="review-t3a",
        rg="auto-rg",
        release="auto-release",
        artist_mbid="auto-artist-a",
        track_mbids={"track-1": "auto-recording"},
        now=10,
    )
    # Concurrent album identified between the two album-1 attempts: its own
    # automatic artist stamp must survive album-1's retraction.
    await _finish_identified(
        store,
        db_path,
        job_id="job-t3c",
        attempt_id="attempt-t3c",
        review_id="review-t3c",
        rg="auto-rg-2",
        release="auto-release-2",
        artist_mbid="auto-artist-b",
        track_mbids={"track-2": "auto-recording-2"},
        now=11,
        album_id="album-2",
    )
    with sqlite3.connect(db_path) as connection:
        artist_before = connection.execute(
            "SELECT local_artist_id, provider_artist_id, decision_source, attempt_id "
            "FROM local_artist_external_identities ORDER BY local_artist_id"
        ).fetchall()
    assert [tuple(row) for row in artist_before] == [
        ("artist-1", "auto-artist-a", "automatic", "attempt-t3a"),
        ("artist-2", "auto-artist-b", "automatic", "attempt-t3c"),
        ("artist-3", "manual-artist-c", "manual", None),
    ]

    await _finish_identified(
        store,
        db_path,
        job_id="job-t3b",
        attempt_id="attempt-t3b",
        review_id="review-t3b",
        rg="other-rg",
        release="other-release",
        artist_mbid=None,
        track_mbids={},
        now=12,
        outcome="contradictory",
        reason="CONFLICTING_TRACK_EVIDENCE",
    )

    with sqlite3.connect(db_path) as connection:
        album_rows = connection.execute(
            "SELECT local_album_id, release_group_mbid, release_mbid, "
            "decision_source, attempt_id FROM local_album_external_identities "
            "ORDER BY local_album_id"
        ).fetchall()
        track_rows = connection.execute(
            "SELECT local_track_id, recording_mbid, decision_source, attempt_id "
            "FROM local_track_external_identities ORDER BY local_track_id"
        ).fetchall()
        retracted_artist = connection.execute(
            "SELECT 1 FROM local_artist_external_identities "
            "WHERE local_artist_id = 'artist-1'"
        ).fetchone()
        artist_rows = connection.execute(
            "SELECT local_artist_id, provider_artist_id, decision_source, attempt_id "
            "FROM local_artist_external_identities ORDER BY local_artist_id"
        ).fetchall()
        review = connection.execute(
            "SELECT state, reason_code FROM library_identification_reviews "
            "WHERE local_album_id = 'album-1'"
        ).fetchone()
    # Album-1's rows are retracted; album-2's concurrent identification is intact.
    assert [tuple(row) for row in album_rows] == [
        ("album-2", "auto-rg-2", "auto-release-2", "automatic", "attempt-t3c")
    ]
    assert [tuple(row) for row in track_rows] == [
        ("track-2", "auto-recording-2", "automatic", "attempt-t3c")
    ]
    # The prior attempt's artist row is GONE - not re-stamped, not retained.
    assert retracted_artist is None
    assert [tuple(row) for row in artist_rows] == [
        ("artist-2", "auto-artist-b", "automatic", "attempt-t3c"),
        ("artist-3", "manual-artist-c", "manual", None),
    ]
    assert tuple(review) == ("needs_review", "CONFLICTING_TRACK_EVIDENCE")


async def _stage_merge_setup(
    store: NativeLibraryStore,
    db_path: Path,
    *,
    run_id: str,
    first_number: int,
    second_number: int,
) -> None:
    """Seed one two-track fold under distinct artists for the staged-merge tests."""
    await store.create_catalog_membership(_membership())
    await store.create_catalog_membership(_membership("2"))
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_scan_runs "
            "(id, kind, trigger, state, phase, aggregate_scope, queued_at, updated_at) "
            "VALUES (?, 'incremental', 'manual', 'indexing', 'indexing', 'root-1', 1, 1)",
            (run_id,),
        )
        connection.execute(
            "INSERT INTO library_scan_grouping_contexts "
            "(run_id, root_id, relative_directory) VALUES (?, 'root-1', '.')",
            (run_id,),
        )
        connection.commit()
    await store.stage_grouping_track_page(
        run_id,
        "root-1",
        ".",
        evidence=[
            {
                "local_track_id": "track-1",
                "preliminary_key": "tag:b",
                "title": "Album X",
                "title_normalized": "album x",
                "album_artist_name": "Artist One",
                "album_artist_normalized": "artist one",
                "track_number": first_number,
                "old_album_id": "album-1",
                "album_created_at": 1,
                "reason_code": "TAGGED",
            },
            {
                "local_track_id": "track-2",
                "preliminary_key": "tag:a",
                "title": "Album X",
                "title_normalized": "album x",
                "album_artist_name": "Artist Two",
                "album_artist_normalized": "artist two",
                "track_number": second_number,
                "old_album_id": "album-2",
                "album_created_at": 1,
                "reason_code": "TAGGED",
            },
        ],
        next_cursor=None,
        exhausted=True,
    )


@pytest.mark.asyncio
async def test_staged_merge_ignores_zero_track_number_collision(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """Staged parity: two all-unknown (0-numbered) rows in one fold under
    distinct artists must NOT count as a track-number collision blocking
    the staged merge (mirrors the missing-row absorb >0 convention)."""
    await _stage_merge_setup(
        store, db_path, run_id="scan-zero", first_number=0, second_number=0
    )
    await store.prepare_staged_grouping_tokens("scan-zero", "root-1", ".", limit=10)
    with sqlite3.connect(db_path) as connection:
        context = connection.execute(
            "SELECT grouping_merge_target, grouping_merge_ready "
            "FROM library_scan_grouping_contexts "
            "WHERE run_id = 'scan-zero' AND root_id = 'root-1' "
            "AND relative_directory = '.'"
        ).fetchone()
        tokens = connection.execute(
            "SELECT local_track_id, grouping_token "
            "FROM library_scan_grouping_evidence WHERE run_id = 'scan-zero' "
            "ORDER BY local_track_id"
        ).fetchall()
    assert tuple(context) == ("tag:a", 1)
    assert [tuple(row) for row in tokens] == [
        ("track-1", "tag:a"),
        ("track-2", "tag:a"),
    ]


@pytest.mark.asyncio
async def test_staged_merge_keeps_numbered_artist_collision_split(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """Control: the same fold with genuinely numbered rows under distinct
    artists still counts as a collision and blocks the staged merge."""
    await _stage_merge_setup(
        store, db_path, run_id="scan-numbered", first_number=5, second_number=5
    )
    await store.prepare_staged_grouping_tokens(
        "scan-numbered", "root-1", ".", limit=10
    )
    with sqlite3.connect(db_path) as connection:
        context = connection.execute(
            "SELECT grouping_merge_target, grouping_merge_ready "
            "FROM library_scan_grouping_contexts "
            "WHERE run_id = 'scan-numbered' AND root_id = 'root-1' "
            "AND relative_directory = '.'"
        ).fetchone()
        tokens = connection.execute(
            "SELECT local_track_id, grouping_token "
            "FROM library_scan_grouping_evidence WHERE run_id = 'scan-numbered' "
            "ORDER BY local_track_id"
        ).fetchall()
    assert tuple(context) == (None, 1)
    assert [tuple(row) for row in tokens] == [
        ("track-1", "tag:b"),
        ("track-2", "tag:a"),
    ]


@pytest.mark.asyncio
async def test_partial_decode_outcome_round_trip_and_idempotent_rewrite(
    store: NativeLibraryStore,
) -> None:
    """4.10a store part: partial_decode persists and reads back as bool;
    re-recording the same terminal path short-circuits idempotently."""
    from models.identification import FingerprintOutcome

    await _seed_track_for_outcomes(store)

    def outcome() -> FingerprintOutcome:
        # Constructed twice (same path): two distinct objects, one key.
        return FingerprintOutcome(
            id="outcome-partial",
            local_track_id="track-1",
            stat_revision="stat-1",
            fingerprinter_version=FINGERPRINTER_VERSION,
            state="skipped",
            partial_decode=True,
            first_attempt_at=1,
            last_attempt_at=2,
        )

    first_revision = await store.record_fingerprint_outcome(outcome())
    stored = await store.get_fingerprint_outcome(
        "track-1", "stat-1", FINGERPRINTER_VERSION
    )
    assert stored is not None
    assert stored.partial_decode is True

    second_revision = await store.record_fingerprint_outcome(outcome())
    assert second_revision == first_revision
    stored_again = await store.get_fingerprint_outcome(
        "track-1", "stat-1", FINGERPRINTER_VERSION
    )
    assert stored_again is not None
    assert stored_again.partial_decode is True


@pytest.mark.asyncio
async def test_partial_decode_defaults_false_for_legacy_rows(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """4.10a store part: rows written before the column existed read back
    partial_decode=False via the ADD COLUMN default."""
    await _seed_track_for_outcomes(store)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO audio_fingerprint_outcomes "
            "(id, local_track_id, stat_revision, fingerprinter_version, state, "
            "first_attempt_at, last_attempt_at, row_revision) "
            "VALUES ('outcome-legacy', 'track-1', 'stat-legacy', "
            f"'{FINGERPRINTER_VERSION}', 'failed', 1, 2, 1)"
        )
        connection.commit()
    stored = await store.get_fingerprint_outcome(
        "track-1", "stat-legacy", FINGERPRINTER_VERSION
    )
    assert stored is not None
    assert stored.partial_decode is False


@pytest.mark.asyncio
async def test_n01_below_quorum_files_soft_edition_to_confirm_review(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """2.6 (N-01/T13): a below-quorum outcome without hard conflict files the
    soft `edition_to_confirm` review state instead of hard `needs_review`,
    while the job still terminalizes `needs_review` (mapping unchanged).
    Hard-conflict `contradictory` keeps `needs_review` (pinned by C-03)."""
    await store.create_catalog_membership(_membership())
    await _finish_identified(
        store,
        db_path,
        job_id="job-n01-soft",
        attempt_id="attempt-n01-soft",
        review_id="review-n01-soft",
        rg="quorum-rg",
        release="quorum-release",
        artist_mbid=None,
        track_mbids={},
        now=10,
        outcome="insufficient_evidence",
        reason="INSUFFICIENT_METADATA",
    )
    with sqlite3.connect(db_path) as connection:
        review = connection.execute(
            "SELECT state, reason_code FROM library_identification_reviews "
            "WHERE local_album_id = 'album-1'"
        ).fetchone()
        job_state = connection.execute(
            "SELECT state FROM library_identification_jobs "
            "WHERE local_album_id = 'album-1'"
        ).fetchone()
    assert tuple(review) == ("edition_to_confirm", "INSUFFICIENT_METADATA")
    assert job_state[0] == "needs_review"


def _t31_source_context() -> MbSourceContext:
    # Matches the musicbrainz_base module defaults (official base URL,
    # generation 0): the commit fence requires a current source context.
    return MbSourceContext(source_url="https://musicbrainz.org/ws/2", generation=0)


def _t31_pinned_profile() -> PinnedLibraryManagementProfile:
    return PinnedLibraryManagementProfile(
        profile=LibraryManagementProfile(
            id="profile-1", name="Test", revision="prof-rev-1"
        ),
        naming_script=NamingScriptSettings(
            id="ns-1", name="Naming", source="$title", revision="ns-rev-1"
        ),
    )


def _t31_import_file(
    *,
    ordinal: int,
    relative_path: str,
    title: str,
    authoritative: bool,
    rg: str | None,
    release: str | None,
    recording: str | None,
    release_track: str | None,
) -> LibraryManagementImportFile:
    pinned = _t31_pinned_profile() if authoritative else None
    return LibraryManagementImportFile(
        ordinal=ordinal,
        input_path=f"/incoming/{title}.flac",
        destination_root_id="root-1",
        destination_relative_path=relative_path,
        tag=AudioTag(title=title, artist="Artist 1", album="Album 1", track_number=1),
        info=AudioInfo(
            duration_seconds=180.0,
            bitrate=800,
            sample_rate=44100,
            channels=2,
            file_format="flac",
            file_size_bytes=100,
        ),
        release_group_mbid=rg,
        release_mbid=release,
        recording_mbid=recording,
        confidence=1.0 if authoritative else 0.0,
        source="acquisition",
        authoritative_mapping=authoritative,
        release_track_mbid=release_track,
        medium_position=1 if authoritative else None,
        release_track_position=1 if authoritative else None,
        baseline_relative_path=f"Incoming/{title}.flac" if authoritative else None,
        desired_document=DesiredAudioDocument(fields=()) if authoritative else None,
        pinned_profile=pinned,
        metadata_snapshot_id="snap-1" if authoritative else None,
        projection_hash="proj-1" if authoritative else None,
        settings_revision="settings-1" if authoritative else None,
        naming_policy_revision="naming-1" if authoritative else None,
    )


def _t31_seed_bundle(
    db_path: Path, bundle: LibraryManagementImportBundle, *, with_baseline: bool
) -> None:
    request_json = msgspec.json.encode(bundle).decode()
    blob_sha = "cd" * 32
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_management_blobs "
            "(sha256, kind, byte_length, relative_path, created_at) "
            "VALUES (?, 'tag_snapshot', 10, 'blob/path', 1)",
            (blob_sha,),
        )
        connection.execute(
            "INSERT INTO library_management_metadata_snapshots "
            "(id, provider, entity_kind, entity_id, input_hash, "
            "canonical_payload_json, payload_sha256, fetched_at) "
            "VALUES ('snap-1', 'musicbrainz', 'release', 'rel-1', ?, '{}', ?, 1)",
            ("00" * 32, "01" * 32),
        )
        connection.execute(
            "INSERT INTO library_management_import_bundles "
            "(id, idempotency_key, origin, policy_revision, request_json, "
            "request_hash, state, created_at, updated_at) "
            "VALUES (?, ?, 'acquisition', 'policy-1', ?, ?, 'publishing', 1, 1)",
            (
                "bundle-t31",
                f"acquisition:{bundle.idempotency_key}",
                request_json,
                hashlib.sha256(request_json.encode()).hexdigest(),
            ),
        )
        for ordinal in range(len(bundle.files)):
            baseline = (
                (blob_sha, "flac", "v1", "stat-1", "tag-1", "[]", 200, 420)
                if with_baseline and ordinal == 0
                else (None, None, None, None, None, "[]", None, None)
            )
            connection.execute(
                "INSERT INTO library_management_import_journal "
                "(bundle_id, ordinal, state, source_fingerprint, source_size, "
                "source_mtime_ns, temporary_relative_path, destination_root_id, "
                "destination_relative_path, staged_fingerprint, "
                "baseline_blob_sha256, baseline_format, baseline_adapter_version, "
                "baseline_stat_revision, baseline_tag_revision, "
                "baseline_ancillary_snapshot_json, baseline_file_mtime_ns, "
                "baseline_file_mode, created_at, updated_at) "
                "VALUES (?, ?, 'published', ?, 100, 200, ?, 'root-1', ?, ?, "
                "?, ?, ?, ?, ?, ?, ?, ?, 1, 1)",
                (
                    "bundle-t31",
                    ordinal,
                    "ab" * 32,
                    f"temp/{ordinal}.flac",
                    f"Managed/{ordinal}.flac",
                    "ef" * 32,
                    *baseline,
                ),
            )


@pytest.mark.asyncio
async def test_f05_automatic_import_commit_preserves_manual_identities(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """F-05/T31: an automatic re-download bundle must not downgrade manual
    album/track identities - guarded rows stay byte-identical, the blocked
    overwrite files a review, and the unmanaged sibling gains no identity."""
    seed = _membership()
    await store.create_catalog_membership(seed)
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-1",
            release_group_mbid="manual-rg",
            release_mbid="manual-release",
            decision_source="manual",
            selected_by_user_id="admin",
            selected_at=5,
        ),
        expected_album_revision=1,
    )
    await store.attach_track_identity(
        LocalTrackExternalIdentity(
            local_track_id="track-1",
            recording_mbid="manual-recording",
            release_mbid="manual-release",
            release_track_mbid="manual-release-track",
            medium_position=1,
            release_track_position=1,
            decision_source="manual",
            selected_at=5,
        ),
        expected_track_revision=1,
    )
    before = _identity_snapshot(db_path)
    assert len(before["albums"]) == 1
    assert len(before["tracks"]) == 1

    auto_request = _t31_import_file(
        ordinal=0,
        relative_path="Managed/01.flac",
        title="Track 1",
        authoritative=True,
        rg="auto-rg",
        release="auto-release",
        recording="auto-recording",
        release_track="auto-release-track",
    )
    bonus_request = _t31_import_file(
        ordinal=1,
        relative_path="Managed/02-bonus.flac",
        title="Bonus",
        authoritative=False,
        rg=None,
        release=None,
        recording=None,
        release_track=None,
    )
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:t31",
        origin="acquisition",
        policy_revision="policy-1",
        files=(auto_request, bonus_request),
    )
    _t31_seed_bundle(db_path, bundle, with_baseline=True)
    bonus_track = LocalTrack(
        id="track-bonus",
        local_album_id="album-1",
        root_id="root-1",
        file_path="/music/bonus.flac",
        relative_path="bonus.flac",
        path_hash="hash-bonus",
        file_size_bytes=100,
        file_mtime_ns=200,
        stat_revision="stat-bonus",
        title="Bonus",
        artist_name="Artist 1",
        album_title="Album 1",
        album_artist_name="Artist 1",
        file_format="flac",
        imported_at=1,
    )
    track_ids = await store.commit_library_management_import_bundle(
        "bundle-t31",
        writes=[
            (
                0,
                ScannedTrackWrite(
                    artist=seed.artists[0],
                    album=seed.album,
                    track=seed.tracks[0],
                    credit=LocalArtistCredit(local_artist_id="artist-1", position=0),
                    root_id="root-1",
                    relative_path="1.flac",
                    comparison_result="new",
                    grouping_context="test",
                ),
            ),
            (
                1,
                ScannedTrackWrite(
                    artist=_artist("artist-bonus", "Bonus Artist"),
                    album=seed.album,
                    track=bonus_track,
                    credit=LocalArtistCredit(
                        local_artist_id="artist-bonus", position=0
                    ),
                    root_id="root-1",
                    relative_path="bonus.flac",
                    comparison_result="new",
                    grouping_context="test",
                ),
            ),
        ],
        replacement_track_ids={},
        recycle_track_ids={},
        requests_by_ordinal={0: auto_request, 1: bonus_request},
        automatic_requests={0: auto_request},
        expected_policy_revision="policy-1",
        result_paths_by_ordinal={0: "Managed/01.flac", 1: "Managed/02-bonus.flac"},
        updated_at=100.0,
        source_context=_t31_source_context(),
    )

    # The re-download resolved onto the curator-corrected rows (trigger case).
    assert track_ids[0] == "track-1"
    after = _identity_snapshot(db_path)
    assert after["albums"] == before["albums"]
    assert (
        after["tracks"]
        == [row for row in after["tracks"] if row[0] == "track-1"]
        == before["tracks"]
    )
    with sqlite3.connect(db_path) as connection:
        review = connection.execute(
            "SELECT state, reason_code, input_revision "
            "FROM library_identification_reviews WHERE local_album_id = 'album-1'"
        ).fetchone()
        bonus_identities = connection.execute(
            "SELECT COUNT(*) FROM local_track_external_identities "
            "WHERE local_track_id = ?",
            (track_ids[1],),
        ).fetchone()[0]
    assert tuple(review) == (
        "needs_review",
        "MANUAL_IDENTITY_STALE_IMPORT",
        "automatic-import:bundle-t31",
    )
    assert bonus_identities == 0


@pytest.mark.asyncio
async def test_f05_automatic_import_commit_without_automatic_rows_writes_no_identity(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """F-05/T31 lock: held/quarantine rows (nothing automatic in the bundle)
    commit catalog rows but never create local_*_external_identities rows."""
    seed = _membership()
    await store.create_catalog_membership(seed)
    held_request = _t31_import_file(
        ordinal=0,
        relative_path="Managed/01.flac",
        title="Track 1",
        authoritative=False,
        rg=None,
        release=None,
        recording=None,
        release_track=None,
    )
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:t31-lock",
        origin="acquisition",
        policy_revision="policy-1",
        files=(held_request,),
    )
    _t31_seed_bundle(db_path, bundle, with_baseline=False)
    track_ids = await store.commit_library_management_import_bundle(
        "bundle-t31",
        writes=[
            (
                0,
                ScannedTrackWrite(
                    artist=seed.artists[0],
                    album=seed.album,
                    track=seed.tracks[0],
                    credit=LocalArtistCredit(local_artist_id="artist-1", position=0),
                    root_id="root-1",
                    relative_path="1.flac",
                    comparison_result="new",
                    grouping_context="test",
                ),
            )
        ],
        replacement_track_ids={},
        recycle_track_ids={},
        requests_by_ordinal={0: held_request},
        automatic_requests={},
        expected_policy_revision="policy-1",
        result_paths_by_ordinal={0: "Managed/01.flac"},
        updated_at=100.0,
        source_context=_t31_source_context(),
    )

    assert track_ids == ("track-1",)
    snapshot = _identity_snapshot(db_path)
    assert snapshot["albums"] == []
    assert snapshot["tracks"] == []
    assert snapshot["artists"] == []


def test_track_provenance_columns_ratchet_idempotently_with_absent_defaults(
    tmp_path: Path,
) -> None:
    """M-06/T4 (1.4a): provenance columns exist with `absent` defaults after
    repeated construction of the same database path."""
    db_file = tmp_path / "library.db"
    _seed_auth(db_file)

    first = NativeLibraryStore(db_file, threading.Lock())
    first._ensure_tables()
    second = NativeLibraryStore(db_file, threading.Lock())
    second._ensure_tables()

    with sqlite3.connect(db_file) as connection:
        info = {
            row[1]: (row[3], row[4])
            for row in connection.execute(
                "PRAGMA table_info(local_tracks)"
            ).fetchall()
        }
    for column in (
        "title_provenance",
        "album_title_provenance",
        "album_artist_provenance",
    ):
        assert info[column][0] == 1
        assert info[column][1] == "'absent'"


@pytest.mark.asyncio
async def test_upsert_scanned_track_persists_provenance_on_insert_and_update(
    store: NativeLibraryStore,
) -> None:
    """M-06/T4 (1.4a): the scan upsert writes per-track provenance on insert
    and update, and the identification-context read carries it back."""
    artist = _artist()
    album = _membership().album
    credit = LocalArtistCredit(local_artist_id=artist.id, position=0)

    def write_with(provenance: str) -> LocalTrack:
        track = LocalTrack(
            id="track-1",
            local_album_id=album.id,
            root_id="root-1",
            file_path="/music/1.flac",
            relative_path="1.flac",
            path_hash="hash-1",
            file_size_bytes=100,
            file_mtime_ns=200,
            stat_revision="stat-1",
            title="Track 1",
            artist_name=artist.display_name,
            album_title=album.title,
            album_artist_name=artist.display_name,
            title_provenance=provenance,
            album_title_provenance=provenance,
            album_artist_provenance=provenance,
            file_format="flac",
            imported_at=1,
        )
        return track

    await store.upsert_scanned_track(
        artist=artist, album=album, track=write_with("parsed"), credit=credit
    )
    context = await store.get_album_identification_context(album.id)
    assert context is not None
    assert [track["title_provenance"] for track in context["tracks"]] == ["parsed"]
    assert [track["album_title_provenance"] for track in context["tracks"]] == [
        "parsed"
    ]
    assert [track["album_artist_provenance"] for track in context["tracks"]] == [
        "parsed"
    ]

    await store.upsert_scanned_track(
        artist=artist, album=album, track=write_with("tag"), credit=credit
    )
    context = await store.get_album_identification_context(album.id)
    assert context is not None
    assert [track["title_provenance"] for track in context["tracks"]] == ["tag"]


@pytest.mark.asyncio
async def test_scan_index_batch_persists_provenance_through_upsert_tx(
    store: NativeLibraryStore,
) -> None:
    """M-06/T4 (1.4a): the scan-index batch (`_upsert_scanned_track_tx`)
    writes provenance on insert and on re-index update."""
    await store.create_scan_run(
        ScanRun(id="scan-prov", kind="incremental", trigger="manual", queued_at=1)
    )
    artist = _artist()
    album = _membership().album
    credit = LocalArtistCredit(local_artist_id=artist.id, position=0)

    def batch_with(provenance: str) -> ScannedTrackWrite:
        return ScannedTrackWrite(
            artist=artist,
            album=album,
            track=LocalTrack(
                id="track-1",
                local_album_id=album.id,
                root_id="root-1",
                file_path="/music/1.flac",
                relative_path="1.flac",
                path_hash="hash-1",
                file_size_bytes=100,
                file_mtime_ns=200,
                stat_revision="stat-1",
                title="Track 1",
                artist_name=artist.display_name,
                album_title=album.title,
                album_artist_name=artist.display_name,
                title_provenance=provenance,
                album_title_provenance=provenance,
                album_artist_provenance=provenance,
                file_format="flac",
                imported_at=1,
            ),
            credit=credit,
            root_id="root-1",
            relative_path="1.flac",
            comparison_result="new",
            grouping_context="test",
        )

    async def provenance_rows() -> list[str]:
        context = await store.get_album_identification_context(album.id)
        assert context is not None
        assert len(context["tracks"]) == 1
        row = context["tracks"][0]
        return [
            row["title_provenance"],
            row["album_title_provenance"],
            row["album_artist_provenance"],
        ]

    await store.commit_scan_index_batch(
        "scan-prov",
        writes=[batch_with("parsed")],
        states={},
        failures=[],
        increments={},
        updated_at=2,
    )
    assert await provenance_rows() == ["parsed", "parsed", "parsed"]

    await store.commit_scan_index_batch(
        "scan-prov",
        writes=[batch_with("tag")],
        states={},
        failures=[],
        increments={},
        updated_at=3,
    )
    assert await provenance_rows() == ["tag", "tag", "tag"]


@pytest.mark.asyncio
async def test_catalog_membership_seed_defaults_provenance_to_absent(
    store: NativeLibraryStore,
) -> None:
    """M-06/T4 (1.4a): rows written without explicit provenance read back as
    `absent` - no behavior change for producers that do not set the fields."""
    await store.create_catalog_membership(_membership())
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    assert len(context["tracks"]) == 1
    row = context["tracks"][0]
    assert row["title_provenance"] == "absent"
    assert row["album_title_provenance"] == "absent"
    assert row["album_artist_provenance"] == "absent"


@pytest.mark.asyncio
async def test_locked_rescan_with_failed_tag_read_keeps_displays_and_provenance(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """R2 (LibraryFindings-All loop-1): under catalog lock a rescan whose tag
    read failed/deferred keeps the lock-preserved tag displays paired with
    their provenance on both tag-update paths; the unlocked control still
    refreshes both."""
    artist = _artist()
    membership = _membership()
    seed = membership.tracks[0]
    seed.title_provenance = "tag"
    seed.album_title_provenance = "tag"
    seed.album_artist_provenance = "tag"
    await store.create_catalog_membership(membership)
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-1",
            release_group_mbid="rg-1",
            release_mbid="release-1",
            selected_at=2,
        ),
        expected_album_revision=1,
    )
    await store.attach_track_identity(
        LocalTrackExternalIdentity(
            local_track_id="track-1",
            recording_mbid="recording-1",
            release_mbid="release-1",
            release_track_mbid="release-track-1",
            selected_at=2,
        ),
        expected_track_revision=1,
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET membership_locked = 1 WHERE id = 'track-1'"
        )
        connection.commit()

    guarded = (
        "title",
        "title_folded",
        "artist_name",
        "artist_name_folded",
        "album_title",
        "album_title_folded",
        "album_artist_name",
        "album_artist_name_folded",
        "title_provenance",
        "album_title_provenance",
        "album_artist_provenance",
    )

    def guarded_snapshot(track_id: str) -> tuple:
        with sqlite3.connect(db_path) as connection:
            connection.row_factory = sqlite3.Row
            row = connection.execute(
                "SELECT * FROM local_tracks WHERE id = ?", (track_id,)
            ).fetchone()
            assert row is not None
            return tuple(row[column] for column in guarded)

    def failed_tag_read() -> LocalTrack:
        return LocalTrack(
            id="track-1",
            local_album_id="album-1",
            root_id="root-1",
            file_path="/music/1.flac",
            relative_path="1.flac",
            path_hash="hash-1",
            file_size_bytes=100,
            file_mtime_ns=200,
            stat_revision="stat-2",
            tag_revision="tag-2",
            title="1",
            artist_name="placeholder-artist",
            album_title="placeholder-album",
            album_artist_name="placeholder-album-artist",
            title_provenance="absent",
            album_title_provenance="absent",
            album_artist_provenance="absent",
            metadata_incomplete=True,
            file_format="flac",
            imported_at=1,
        )

    before = guarded_snapshot("track-1")
    assert before[8:] == ("tag", "tag", "tag")

    await store.upsert_scanned_track(
        artist=artist,
        album=membership.album,
        track=failed_tag_read(),
        credit=LocalArtistCredit(local_artist_id=artist.id, position=0),
    )
    assert guarded_snapshot("track-1") == before

    await store.create_scan_run(
        ScanRun(id="scan-r2", kind="incremental", trigger="manual", queued_at=1)
    )
    await store.commit_scan_index_batch(
        "scan-r2",
        writes=[
            ScannedTrackWrite(
                artist=artist,
                album=membership.album,
                track=failed_tag_read(),
                credit=LocalArtistCredit(local_artist_id=artist.id, position=0),
                root_id="root-1",
                relative_path="1.flac",
                comparison_result="changed",
                grouping_context="test",
            )
        ],
        states={},
        failures=[],
        increments={},
        updated_at=2,
    )
    assert guarded_snapshot("track-1") == before

    control = _membership("2")
    control.tracks[0].title_provenance = "tag"
    control.tracks[0].album_title_provenance = "tag"
    control.tracks[0].album_artist_provenance = "tag"
    await store.create_catalog_membership(control)
    await store.upsert_scanned_track(
        artist=_artist("artist-2", "Artist 2"),
        album=control.album,
        track=LocalTrack(
            id="track-2",
            local_album_id="album-2",
            root_id="root-1",
            file_path="/music/2.flac",
            relative_path="2.flac",
            path_hash="hash-2",
            file_size_bytes=100,
            file_mtime_ns=200,
            stat_revision="stat-2",
            tag_revision="tag-2",
            title="Refreshed Title",
            artist_name="Refreshed Artist",
            album_title="Refreshed Album",
            album_artist_name="Refreshed Album Artist",
            title_provenance="parsed",
            album_title_provenance="parsed",
            album_artist_provenance="parsed",
            file_format="flac",
            imported_at=1,
        ),
        credit=LocalArtistCredit(local_artist_id="artist-2", position=0),
    )
    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        refreshed = connection.execute(
            "SELECT * FROM local_tracks WHERE id = 'track-2'"
        ).fetchone()
        assert refreshed is not None
        assert refreshed["title"] == "Refreshed Title"
        assert refreshed["album_title"] == "Refreshed Album"
        assert refreshed["album_artist_name"] == "Refreshed Album Artist"
        assert refreshed["title_provenance"] == "parsed"
        assert refreshed["album_title_provenance"] == "parsed"
        assert refreshed["album_artist_provenance"] == "parsed"
