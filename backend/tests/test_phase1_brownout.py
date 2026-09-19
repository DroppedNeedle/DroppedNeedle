"""Phase 1 boot gate, F-NL-03 cutover edition: the supported target entrypoint
boots with no legacy scanner surface, the only Lidarr route paths are the
read-only migration importer, and the download-client settings route is mounted."""

from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from core.dependencies.service_providers import (
    get_library_repository,
    get_local_files_service,
)
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.local_catalog import (
    CatalogMembership,
    LocalAlbum,
    LocalArtist,
    LocalArtistCredit,
    LocalTrack,
)
from services.native.target_library_repository import TargetLibraryRepository
from target_main import app
from tests.helpers import openapi_method_paths


# TestClient without `with` skips lifespan startup (no background tasks needed for these checks).
client = TestClient(app)


def test_library_providers_are_target_not_stub():
    """LocalFilesService and library repository are DI-wired to TargetLibraryRepository,
    not a silent stub; populated target data is observable via the repository."""
    repo = get_library_repository()
    assert isinstance(repo, TargetLibraryRepository)
    local_files = get_local_files_service()
    assert isinstance(local_files._library_repo, TargetLibraryRepository)


@pytest.mark.asyncio
async def test_seeded_target_catalog_is_observable_via_target_repository(tmp_path: Path) -> None:
    album_id = "10000000-0000-4000-8000-000000000001"
    track_id = "20000000-0000-4000-8000-000000000001"
    artist_id = "30000000-0000-4000-8000-000000000001"
    artist = LocalArtist(
        id=artist_id,
        display_name="Brownout Artist",
        folded_name="brownout artist",
        kind="person",
        created_at=1,
        updated_at=1,
    )
    album = LocalAlbum(
        id=album_id,
        root_id="root-1",
        grouping_key="group:brownout",
        title="Brownout Album",
        album_artist_id=artist_id,
        album_artist_name=artist.display_name,
        created_at=1,
        updated_at=1,
    )
    path = tmp_path / "brownout.flac"
    path.write_bytes(b"fLaC" + b"\0" * 64)
    track = LocalTrack(
        id=track_id,
        local_album_id=album_id,
        root_id="root-1",
        file_path=str(path),
        relative_path=path.name,
        path_hash=f"hash:{track_id}",
        file_size_bytes=path.stat().st_size,
        file_mtime_ns=path.stat().st_mtime_ns,
        stat_revision=f"stat:{track_id}",
        title="Brownout Track",
        artist_name=artist.display_name,
        album_title=album.title,
        album_artist_name=artist.display_name,
        duration_seconds=180,
        file_format="flac",
        imported_at=2,
    )
    credit = LocalArtistCredit(local_artist_id=artist_id, position=0)
    store = NativeLibraryStore(tmp_path / "brownout.db", __import__("threading").Lock())
    await store.create_catalog_membership(
        CatalogMembership(
            album=album,
            artists=[artist],
            tracks=[track],
            album_credits=[credit],
            track_credits={track_id: [credit]},
        )
    )
    repo = TargetLibraryRepository(store)

    stats = await repo.get_stats()
    assert stats.total_albums == 1
    tracks = await repo.get_tracks(album_id)
    assert len(tracks) == 1
    assert tracks[0].id == track_id
    with pytest.raises(NotImplementedError):
        await repo.delete_album(123)
    with pytest.raises(NotImplementedError):
        await repo.delete_artist(456)


def test_app_boots_and_health_ok():
    resp = client.get("/health")
    assert resp.status_code == 200
    assert resp.json()["status"] == "ok"


def test_only_sanctioned_lidarr_routes_mounted():
    """The old Lidarr *management* integration stays deleted (LidarrImport D8). The only
    permitted Lidarr route paths are the read-only migration importer under
    ``/lidarr-import`` - any other ``lidarr`` path would mean the management surface came back."""
    lidarr_paths = [
        path for _, path in openapi_method_paths(app) if "lidarr" in path
    ]
    assert lidarr_paths, "expected the lidarr-import routes to be mounted"
    assert all("/lidarr-import" in path for path in lidarr_paths), lidarr_paths


def test_download_client_settings_route_mounted():
    # Phase 6 relocated the download-client config from the P1 brownout stub at
    # /settings/download-client to its canonical home at /download-client/config.
    paths = [path for _, path in openapi_method_paths(app)]
    assert any(path.endswith("/download-client/config") for path in paths)


def test_legacy_scanner_surface_is_gone():
    """F-NL-03: the old /library/scan/* surface must be ABSENT from the supported
    entrypoint - an absence check, not a compatibility response assertion."""
    paths = [path for _, path in openapi_method_paths(app)]
    # Positive control: this is a pure absence check, so pin the lens itself -
    # a blinded (empty) collection must fail loudly instead of passing vacuously.
    assert paths, "expected a populated route surface"
    assert any(path.endswith("/download-client/config") for path in paths)
    for legacy in (
        "/library/scan/start",
        "/library/scan/cancel",
        "/library/scan/status",
        "/library/scan/stream",
        "/library/scan/unmatched",
    ):
        assert legacy not in paths, legacy
