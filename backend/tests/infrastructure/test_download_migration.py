"""Phase-0 migration gates (D8/D9/D14): the in-flight manifest back-fill and the
quarantine-table rebuild must survive an upgrade so a mid-flight download isn't
broken and a previously-blocklisted source stays blocklisted.

These are the load-bearing "invisible refactor" guarantees: msgspec drops unknown
fields on decode (``AppStruct`` has no ``forbid_unknown_fields``), so a legacy
manifest decodes WITHOUT error and ``__post_init__`` synthesises the soulseek
``TaskHandle`` it lacks.
"""

import sqlite3
import threading
from pathlib import Path

import msgspec
import pytest

from models.download_identity import soulseek_identity
from models.download_manifest import DownloadManifest, ManifestCodec
from repositories.protocols.download_client import TaskHandle

_LEGACY_MANIFEST = (
    b'{"task_id":"t1","source_username":"alice","release_group_mbid":"rg",'
    b'"artist_name":"Radiohead","album_title":"OK Computer","naming_template":"X",'
    b'"target_files":[{"filename":"01.flac","size":10},{"filename":"02.flac","size":20}],'
    b'"is_track":false}'
)


def test_legacy_manifest_decodes_and_backfills_soulseek_handle():
    # A required `handle` would have raised here; both fields stay optional so the
    # legacy blob decodes, then __post_init__ reconstructs the handle.
    manifest = ManifestCodec().decode(_LEGACY_MANIFEST)
    assert manifest.handle is not None
    assert manifest.handle.source == "soulseek"
    assert manifest.handle.username == "alice"
    assert manifest.handle.filenames == ["01.flac", "02.flac"]
    # The legacy correlation field is preserved (rollback-safe).
    assert manifest.source_username == "alice"


def test_legacy_manifest_with_unknown_field_does_not_raise():
    blob = (
        b'{"task_id":"t1","source_username":"bob","release_group_mbid":"rg",'
        b'"artist_name":"A","album_title":"B","naming_template":"X","target_files":[],'
        b'"obsolete_field":123}'
    )
    manifest = msgspec.json.decode(blob, type=DownloadManifest)
    assert manifest.handle.username == "bob"


def test_new_manifest_with_explicit_handle_is_not_clobbered():
    # A Usenet manifest carries an explicit handle and no source_username;
    # __post_init__ must leave it untouched.
    manifest = DownloadManifest(
        task_id="t2",
        release_group_mbid="rg",
        artist_name="A",
        album_title="B",
        naming_template="X",
        target_files=[],
        handle=TaskHandle(source="usenet", job_name="droppedneedle-t2", nzo_id="SAB_1"),
    )
    encoded = ManifestCodec().encode(manifest)
    decoded = ManifestCodec().decode(encoded)
    assert decoded.handle.source == "usenet"
    assert decoded.handle.job_name == "droppedneedle-t2"
    assert decoded.handle.nzo_id == "SAB_1"
    assert decoded.handle.external_id == "SAB_1"
    assert decoded.handle.correlation_id == "droppedneedle-t2"
    assert decoded.source_username is None


def _seed_legacy_quarantine(db_path: Path) -> None:
    conn = sqlite3.connect(db_path)
    try:
        conn.executescript(
            """
            CREATE TABLE auth_users (id TEXT PRIMARY KEY, username TEXT, role TEXT);
            CREATE TABLE download_quarantine (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                client_id TEXT NOT NULL DEFAULT 'unknown',
                username TEXT NOT NULL,
                filename TEXT NOT NULL,
                release_group_mbid TEXT,
                reason TEXT NOT NULL,
                quarantined_at REAL NOT NULL,
                UNIQUE (client_id, username, filename, release_group_mbid)
            );
            INSERT INTO download_quarantine
                (client_id, username, filename, release_group_mbid, reason, quarantined_at)
            VALUES ('slskd','peerX','bad.flac','rg','verify_failed',1.0);
            """
        )
        conn.commit()
    finally:
        conn.close()


@pytest.mark.asyncio
async def test_quarantine_rebuild_preserves_blocklist(tmp_path: Path):
    from infrastructure.persistence.download_store import DownloadStore

    db_path = tmp_path / "library.db"
    _seed_legacy_quarantine(db_path)
    # Constructing the store runs _ensure_tables -> _migrate_quarantine (rebuild).
    store = DownloadStore(db_path=db_path, write_lock=threading.Lock())

    quarantine = await store.load_quarantine_set()
    # The old (username, filename) row is preserved as the soulseek identity, so a
    # source blocklisted before the upgrade is still excluded after it.
    assert ("soulseek", soulseek_identity("peerX", "bad.flac")) in quarantine

    # The rebuilt CHECK accepts the new download_failed reason (D11) + the usenet key.
    from models.download_identity import usenet_identity

    ident = usenet_identity("Album [FLAC]", 350 * 1024 * 1024)
    await store.record_quarantine(
        source="usenet", identity=ident, reason="download_failed", release_group_mbid="rg2"
    )
    assert ("usenet", ident) in await store.load_quarantine_set()

    # The rebuilt table keeps its secondary indexes (the rename->recreate must not lose
    # them to a name collision with the renamed-aside legacy indexes).
    conn = sqlite3.connect(db_path)
    try:
        idx = {r[0] for r in conn.execute(
            "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='download_quarantine'"
        )}
    finally:
        conn.close()
    assert "idx_quarantine_lookup" in idx
    assert "idx_quarantine_quarantined_at" in idx


def test_quarantine_admin_projection_round_trips_soulseek_identity(tmp_path: Path):
    """The admin list keeps the legacy (client_id/username/filename) shape so the
    existing admin UI works post-rebuild."""
    import asyncio

    from infrastructure.persistence.download_store import DownloadStore

    db_path = tmp_path / "library.db"
    _seed_legacy_quarantine(db_path)
    store = DownloadStore(db_path=db_path, write_lock=threading.Lock())

    rows = asyncio.run(store.list_quarantine())
    assert rows[0]["username"] == "peerX"
    assert rows[0]["filename"] == "bad.flac"
    assert rows[0]["client_id"] == "soulseek"
    assert rows[0]["source"] == "soulseek"
