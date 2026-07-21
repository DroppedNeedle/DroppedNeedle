"""Authenticated target HTTP playback measurement used by Phase 10 benchmarks."""

from __future__ import annotations

import asyncio
import hashlib
import os
import socket
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import httpx
import uvicorn
from fastapi import FastAPI

from infrastructure.msgspec_fastapi import MsgSpecJSONResponse
from infrastructure.persistence.auth_store import AuthStore
from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.local_catalog import (
    CatalogMembership,
    LocalAlbum,
    LocalArtist,
    LocalArtistCredit,
    LocalTrack,
)
from services.local_files_service import LocalFilesService
from services.auth_service import AuthService
from services.native.target_library_repository import TargetLibraryRepository


PLAYBACK_TRACK_ID = "feedback-fixes-http-playback-track"


class _Preferences:
    def __init__(self, root: Path) -> None:
        self._root = root

    def get_library_settings(self) -> SimpleNamespace:
        return SimpleNamespace(library_paths=[str(self._root)])

    def get_typed_library_settings(self) -> SimpleNamespace:
        return SimpleNamespace(library_roots=[SimpleNamespace(path=str(self._root))])

    def get_advanced_settings(self) -> SimpleNamespace:
        return SimpleNamespace(cache_ttl_local_files_recently_added=120)


async def seed_http_playback_track(store: NativeLibraryStore, audio_path: Path) -> None:
    stat_result = audio_path.stat()
    artist = LocalArtist(
        id="feedback-fixes-http-playback-artist",
        display_name="Playback Benchmark",
        folded_name="playback benchmark",
        kind="group",
        created_at=1,
        updated_at=1,
    )
    album = LocalAlbum(
        id="feedback-fixes-http-playback-album",
        root_id="playback-root",
        grouping_key="feedback-fixes-http-playback",
        title="HTTP Playback Benchmark",
        album_artist_id=artist.id,
        album_artist_name=artist.display_name,
        created_at=1,
        updated_at=1,
    )
    track = LocalTrack(
        id=PLAYBACK_TRACK_ID,
        local_album_id=album.id,
        root_id="playback-root",
        file_path=str(audio_path),
        relative_path=audio_path.name,
        path_hash=hashlib.sha256(str(audio_path).encode()).hexdigest(),
        file_size_bytes=stat_result.st_size,
        file_mtime_ns=stat_result.st_mtime_ns,
        stat_revision="playback-stat",
        tag_revision="playback-tag",
        title="HTTP Playback Benchmark",
        artist_name=artist.display_name,
        album_title=album.title,
        album_artist_name=artist.display_name,
        file_format=audio_path.suffix.removeprefix("."),
        imported_at=1,
    )
    credit = LocalArtistCredit(
        local_artist_id=artist.id,
        position=0,
        credited_name=artist.display_name,
    )
    await store.create_catalog_membership(
        CatalogMembership(
            album=album,
            artists=[artist],
            tracks=[track],
            album_credits=[credit],
            track_credits={track.id: [credit]},
        )
    )


class AuthenticatedHTTPPlaybackProbe:
    """Measure a 64 KiB range through auth, routing, SQLite, and streaming."""

    def __init__(
        self, store: NativeLibraryStore, allowed_root: Path, audio_path: Path
    ) -> None:
        self._store = store
        self._allowed_root = allowed_root
        self._audio_path = audio_path
        self._client: httpx.AsyncClient | None = None
        self._auth_patch = None
        self._server: uvicorn.Server | None = None
        self._server_task: asyncio.Task[None] | None = None
        self.authentication_rejections = 0
        self.samples = 0
        self.status_codes: set[int] = set()
        self.content_ranges: set[str] = set()
        self.bytes_received = 0
        self._raw_token = ""

    async def __aenter__(self) -> "AuthenticatedHTTPPlaybackProbe":
        if "ROOT_APP_DIR" not in os.environ:
            import_root = self._allowed_root / ".feedback-fixes-benchmark-app"
            import_root.mkdir(parents=True, exist_ok=True)
            os.environ["ROOT_APP_DIR"] = str(import_root)
        from api.v1.routes import auth, stream
        from core.dependencies import get_local_files_service
        from middleware import AuthMiddleware

        await seed_http_playback_track(self._store, self._audio_path)
        local_files = LocalFilesService(
            TargetLibraryRepository(self._store),
            _Preferences(self._allowed_root),
            SimpleNamespace(),
        )
        app = FastAPI(default_response_class=MsgSpecJSONResponse)
        app.include_router(auth.router, prefix="/api/v1")
        app.include_router(stream.router, prefix="/api/v1")
        app.dependency_overrides[get_local_files_service] = lambda: local_files
        app.add_middleware(AuthMiddleware)
        auth_store = AuthStore(self._store.db_path, self._store._write_lock)
        user = await auth_store.get_user_by_id("benchmark-user")
        if user is None:
            await auth_store.create_user(
                id="benchmark-user",
                display_name="Benchmark User",
                role="admin",
                username="benchmark-user",
            )
        self._raw_token, token_hash = auth_store.issue_token()
        await auth_store.store_token(
            id="benchmark-token",
            user_id="benchmark-user",
            token_hash=token_hash,
            user_agent="post-upgrade-rehearsal",
        )
        auth_service = AuthService(auth_store)
        self._auth_patch = patch(
            "core.dependencies.auth_providers.get_auth_service",
            return_value=auth_service,
        )
        self._auth_patch.start()
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = int(listener.getsockname()[1])
        self._server = uvicorn.Server(
            uvicorn.Config(
                app,
                host="127.0.0.1",
                port=port,
                lifespan="off",
                access_log=False,
                log_level="warning",
            )
        )
        self._server_task = asyncio.create_task(self._server.serve())
        for _ in range(500):
            if self._server.started:
                break
            if self._server_task.done():
                await self._server_task
                raise RuntimeError("The playback benchmark HTTP server exited early.")
            await asyncio.sleep(0.01)
        if not self._server.started:
            raise RuntimeError("The playback benchmark HTTP server did not start.")
        self._client = httpx.AsyncClient(
            base_url=f"http://127.0.0.1:{port}",
            timeout=5,
        )
        unauthorized = await self._client.get(
            f"/api/v1/stream/local/{PLAYBACK_TRACK_ID}",
            headers={"Range": "bytes=0-65535"},
        )
        if unauthorized.status_code != 401:
            raise RuntimeError("The playback benchmark route was not auth protected.")
        self.authentication_rejections += 1
        warm = await self._request()
        if warm <= 0:
            raise RuntimeError(
                "The authenticated playback benchmark returned no bytes."
            )
        await self.sample_auth_me()
        return self

    async def __aexit__(self, *_args: object) -> None:
        if self._client is not None:
            await self._client.aclose()
        if self._server is not None:
            self._server.should_exit = True
        if self._server_task is not None:
            await asyncio.wait_for(self._server_task, timeout=5)
        if self._auth_patch is not None:
            self._auth_patch.stop()

    async def _request(self) -> int:
        if self._client is None:
            raise RuntimeError("The playback probe has not started.")
        received = 0
        async with self._client.stream(
            "GET",
            f"/api/v1/stream/local/{PLAYBACK_TRACK_ID}",
            headers={
                "Authorization": f"Bearer {self._raw_token}",
                "Range": "bytes=0-65535",
            },
        ) as response:
            if response.status_code != 206:
                raise RuntimeError(
                    f"Authenticated playback returned HTTP {response.status_code}."
                )
            async for chunk in response.aiter_bytes():
                received += len(chunk)
                if received:
                    break
            self.status_codes.add(response.status_code)
            content_range = response.headers.get("content-range")
            if content_range:
                self.content_ranges.add(content_range)
        if received <= 0:
            raise RuntimeError("Authenticated playback yielded no stream bytes.")
        self.bytes_received += received
        return received

    async def sample(self) -> float:
        from time import perf_counter

        started = perf_counter()
        await self._request()
        self.samples += 1
        return perf_counter() - started

    async def sample_auth_me(self) -> float:
        if self._client is None:
            raise RuntimeError("The playback probe has not started.")
        from time import perf_counter

        started = perf_counter()
        response = await self._client.get(
            "/api/v1/auth/me",
            headers={"Authorization": f"Bearer {self._raw_token}"},
        )
        if response.status_code != 200:
            raise RuntimeError(
                f"Authenticated profile returned HTTP {response.status_code}."
            )
        return perf_counter() - started

    def evidence(self) -> dict[str, object]:
        return {
            "transport": "HTTP/1.1 over loopback TCP with uvicorn and httpx",
            "endpoint": f"/api/v1/stream/local/{PLAYBACK_TRACK_ID}",
            "authentication": "Bearer token through AuthMiddleware",
            "authentication_backend": "AuthService joined token and user lookup",
            "catalog_lookup": "TargetLibraryRepository over NativeLibraryStore SQLite",
            "range": "bytes=0-65535",
            "latency_boundary": "request start through first streamed response bytes",
            "authentication_rejections": self.authentication_rejections,
            "samples": self.samples,
            "status_codes": sorted(self.status_codes),
            "content_ranges": sorted(self.content_ranges),
            "bytes_received": self.bytes_received,
        }
