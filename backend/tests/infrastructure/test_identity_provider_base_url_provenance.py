"""P2 full-mirror provenance stamping (owner decision 2026-08-24).

The additive ``provider_source_*`` columns record the opaque MusicBrainz source
identity for an accepted AUTOMATIC identity. The legacy ``provider_base_url``
column remains NULL so endpoint details are never persisted. These tests pin:
the ratchet is idempotent (construct-twice), automatic rows keep endpoint data
out of storage, and manual/legacy rows stay NULL. No read path or identity
predicate consumes the columns."""

import sqlite3
import threading
from pathlib import Path

import pytest

from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.local_catalog import LocalAlbumExternalIdentity
from infrastructure.http.brainzmash_transport import BRAINZMASH_ENDPOINT
from repositories.musicbrainz_base import (
    brainzmash_runtime_enabled,
    capture_mb_source_context,
    clear_mb_response_context,
    get_mb_source_id,
    set_mb_api_base,
)


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


@pytest.fixture
def mirror_base():
    """Point the MB base at a mirror for capture-time stamping; restore after."""
    original = capture_mb_source_context()
    original_source_id = get_mb_source_id()
    original_runtime = brainzmash_runtime_enabled()
    set_mb_api_base(
        "https://mirror.example.com/ws/2",
        source_mode="mirror",
        source_id="mirror-provenance",
        generation=original.generation + 1,
    )
    yield "https://mirror.example.com/ws/2"
    set_mb_api_base(
        original.source_url,
        source_mode=original.source_mode,
        source_id=original_source_id,
        generation=original.generation,
        brainzmash_binding_valid=original_runtime,
    )


def _seed_album(store: NativeLibraryStore, album_id: str) -> None:
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "INSERT OR IGNORE INTO local_artists (id, display_name, folded_name, kind, "
            "created_at, updated_at) VALUES ('artist-1', 'Artist', 'artist', 'group', 1, 1)"
        )
        connection.execute(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, "
            "album_artist_id, grouping_source, created_at, updated_at) "
            "VALUES (?, 'root', 'key', 'Album', 'album', 'artist-1', 'automatic', 1, 1)",
            (album_id,),
        )


def _album_identity_row(store: NativeLibraryStore, album_id: str):
    with sqlite3.connect(store.db_path) as connection:
        connection.row_factory = sqlite3.Row
        return connection.execute(
            "SELECT release_group_mbid, provider_base_url FROM "
            "local_album_external_identities WHERE local_album_id = ?",
            (album_id,),
        ).fetchone()


class TestProviderBaseUrlRatchet:
    def test_construct_twice_is_idempotent(self, db_path: Path):
        first = NativeLibraryStore(db_path, threading.Lock())
        second = NativeLibraryStore(db_path, threading.Lock())
        assert first is not None and second is not None

        with sqlite3.connect(second.db_path) as connection:
            connection.row_factory = sqlite3.Row

            def column_names(table: str) -> list[str]:
                return [
                    row["name"]
                    for row in connection.execute(f"PRAGMA table_info({table})")
                ]

        assert (
            column_names("local_album_external_identities").count("provider_base_url")
            == 1
        )
        assert (
            column_names("local_track_external_identities").count("provider_base_url")
            == 1
        )


class TestAutomaticProvenanceStamping:
    @pytest.mark.asyncio
    async def test_automatic_attach_does_not_persist_serving_base_url(
        self, store: NativeLibraryStore, mirror_base: str
    ):
        _seed_album(store, "alb-1")
        await store.attach_album_identity(
            LocalAlbumExternalIdentity(
                local_album_id="alb-1",
                release_group_mbid="rg-1",
                decision_source="automatic",
                matcher_version="matcher-v1",
                selected_at=10.0,
            ),
            expected_album_revision=1,
        )

        row = _album_identity_row(store, "alb-1")

        assert row["provider_base_url"] is None
        assert row["release_group_mbid"] == "rg-1"

    @pytest.mark.asyncio
    @pytest.mark.parametrize("source", ["manual", "legacy_import", "embedded"])
    async def test_non_automatic_paths_stay_null(
        self, store: NativeLibraryStore, source: str
    ):
        _seed_album(store, f"alb-{source}")
        await store.attach_album_identity(
            LocalAlbumExternalIdentity(
                local_album_id=f"alb-{source}",
                release_group_mbid="rg-x",
                decision_source=source,
                selected_at=10.0,
            ),
            expected_album_revision=1,
        )

        assert _album_identity_row(store, f"alb-{source}")["provider_base_url"] is None

    @pytest.mark.asyncio
    async def test_automatic_update_keeps_endpoint_unpersisted(
        self, store: NativeLibraryStore, mirror_base: str
    ):
        _seed_album(store, "alb-upd")
        await store.attach_album_identity(
            LocalAlbumExternalIdentity(
                local_album_id="alb-upd",
                release_group_mbid="rg-old",
                decision_source="manual",
                selected_at=10.0,
            ),
            expected_album_revision=1,
        )

        # a later automatic pass through a different endpoint restamps
        mirror_context = capture_mb_source_context()
        mirror_source_id = get_mb_source_id()
        set_mb_api_base(
            "https://other-mirror.example.com/ws/2",
            source_mode="mirror",
            source_id="other-mirror-provenance",
            generation=mirror_context.generation + 1,
        )
        try:
            with sqlite3.connect(store.db_path) as connection:
                current_revision = connection.execute(
                    "SELECT row_revision FROM local_albums WHERE id = 'alb-upd'"
                ).fetchone()[0]
            await store.attach_album_identity(
                LocalAlbumExternalIdentity(
                    local_album_id="alb-upd",
                    release_group_mbid="rg-new",
                    decision_source="automatic",
                    selected_at=20.0,
                ),
                expected_album_revision=current_revision,
            )
        finally:
            set_mb_api_base(
                mirror_context.source_url,
                source_mode=mirror_context.source_mode,
                source_id=mirror_source_id,
                generation=mirror_context.generation,
            )

        row = _album_identity_row(store, "alb-upd")

        assert row["release_group_mbid"] == "rg-new"
        assert row["provider_base_url"] is None

    @pytest.mark.asyncio
    async def test_brainzmash_automatic_identity_stores_opaque_provenance(
        self, store: NativeLibraryStore
    ):
        original = capture_mb_source_context()
        original_source_id = get_mb_source_id()
        original_runtime = brainzmash_runtime_enabled()
        clear_mb_response_context()
        brainzmash_generation = original.generation + 1
        set_mb_api_base(
            BRAINZMASH_ENDPOINT.rstrip("/"),
            source_mode="brainzmash",
            source_id="brainzmash-provenance",
            generation=brainzmash_generation,
        )
        capture_mb_source_context()
        try:
            _seed_album(store, "alb-brainzmash")
            await store.attach_album_identity(
                LocalAlbumExternalIdentity(
                    local_album_id="alb-brainzmash",
                    release_group_mbid="rg-brainz",
                    decision_source="automatic",
                    selected_at=30.0,
                    provider_source_mode="brainzmash",
                    provider_source_id="brainzmash-provenance",
                    provider_source_generation=brainzmash_generation,
                ),
                expected_album_revision=1,
            )
            with sqlite3.connect(store.db_path) as connection:
                row = connection.execute(
                    "SELECT provider_base_url, provider_source_mode, "
                    "provider_source_id, provider_source_generation "
                    "FROM local_album_external_identities "
                    "WHERE local_album_id = 'alb-brainzmash'"
                ).fetchone()
        finally:
            set_mb_api_base(
                original.source_url,
                source_mode=original.source_mode,
                source_id=original_source_id,
                generation=original.generation,
                brainzmash_binding_valid=original_runtime,
            )
            clear_mb_response_context()

        assert row == (
            None,
            "brainzmash",
            "brainzmash-provenance",
            brainzmash_generation,
        )
