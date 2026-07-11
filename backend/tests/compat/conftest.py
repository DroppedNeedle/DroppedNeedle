"""Shared fixtures for the Connect Apps (compat) test suite.

Builds temp stores over a single shared WAL db file (mirroring production: auth,
library and compat tables co-locate) so FKs resolve. Grown per-milestone.
"""

from __future__ import annotations

import hashlib
import threading
from pathlib import Path

import pytest
import pytest_asyncio

from infrastructure.crypto import init_crypto
from infrastructure.persistence.app_password_store import AppPasswordStore
from infrastructure.persistence.auth_store import AuthStore
from infrastructure.persistence.favorites_store import FavoritesStore


@pytest.fixture(autouse=True)
def _crypto(tmp_path: Path) -> None:
    init_crypto(tmp_path / "config")


@pytest.fixture
def db_path(tmp_path: Path) -> Path:
    return tmp_path / "library.db"


@pytest.fixture
def write_lock() -> threading.Lock:
    return threading.Lock()


@pytest_asyncio.fixture
async def auth_store(db_path: Path, write_lock: threading.Lock) -> AuthStore:
    store = AuthStore(db_path=db_path, write_lock=write_lock)
    await store.create_user(
        id="user-alice", display_name="Alice", role="user", username="alice"
    )
    await store.create_user(
        id="user-bob", display_name="Bob", role="user", username="bob"
    )
    return store


@pytest.fixture
def app_password_store(db_path: Path, write_lock: threading.Lock) -> AppPasswordStore:
    return AppPasswordStore(db_path=db_path, write_lock=write_lock)


@pytest_asyncio.fixture
async def app_password_service(
    app_password_store: AppPasswordStore, auth_store: AuthStore
) -> "AppPasswordService":
    from services.compat.app_password_service import AppPasswordService

    return AppPasswordService(app_password_store, auth_store)


@pytest.fixture
def favorites_store(db_path: Path, write_lock: threading.Lock) -> FavoritesStore:
    return FavoritesStore(db_path=db_path, write_lock=write_lock)


@pytest_asyncio.fixture
async def favorites_service(
    favorites_store: FavoritesStore, auth_store: AuthStore
) -> "FavoritesService":
    from services.compat.favorites_service import FavoritesService

    return FavoritesService(favorites_store)


@pytest.fixture
def compat_id_map_service(
    db_path: Path, write_lock: threading.Lock
) -> "CompatIdMapService":
    from infrastructure.persistence.compat_id_map_store import CompatIdMapStore
    from services.compat.id_map_service import CompatIdMapService

    return CompatIdMapService(
        CompatIdMapStore(db_path=db_path, write_lock=write_lock)
    )


_FIXTURE_FLAC = (
    Path(__file__).parent.parent / "fixtures" / "library" / "flac_full_01.flac"
)


@pytest_asyncio.fixture
async def streaming_env(tmp_path):
    """A real-file env (FLAC on disk) with BOTH compat shims + GZipMiddleware (so
    GZip-vs-Range is exercised) over real LocalFilesService/id_map. For
    direct-play + transcode-wiring + Jellyfin stream tests."""
    import shutil
    from types import SimpleNamespace
    from unittest.mock import Mock

    from fastapi import FastAPI
    from starlette.middleware.gzip import GZipMiddleware

    from api.compat.jellyfin.router import router as jellyfin_router
    from api.compat.subsonic.router import router as subsonic_router
    from api.v1.schemas.settings import ConnectAppsSettings, LibrarySettings
    from core import dependencies as deps
    from core.config import Settings
    from infrastructure.persistence.app_password_store import AppPasswordStore
    from infrastructure.persistence.auth_store import AuthStore
    from infrastructure.persistence.compat_id_map_store import CompatIdMapStore
    from infrastructure.persistence.favorites_store import FavoritesStore
    from infrastructure.persistence.library_db import LibraryDB
    from models.audio import AudioInfo, AudioTag
    from services.compat.app_password_service import AppPasswordService
    from services.compat.favorites_service import FavoritesService
    from services.compat.id_map_service import CompatIdMapService
    from services.compat.library_view_service import LibraryViewService
    from services.local_files_service import LocalFilesService
    from services.native.library_manager import LibraryManager
    from services.preferences_service import PreferencesService
    from tests.helpers import build_test_client

    lock = threading.Lock()
    db_path = tmp_path / "library.db"
    music = tmp_path / "music"
    music.mkdir()
    dst = music / "flac_full_01.flac"
    shutil.copy(_FIXTURE_FLAC, dst)

    auth = AuthStore(db_path=db_path, write_lock=lock)
    await auth.create_user(
        id="user-alice", display_name="Alice", role="user", username="alice"
    )
    db = LibraryDB(db_path=db_path, write_lock=lock)
    lm = LibraryManager(db)
    fid = await lm.upsert_file(
        dst,
        AudioTag(title="Airbag", artist="Radiohead", album="OK Computer",
                 track_number=1, album_artist="Radiohead", year=1997),
        AudioInfo(duration_seconds=0.3, bitrate=900, sample_rate=44100, channels=2,
                  file_format="flac", file_size_bytes=dst.stat().st_size, bit_depth=16),
        release_group_mbid="b1392450-e666-3926-a536-22c65f834433",
        recording_mbid="rec-1", file_mtime=dst.stat().st_mtime,
    )

    settings = Settings()
    settings.config_file_path = tmp_path / "cfg.json"
    prefs = PreferencesService(settings)
    prefs.save_connect_apps_settings(
        ConnectAppsSettings(subsonic_enabled=True, jellyfin_enabled=True)
    )
    prefs.save_library_settings(LibrarySettings(library_paths=[str(music)]))

    class _Cover:
        async def get_release_group_cover_etag(self, *a, **k):
            return None

    cover = _Cover()
    favorites = FavoritesService(FavoritesStore(db_path=db_path, write_lock=lock))
    view = LibraryViewService(lm, db, cover, favorites)
    id_map = CompatIdMapService(CompatIdMapStore(db_path=db_path, write_lock=lock))
    local_files = LocalFilesService(lm, prefs, Mock())
    app_pw = AppPasswordService(
        AppPasswordStore(db_path=db_path, write_lock=lock), auth
    )
    _record, secret = await app_pw.create("user-alice", "stream client")

    app = FastAPI()
    app.add_middleware(GZipMiddleware, minimum_size=500)  # replicate prod GZip
    app.include_router(subsonic_router)
    app.include_router(jellyfin_router)
    app.dependency_overrides.update({
        deps.get_app_password_service: lambda: app_pw,
        deps.get_local_files_service: lambda: local_files,
        deps.get_preferences_service: lambda: prefs,
        deps.get_library_view_service: lambda: view,
        deps.get_compat_id_map_service: lambda: id_map,
        deps.get_favorites_service: lambda: favorites,
        deps.get_playlist_service: lambda: Mock(),
        deps.get_compat_scrobble_adapter: lambda: Mock(),
        deps.get_compat_discover_service: lambda: Mock(),
        deps.get_coverart_repository: lambda: cover,
        deps.get_now_playing_service: lambda: Mock(),
    })
    client = build_test_client(app)
    jf_track_id = await id_map.to_jf("track", fid)
    return SimpleNamespace(
        client=client, secret=secret, track_id="tr-" + fid, file_id=fid,
        jf_track_id=jf_track_id, raw=dst.read_bytes(), app=app, prefs=prefs,
        id_map=id_map,
    )


class _FakeCoverArt:
    """Minimal CoverArtRepository stand-in - LibraryViewService only calls the
    cheap etag path; return None (no cover) for deterministic tests."""

    async def get_release_group_cover_etag(self, release_group_id, size="500"):
        return None


@pytest_asyncio.fixture
async def seeded_library(
    db_path: Path, write_lock: threading.Lock, auth_store: AuthStore
):
    """A tiny native library: one Radiohead album (MBID-less artist -> synth) with
    two tracks. Returns (library_db, library_manager, ids)."""
    from models.audio import AudioInfo, AudioTag
    from infrastructure.persistence.library_db import LibraryDB
    from services.native.library_manager import LibraryManager

    db = LibraryDB(db_path=db_path, write_lock=write_lock)
    lm = LibraryManager(db)
    rg = "b1392450-e666-3926-a536-22c65f834433"
    ids = {"rg": rg, "tracks": []}
    for title, trackno in (("Airbag", 1), ("Paranoid Android", 2)):
        fid = await lm.upsert_file(
            Path(f"/music/{title}.flac"),
            AudioTag(
                title=title, artist="Radiohead", album="OK Computer",
                track_number=trackno, album_artist="Radiohead", year=1997,
                genre="Alternative Rock",
            ),
            AudioInfo(
                duration_seconds=200.0 + trackno, bitrate=900, sample_rate=44100,
                channels=2, file_format="flac", file_size_bytes=1000, bit_depth=16,
            ),
            release_group_mbid=rg, recording_mbid=f"rec-{trackno}", file_mtime=1.0,
        )
        ids["tracks"].append(fid)
    return db, lm, ids


@pytest_asyncio.fixture
async def library_view_service(seeded_library, favorites_service):
    from services.compat.library_view_service import LibraryViewService

    db, lm, _ids = seeded_library
    return LibraryViewService(lm, db, _FakeCoverArt(), favorites_service)


class _JpegCoverArt:
    """CoverArtRepository stand-in that returns a tiny JPEG for getCoverArt."""

    JPEG = b"\xff\xd8\xff\xe0fake-jpeg\xff\xd9"

    async def get_release_group_cover_etag(self, release_group_id, size="500"):
        return None

    async def get_release_group_cover(self, release_group_id, size="500", **kw):
        return (self.JPEG, "image/jpeg", "fake")

    async def get_artist_image(self, artist_id, size=None, **kw):
        return (self.JPEG, "image/jpeg", "fake")


def subsonic_query(secret, username, *, fmt="json", scheme="apikey", client="pytest"):
    """Query params a Subsonic client would send. scheme: apikey | token | enc."""
    base = {"v": "1.16.1", "c": client, "f": fmt}
    if scheme == "apikey":
        return {**base, "apiKey": secret}
    if scheme == "enc":
        return {**base, "u": username, "p": "enc:" + secret.encode().hex()}
    # token scheme
    salt = "abcd1234"
    token = hashlib.md5((secret + salt).encode()).hexdigest()
    return {**base, "u": username, "t": token, "s": salt}


@pytest_asyncio.fixture
async def compat_env(
    db_path, write_lock, auth_store, seeded_library, app_password_service,
    favorites_service, compat_id_map_service, tmp_path,
):
    """A FastAPI test client with the compat shims mounted and the DI bundle
    overridden to temp services over the seeded (fake-path) library. Both
    protocols enabled. Returns an object with .client/.secret/.username/.ids.

    For browse/search/cover - streaming tests build their own real-file env."""
    from types import SimpleNamespace
    from unittest.mock import AsyncMock, Mock

    from fastapi import FastAPI

    from api.compat.subsonic.router import router as subsonic_router
    from api.compat.jellyfin.router import router as jellyfin_router
    from core.config import Settings
    from core import dependencies as deps
    from infrastructure.persistence.library_db import LibraryDB
    from infrastructure.persistence.play_history_store import PlayHistoryStore
    from repositories.playlist_repository import PlaylistRepository
    from services.compat.compat_scrobble_adapter import CompatScrobbleAdapter
    from services.compat.discover_service import CompatDiscoverService
    from services.compat.library_view_service import LibraryViewService
    from services.playlist_service import PlaylistService
    from services.preferences_service import PreferencesService
    from services.scrobble_service import ScrobbleService
    from tests.helpers import build_test_client
    from api.v1.schemas.settings import ConnectAppsSettings

    db, _lm, ids = seeded_library
    cover = _JpegCoverArt()

    prefs_settings = Settings()
    prefs_settings.config_file_path = tmp_path / "compat-config.json"
    preferences = PreferencesService(prefs_settings)
    preferences.save_connect_apps_settings(
        ConnectAppsSettings(subsonic_enabled=True, jellyfin_enabled=True)
    )

    view = LibraryViewService(_lm, db, cover, favorites_service)
    playlists = PlaylistService(
        repo=PlaylistRepository(db_path=db_path, write_lock=write_lock),
        cache_dir=tmp_path, auth_store=auth_store, library_db=db,
    )
    phs = PlayHistoryStore(db_path=db_path, write_lock=write_lock)
    listening_prefs = AsyncMock()
    listening_prefs.get = AsyncMock(return_value=SimpleNamespace(
        scrobble_to_lastfm=False, scrobble_to_listenbrainz=False))
    scrobble_service = ScrobbleService(
        client_factory=AsyncMock(), listening_prefs_store=listening_prefs,
        play_history_store=phs,
    )
    from infrastructure.sse_publisher import SSEPublisher
    from services.now_playing_service import NowPlayingService

    np_prefs = AsyncMock()
    np_prefs.get = AsyncMock(
        return_value=SimpleNamespace(now_playing_visibility="full")
    )
    now_playing = NowPlayingService(SSEPublisher(), np_prefs)
    scrobble = CompatScrobbleAdapter(scrobble_service, view, now_playing)
    discover = CompatDiscoverService(
        library_db=db, library_view_service=view, preferences_service=preferences,
        play_history_store=phs,
    )

    record, secret = await app_password_service.create("user-alice", "pytest client")

    app = FastAPI()
    app.include_router(subsonic_router)
    app.include_router(jellyfin_router)
    from api.compat.common.path_case import CompatPathCaseMiddleware

    app.add_middleware(
        CompatPathCaseMiddleware,
        routes=[*subsonic_router.routes, *jellyfin_router.routes],
    )
    overrides = {
        deps.get_app_password_service: lambda: app_password_service,
        deps.get_library_view_service: lambda: view,
        deps.get_favorites_service: lambda: favorites_service,
        deps.get_playlist_service: lambda: playlists,
        deps.get_compat_scrobble_adapter: lambda: scrobble,
        deps.get_compat_discover_service: lambda: discover,
        deps.get_compat_id_map_service: lambda: compat_id_map_service,
        deps.get_local_files_service: lambda: Mock(),
        deps.get_coverart_repository: lambda: cover,
        deps.get_preferences_service: lambda: preferences,
        deps.get_now_playing_service: lambda: now_playing,
    }
    app.dependency_overrides.update(overrides)
    client = build_test_client(app)
    return SimpleNamespace(
        client=client, secret=secret, username="alice", ids=ids,
        preferences=preferences, app=app, view=view, favorites=favorites_service,
        playlists=playlists, phs=phs, discover=discover,
        lm=_lm, db=db, id_map=compat_id_map_service, now_playing=now_playing,
    )
