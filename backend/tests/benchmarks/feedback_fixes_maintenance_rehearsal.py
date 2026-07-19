"""Scratch-only complete-manifest, migration, startup, and rollback rehearsal."""

from __future__ import annotations

import argparse
import asyncio
import importlib.util
import json
import os
import socket
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import threading
import uuid
from math import ceil
from contextlib import suppress
from pathlib import Path
from time import perf_counter, time
from uuid import NAMESPACE_URL, uuid5

# Application imports resolve deployment paths immediately. Give direct CLI runs the
# same isolated root that pytest supplies through tests/conftest.py.
_IMPORT_ROOT: tempfile.TemporaryDirectory[str] | None = None
if "ROOT_APP_DIR" not in os.environ:
    _IMPORT_ROOT = tempfile.TemporaryDirectory(
        prefix="feedback-fixes-maintenance-import-"
    )
    os.environ["ROOT_APP_DIR"] = _IMPORT_ROOT.name

from cryptography.fernet import Fernet
import httpx

from api.v1.schemas.library_policies import LibraryRootSettings, TypedLibrarySettings
from infrastructure.persistence.app_password_store import AppPasswordStore
from infrastructure.persistence.auth_store import AuthStore
from infrastructure.crypto import init_crypto
from infrastructure.persistence.maintenance_manifest import (
    capture_complete_manifest,
    capture_source_identity,
    restore_complete_manifest,
    validate_complete_manifest,
)
from infrastructure.persistence.native_library_store import NativeLibraryStore
from services.compat.app_password_service import AppPasswordService
from services.native.bounded_legacy_catalog_migrator import (
    BoundedLegacyCatalogMigrator,
)
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.target_startup_validator import TargetStartupValidator
from repositories.coverart_disk_cache import get_cache_filename


_BACKEND_ROOT = Path(__file__).parents[2]
_REPOSITORY_ROOT = _BACKEND_ROOT.parent


def _free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


async def _start_process(
    *,
    module: str,
    application_root: Path,
    encryption_key: str,
    transcript: list[dict[str, object]],
) -> tuple[subprocess.Popen[str], object, str]:
    port = _free_port()
    base_url = f"http://127.0.0.1:{port}"
    environment = os.environ.copy()
    python_path = environment.get("PYTHONPATH")
    environment.update(
        {
            "ROOT_APP_DIR": str(application_root),
            "DATA_ENC_KEY": encryption_key,
            "LOG_LEVEL": "INFO",
            "PYTHONPATH": (
                str(_BACKEND_ROOT)
                if not python_path
                else f"{_BACKEND_ROOT}{os.pathsep}{python_path}"
            ),
        }
    )
    log = tempfile.TemporaryFile(mode="w+t", encoding="utf-8")
    started = perf_counter()
    process = subprocess.Popen(
        [
            sys.executable,
            "-m",
            "uvicorn",
            f"{module}:app",
            "--host",
            "127.0.0.1",
            "--port",
            str(port),
            "--workers",
            "1",
            "--no-access-log",
        ],
        cwd=application_root,
        env=environment,
        stdout=log,
        stderr=subprocess.STDOUT,
        text=True,
    )
    deadline = perf_counter() + 120
    async with httpx.AsyncClient(timeout=5) as client:
        while perf_counter() < deadline:
            if process.poll() is not None:
                log.seek(0)
                raise RuntimeError(
                    f"Scratch {module} process exited during startup: {log.read()[-4000:]}"
                )
            try:
                response = await client.get(f"{base_url}/health")
                if response.status_code == 200:
                    transcript.append(
                        {
                            "event": "started",
                            "application": module,
                            "pid": process.pid,
                            "workers": 1,
                            "readiness_status": response.status_code,
                            "elapsed_seconds": perf_counter() - started,
                        }
                    )
                    print(
                        json.dumps(
                            {
                                "event": "started",
                                "application": module,
                                "pid": process.pid,
                            },
                            sort_keys=True,
                        ),
                        flush=True,
                    )
                    return process, log, base_url
            except httpx.HTTPError:
                pass
            await asyncio.sleep(0.1)
    process.terminate()
    with suppress(subprocess.TimeoutExpired):
        process.wait(timeout=10)
    log.seek(0)
    raise RuntimeError(
        f"Scratch {module} process did not become ready: {log.read()[-4000:]}"
    )


def _database_accepts_writer(database_path: Path) -> bool:
    try:
        with sqlite3.connect(database_path, timeout=0) as connection:
            connection.execute("BEGIN IMMEDIATE")
            connection.rollback()
        return True
    except sqlite3.OperationalError:
        return False


def _stop_process(
    process: subprocess.Popen[str],
    log: object,
    *,
    module: str,
    database_path: Path,
    transcript: list[dict[str, object]],
) -> dict[str, object]:
    started = perf_counter()
    process.terminate()
    try:
        exit_code = process.wait(timeout=30)
    except subprocess.TimeoutExpired:
        process.kill()
        exit_code = process.wait(timeout=10)
    log.seek(0)
    output = log.read()
    log.close()
    writer_probe = _database_accepts_writer(database_path)
    error_lines = [
        line
        for line in output.splitlines()
        if line.startswith("ERROR:") or " - ERROR - " in line
    ]
    error_loggers = sorted(
        {parts[1] for line in error_lines if len(parts := line.split(" - ", 3)) == 4}
    )
    evidence = {
        "event": "stopped",
        "application": module,
        "pid": process.pid,
        "exit_code": exit_code,
        "process_exited": process.poll() is not None,
        "database_writer_lock_available": writer_probe,
        "elapsed_seconds": perf_counter() - started,
        "log_line_count": len(output.splitlines()),
        "error_line_count": len(error_lines),
        "error_loggers": error_loggers,
    }
    transcript.append(evidence)
    print(
        json.dumps(
            {
                "event": "stopped",
                "application": module,
                "pid": process.pid,
                "writer_lock_available": writer_probe,
            },
            sort_keys=True,
        ),
        flush=True,
    )
    return evidence


async def _seed_process_auth(database_path: Path) -> tuple[str, str]:
    with sqlite3.connect(database_path) as connection:
        columns = {
            row[1] for row in connection.execute("PRAGMA table_info(auth_users)")
        }
        additions = {
            "email": "TEXT",
            "avatar_url": "TEXT",
            "role": "TEXT NOT NULL DEFAULT 'admin'",
            "created_at": "TEXT NOT NULL DEFAULT '2026-07-01T00:00:00+00:00'",
            "last_login_at": "TEXT",
        }
        for name, declaration in additions.items():
            if name not in columns:
                connection.execute(
                    f"ALTER TABLE auth_users ADD COLUMN {name} {declaration}"
                )
        connection.execute("UPDATE auth_users SET role = 'admin' WHERE id = 'alice'")
    lock = threading.Lock()
    auth = AuthStore(database_path, lock)
    raw_token, token_hash = auth.issue_token()
    await auth.store_token(
        id=str(uuid.uuid4()),
        user_id="alice",
        token_hash=token_hash,
        user_agent="feedback-fixes-maintenance-rehearsal",
    )
    app_passwords = AppPasswordService(AppPasswordStore(database_path, lock), auth)
    _, app_secret = await app_passwords.create(
        "alice", "Feedback Fixes maintenance rehearsal"
    )
    return raw_token, app_secret


async def _http_smoke(
    *,
    base_url: str,
    bearer_token: str,
    app_secret: str,
    database_path: Path,
    target: bool,
) -> dict[str, object]:
    bearer = {"Authorization": f"Bearer {bearer_token}"}
    subsonic = {
        "v": "1.16.1",
        "c": "feedback-fixes-rehearsal",
        "f": "json",
        "apiKey": app_secret,
    }
    jellyfin = {
        "Authorization": (
            f'MediaBrowser Token="{app_secret}", ' 'Client="Feedback Fixes rehearsal"'
        )
    }

    async with httpx.AsyncClient(base_url=base_url, timeout=30) as client:

        async def json_get(
            path: str,
            *,
            params: dict[str, object] | None = None,
            headers: dict[str, str] | None = None,
        ) -> dict[str, object]:
            response = await client.get(path, params=params, headers=headers)
            response.raise_for_status()
            return response.json()

        albums = await json_get(
            "/api/v1/library/albums",
            params={"page_size": 100},
            headers=bearer,
        )
        await json_get(
            "/api/v1/library/tracks",
            params={"limit": 200},
            headers=bearer,
        )
        local_album_search = await json_get(
            "/api/v1/library/albums",
            params={"page_size": 10, "q": "Local Album"},
            headers=bearer,
        )
        local_track_search = await json_get(
            "/api/v1/library/tracks",
            params={"limit": 10, "q": "Local Album"},
            headers=bearer,
        )
        compilation_album_search = await json_get(
            "/api/v1/library/albums",
            params={"page_size": 10, "q": "Compilation"},
            headers=bearer,
        )
        compilation_track_search = await json_get(
            "/api/v1/library/tracks",
            params={"limit": 10, "q": "Compilation"},
            headers=bearer,
        )
        native_local_album = next(
            (
                item
                for item in local_album_search["items"]
                if (item.get("title") or item.get("album_title")) == "Local Album"
            ),
            None,
        )
        native_local_track = next(
            (
                item
                for item in local_track_search["items"]
                if (item.get("album_title") or item.get("album_name")) == "Local Album"
            ),
            None,
        )
        native_play_track = (
            native_local_track
            if target
            else next(
                (
                    item
                    for item in compilation_track_search["items"]
                    if (item.get("album_title") or item.get("album_name"))
                    == "Compilation"
                ),
                None,
            )
        )
        native_playback = None
        if native_play_track is not None:
            file_id = native_play_track.get("id") or native_play_track.get(
                "track_file_id"
            )
            native_playback = await client.get(
                f"/api/v1/stream/local/{file_id}",
                headers={**bearer, "Range": "bytes=0-3"},
            )
            native_playback.raise_for_status()

        playlist_cover = await client.get(
            "/api/v1/playlists/playlist-1/cover", headers=bearer
        )
        playlist_cover.raise_for_status()
        provider_cover = await client.get(
            "/api/v1/covers/release-group/11111111-1111-4111-8111-111111111111",
            headers=bearer,
        )
        provider_cover.raise_for_status()
        secret_status = await json_get("/api/v1/lidarr-import/status", headers=bearer)

        subsonic_albums = await json_get(
            "/subsonic/rest/getAlbumList2",
            params={**subsonic, "type": "alphabeticalByName", "size": 100},
        )
        subsonic_album_items = (
            subsonic_albums["subsonic-response"].get("albumList2", {}).get("album", [])
        )
        subsonic_local_results = await json_get(
            "/subsonic/rest/search3",
            params={
                **subsonic,
                "query": "Local Album",
                "artistCount": 0,
                "albumCount": 10,
                "songCount": 0,
            },
        )
        subsonic_compilation_results = await json_get(
            "/subsonic/rest/search3",
            params={
                **subsonic,
                "query": "Compilation",
                "artistCount": 0,
                "albumCount": 10,
                "songCount": 0,
            },
        )
        subsonic_local_items = (
            subsonic_local_results["subsonic-response"]
            .get("searchResult3", {})
            .get("album", [])
        )
        subsonic_compilation_items = (
            subsonic_compilation_results["subsonic-response"]
            .get("searchResult3", {})
            .get("album", [])
        )
        subsonic_local_album = next(
            (
                item
                for item in subsonic_local_items
                if item.get("name") == "Local Album"
            ),
            None,
        )
        subsonic_local_track = None
        subsonic_playback = None
        subsonic_play_album = (
            subsonic_local_album
            if target
            else next(
                (
                    item
                    for item in subsonic_compilation_items
                    if item.get("name") == "Compilation"
                ),
                None,
            )
        )
        if subsonic_play_album is not None:
            subsonic_detail = await json_get(
                "/subsonic/rest/getAlbum",
                params={**subsonic, "id": subsonic_play_album["id"]},
            )
            songs = (
                subsonic_detail["subsonic-response"].get("album", {}).get("song", [])
            )
            subsonic_local_track = songs[0] if songs else None
            if subsonic_local_track is not None:
                subsonic_playback = await client.get(
                    "/subsonic/rest/stream",
                    params={**subsonic, "id": subsonic_local_track["id"]},
                    headers={"Range": "bytes=0-3"},
                )
                subsonic_playback.raise_for_status()
        identified_subsonic = next(
            item
            for item in subsonic_compilation_items
            if item.get("name") == "Compilation"
        )
        subsonic_cover = await client.get(
            "/subsonic/rest/getCoverArt",
            params={**subsonic, "id": identified_subsonic["coverArt"]},
        )
        subsonic_cover.raise_for_status()

        jellyfin_albums = await json_get(
            "/jellyfin/Items",
            params={"IncludeItemTypes": "MusicAlbum", "Limit": 100},
            headers=jellyfin,
        )
        await json_get(
            "/jellyfin/Items",
            params={"IncludeItemTypes": "Audio", "Limit": 200},
            headers=jellyfin,
        )
        jellyfin_local_albums = await json_get(
            "/jellyfin/Items",
            params={
                "IncludeItemTypes": "MusicAlbum",
                "Limit": 10,
                "SearchTerm": "Local Album",
            },
            headers=jellyfin,
        )
        jellyfin_local_tracks = await json_get(
            "/jellyfin/Items",
            params={
                "IncludeItemTypes": "Audio",
                "Limit": 10,
                "SearchTerm": "Local Album",
            },
            headers=jellyfin,
        )
        jellyfin_compilation_albums = await json_get(
            "/jellyfin/Items",
            params={
                "IncludeItemTypes": "MusicAlbum",
                "Limit": 10,
                "SearchTerm": "Compilation",
            },
            headers=jellyfin,
        )
        jellyfin_compilation_tracks = await json_get(
            "/jellyfin/Items",
            params={
                "IncludeItemTypes": "Audio",
                "Limit": 10,
                "SearchTerm": "Compilation",
            },
            headers=jellyfin,
        )
        jellyfin_local_album = next(
            (
                item
                for item in jellyfin_local_albums["Items"]
                if item.get("Name") == "Local Album"
            ),
            None,
        )
        jellyfin_local_track = next(
            (
                item
                for item in jellyfin_local_tracks["Items"]
                if item.get("Album") == "Local Album"
            ),
            None,
        )
        jellyfin_play_track = (
            jellyfin_local_track
            if target
            else next(
                (
                    item
                    for item in jellyfin_compilation_tracks["Items"]
                    if item.get("Album") == "Compilation"
                ),
                None,
            )
        )
        jellyfin_playback = None
        if jellyfin_play_track is not None:
            jellyfin_playback = await client.get(
                f"/jellyfin/Audio/{jellyfin_play_track['Id']}/stream",
                params={"static": "true", "ApiKey": app_secret},
                headers={"Range": "bytes=0-3"},
            )
            jellyfin_playback.raise_for_status()
        identified_jellyfin = next(
            item
            for item in jellyfin_compilation_albums["Items"]
            if item.get("Name") == "Compilation"
        )
        jellyfin_cover = await client.get(
            f"/jellyfin/Items/{identified_jellyfin['Id']}/Images/Primary"
        )
        jellyfin_cover.raise_for_status()

        cached_native_cover_status = None
        if target:
            identified_native = next(
                item
                for item in compilation_album_search["items"]
                if item.get("musicbrainz_release_group_id")
                == "11111111-1111-4111-8111-111111111111"
            )
            with sqlite3.connect(database_path) as connection:
                cover_version = int(
                    connection.execute(
                        "SELECT version FROM local_album_artwork "
                        "WHERE local_album_id = ?",
                        (identified_native["id"],),
                    ).fetchone()[0]
                )
            cached_native_cover = await client.get(
                f"/api/v1/library/albums/{identified_native['id']}/artwork/cached",
                params={"v": cover_version},
                headers=bearer,
            )
            cached_native_cover.raise_for_status()
            cached_native_cover_status = cached_native_cover.status_code

    local_expected = target
    local_browse_consistent = all(
        value is local_expected
        for value in (
            native_local_album is not None,
            subsonic_local_album is not None,
            jellyfin_local_album is not None,
        )
    )
    return {
        "authenticated_native_status": 200,
        "native_album_count": int(albums["total"]),
        "subsonic_album_count": len(subsonic_album_items),
        "jellyfin_album_count": int(jellyfin_albums["TotalRecordCount"]),
        "local_only_expected": local_expected,
        "local_only_native": native_local_album is not None,
        "local_only_subsonic": subsonic_local_album is not None,
        "local_only_jellyfin": jellyfin_local_album is not None,
        "local_only_browse_consistent": local_browse_consistent,
        "native_range_status": (
            native_playback.status_code if native_playback is not None else None
        ),
        "subsonic_range_status": (
            subsonic_playback.status_code if subsonic_playback is not None else None
        ),
        "jellyfin_range_status": (
            jellyfin_playback.status_code if jellyfin_playback is not None else None
        ),
        "native_playback_prefix_ok": (
            native_playback.content == b"fLaC" if native_playback is not None else None
        ),
        "subsonic_playback_prefix_ok": (
            subsonic_playback.content == b"fLaC"
            if subsonic_playback is not None
            else None
        ),
        "jellyfin_playback_prefix_ok": (
            jellyfin_playback.content == b"fLaC"
            if jellyfin_playback is not None
            else None
        ),
        "playlist_artwork_status": playlist_cover.status_code,
        "provider_artwork_status": provider_cover.status_code,
        "subsonic_artwork_status": subsonic_cover.status_code,
        "jellyfin_artwork_status": jellyfin_cover.status_code,
        "cached_native_artwork_status": cached_native_cover_status,
        "restored_artwork_bytes_match": all(
            response.content.startswith(b"\x89PNG\r\n\x1a\n")
            for response in (provider_cover, subsonic_cover, jellyfin_cover)
        )
        and playlist_cover.content.startswith(b"\xff\xd8\xff"),
        "paired_secret_loaded_by_application": secret_status.get("configured") is True,
    }


def _fixture_module():
    path = (
        Path(__file__).parents[1] / "infrastructure" / "test_legacy_catalog_importer.py"
    )
    spec = importlib.util.spec_from_file_location(
        "feedback_fixes_maintenance_fixture", path
    )
    if spec is None or spec.loader is None:
        raise RuntimeError("The migration fixture module could not be loaded.")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _write_audio_fixture(music_root: Path) -> None:
    fixture = Path(__file__).parents[1] / "fixtures" / "library" / "flac_no_tags.flac"
    for relative in (
        "Compilation/01.flac",
        "Compilation/02.flac",
        "Local Album/01.flac",
        "Rejected/01.flac",
    ):
        destination = music_root / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(fixture, destination)


def _seed_production_shape(
    database_path: Path, music_root: Path, *, total_files: int
) -> dict[str, object]:
    """Extend the representative fixture to the approved 115,000-file shape."""

    existing_files = 4
    additional = total_files - existing_files
    if additional < 0:
        raise ValueError("The production-shaped fixture requires at least four files.")
    artist_id = str(uuid5(NAMESPACE_URL, "feedback-fixes:scale-artist"))
    inserted = 0
    started = perf_counter()
    with sqlite3.connect(database_path) as connection:
        batch: list[tuple[object, ...]] = []
        for index in range(additional):
            album_index = index // 10
            track_index = index % 10 + 1
            album_id = str(
                uuid5(NAMESPACE_URL, f"feedback-fixes:scale-album:{album_index}")
            )
            release_id = str(
                uuid5(NAMESPACE_URL, f"feedback-fixes:scale-release:{album_index}")
            )
            recording_id = str(
                uuid5(NAMESPACE_URL, f"feedback-fixes:scale-recording:{index}")
            )
            track_id = str(uuid5(NAMESPACE_URL, f"feedback-fixes:scale-track:{index}"))
            batch.append(
                (
                    track_id,
                    album_id,
                    release_id,
                    recording_id,
                    1,
                    track_index,
                    f"Scale Track {index:06d}",
                    "Scale Artist",
                    artist_id,
                    "Scale Artist",
                    artist_id,
                    f"Scale Album {album_index:05d}",
                    2026,
                    str(
                        music_root
                        / "Scale"
                        / f"{album_index:05d}"
                        / f"{track_index:02d}.flac"
                    ),
                    8_000_000,
                    1_750_000_000.0 + index,
                    240.0,
                    "flac",
                    900_000,
                    48_000,
                    24,
                    2,
                    "scan",
                    0,
                    1_750_000_000.0,
                    1_750_000_000.0,
                    "Electronic",
                )
            )
            if len(batch) == 1_000:
                connection.executemany(
                    "INSERT INTO library_files "
                    "(id, release_group_mbid, release_mbid, recording_mbid, disc_number, "
                    "track_number, track_title, artist_name, artist_mbid, album_artist_name, "
                    "album_artist_mbid, album_title, year, file_path, file_size_bytes, "
                    "file_mtime, duration_seconds, file_format, bit_rate, sample_rate, "
                    "bit_depth, channels, source, is_compilation, tagged_at, imported_at, "
                    "genre) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                    batch,
                )
                inserted += len(batch)
                batch.clear()
        if batch:
            connection.executemany(
                "INSERT INTO library_files "
                "(id, release_group_mbid, release_mbid, recording_mbid, disc_number, "
                "track_number, track_title, artist_name, artist_mbid, album_artist_name, "
                "album_artist_mbid, album_title, year, file_path, file_size_bytes, "
                "file_mtime, duration_seconds, file_format, bit_rate, sample_rate, "
                "bit_depth, channels, source, is_compilation, tagged_at, imported_at, "
                "genre) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                batch,
            )
            inserted += len(batch)
        connection.commit()
        connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        reconciled_file_count = int(
            connection.execute(
                "SELECT COUNT(*) FROM ("
                "SELECT file_path FROM library_files "
                "UNION SELECT file_path FROM manual_review_queue)"
            ).fetchone()[0]
        )
    if reconciled_file_count != total_files:
        raise RuntimeError(
            "The generated maintenance workload has the wrong file count."
        )
    return {
        "approved_file_count": total_files,
        "reconciled_file_count": reconciled_file_count,
        "base_fixture_files": existing_files,
        "generated_library_rows": inserted,
        "generated_album_groups": ceil(additional / 10),
        "generation_seconds": perf_counter() - started,
        "database_bytes": database_path.stat().st_size,
    }


def _seed_managed_asset_shape(
    covers_dir: Path, *, total_bytes: int, file_count: int = 256
) -> dict[str, object]:
    started = perf_counter()
    generated_dir = covers_dir / "production-shape"
    generated_dir.mkdir(parents=True, exist_ok=True)
    remaining = total_bytes
    for index in range(file_count):
        files_left = file_count - index
        size = remaining // files_left
        path = generated_dir / f"cache-{index:04d}.bin"
        with path.open("wb") as stream:
            stream.truncate(size)
        remaining -= size
    generated = sorted(generated_dir.iterdir())
    return {
        "generated_file_count": len(generated),
        "generated_bytes": sum(path.stat().st_size for path in generated),
        "generation_seconds": perf_counter() - started,
    }


async def run(
    output: Path,
    *,
    source_commit: str,
    file_count: int = 115_000,
    managed_asset_bytes: int = 128 * 1024 * 1024,
) -> dict[str, object]:
    started = perf_counter()
    with tempfile.TemporaryDirectory(prefix="feedback-fixes-maintenance-") as directory:
        scratch = Path(directory)
        process_transcript: list[dict[str, object]] = []
        source_root = scratch / "closed-source"
        config_dir = source_root / "config"
        cache_dir = source_root / "cache"
        covers_dir = cache_dir / "covers"
        playlist_dir = covers_dir / "playlists"
        playlist_dir.mkdir(parents=True)
        config_dir.mkdir(parents=True)
        music_root = scratch / "Music"
        music_root.mkdir()
        _write_audio_fixture(music_root)

        fixture = _fixture_module()
        source_database = cache_dir / "library.db"
        fixture._create_source(source_database, music_root)
        workload_shape = _seed_production_shape(
            source_database, music_root, total_files=file_count
        )
        playlist_cover = playlist_dir / "mix.jpg"
        playlist_cover.write_bytes(b"\xff\xd8\xffmanaged-playlist-cover")
        provider_cover = covers_dir / (
            get_cache_filename("rg_11111111-1111-4111-8111-111111111111", "500")
            + ".bin"
        )
        provider_cover.write_bytes(b"\x89PNG\r\n\x1a\nmanaged-provider-cover")
        managed_asset_shape = _seed_managed_asset_shape(
            covers_dir, total_bytes=managed_asset_bytes
        )
        with sqlite3.connect(source_database) as connection:
            connection.execute(
                "UPDATE playlists SET cover_image_path = ? WHERE id = 'playlist-1'",
                ("cache/covers/playlists/mix.jpg",),
            )
            # Preserve explicit zero-source reconciliation evidence in the retained
            # production-shaped artifact. Non-zero mapping for both kinds is covered by
            # the focused importer suite.
            connection.execute("DELETE FROM album_release_pins")
            connection.execute("DELETE FROM compat_bookmarks")

        key = Fernet.generate_key()
        environment_path = config_dir / ".env"
        environment_path.write_text(f"DATA_ENC_KEY={key.decode()}\n", encoding="utf-8")
        environment_path.chmod(0o600)
        os.environ["DATA_ENC_KEY"] = key.decode()
        init_crypto(config_dir)
        bearer_token, app_secret = await _seed_process_auth(source_database)
        encrypted_probe = Fernet(key).encrypt(b"paired-secret-probe").decode()
        config_path = config_dir / "config.json"
        config_path.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "connect_apps": {
                        "subsonic_enabled": True,
                        "jellyfin_enabled": True,
                    },
                    "library_scan_schedule": {"scan_frequency": "manual"},
                    "lidarr_import": {
                        "url": "http://127.0.0.1:9",
                        "api_key": encrypted_probe,
                    },
                    "library_settings": {
                        "library_paths": [str(music_root)],
                        "library_roots": [
                            {
                                "id": "root-1",
                                "path": str(music_root),
                                "label": "Music",
                                "policy": "automatic",
                            }
                        ],
                    },
                }
            ),
            encoding="utf-8",
        )
        source_process, source_log, source_url = await _start_process(
            module="main",
            application_root=source_root,
            encryption_key=key.decode(),
            transcript=process_transcript,
        )
        try:
            live_source_smoke = await _http_smoke(
                base_url=source_url,
                bearer_token=bearer_token,
                app_secret=app_secret,
                database_path=source_database,
                target=False,
            )
        finally:
            source_stop = _stop_process(
                source_process,
                source_log,
                module="main",
                database_path=source_database,
                transcript=process_transcript,
            )
        if not (
            source_stop["process_exited"]
            and source_stop["database_writer_lock_available"]
        ):
            raise RuntimeError(
                "The scratch source did not close every database writer."
            )

        manifest_root = scratch / "manifest"
        source_identity = capture_source_identity(_REPOSITORY_ROOT)
        if source_identity["commit"] != source_commit:
            raise RuntimeError(
                "The requested source commit is not the current worktree base."
            )
        prior_application = {
            "container_id": "isolated-source-process",
            "image_id": "sha256:" + source_identity["worktree_sha256"],
            "rollback_image_reference": "scratch:feedback-fixes-source",
            "entrypoint": [sys.executable, "-m", "uvicorn"],
            "command": ["main:app", "--workers", "1"],
            "launch_command": [
                sys.executable,
                "-m",
                "uvicorn",
                "main:app",
                "--workers",
                "1",
            ],
            "compose_config_sha256": source_identity["diff_sha256"],
        }
        capture = capture_complete_manifest(
            source_root=source_root,
            database_path=source_database,
            config_path=config_path,
            environment_path=environment_path,
            destination=manifest_root,
            application_source_root=_REPOSITORY_ROOT,
            prior_application=prior_application,
            closed_source_confirmed=True,
        )
        validation_started = perf_counter()
        validated = validate_complete_manifest(
            manifest_root,
            expected_source_identity=source_identity,
            expected_prior_application=prior_application,
        )
        manifest_validation_seconds = perf_counter() - validation_started

        rollback_root = scratch / "rollback-source"
        rollback_restore = restore_complete_manifest(manifest_root, rollback_root)
        restored_config = json.loads(
            (rollback_root / "config" / "config.json").read_text(encoding="utf-8")
        )
        restored_key = next(
            line.split("=", 1)[1]
            for line in (rollback_root / "config" / ".env")
            .read_text(encoding="utf-8")
            .splitlines()
            if line.startswith("DATA_ENC_KEY=")
        )
        secret_pair_valid = (
            Fernet(restored_key.encode())
            .decrypt(restored_config["lidarr_import"]["api_key"].encode())
            .decode()
            == "paired-secret-probe"
        )
        rollback_database = rollback_root / "cache" / "library.db"
        restored_process, restored_log, restored_url = await _start_process(
            module="main",
            application_root=rollback_root,
            encryption_key=restored_key,
            transcript=process_transcript,
        )
        try:
            source_smoke = await _http_smoke(
                base_url=restored_url,
                bearer_token=bearer_token,
                app_secret=app_secret,
                database_path=rollback_database,
                target=False,
            )
        finally:
            restored_source_stop = _stop_process(
                restored_process,
                restored_log,
                module="main",
                database_path=rollback_database,
                transcript=process_transcript,
            )

        migration_root = scratch / "migration-source"
        migration_restore = restore_complete_manifest(manifest_root, migration_root)
        migration_database = migration_root / "cache" / "library.db"
        store = NativeLibraryStore(migration_database, threading.Lock())
        resolver = LibraryPolicyResolver(
            TypedLibrarySettings(
                library_roots=[
                    LibraryRootSettings(
                        id="root-1",
                        path=str(music_root),
                        label="Music",
                        policy="automatic",
                    )
                ]
            )
        )
        migration_prepare_seconds = 0.0
        apply_started = perf_counter()
        applied_outcome = await BoundedLegacyCatalogMigrator(
            store,
            resolver,
            emit_progress=lambda _message: None,
        ).migrate("maintenance-rehearsal", now=101)
        migration_apply_seconds = perf_counter() - apply_started
        repeat_started = perf_counter()
        repeated_outcome = await BoundedLegacyCatalogMigrator(
            store,
            resolver,
            emit_progress=lambda _message: None,
        ).migrate("maintenance-rehearsal", now=102)
        migration_repeat_seconds = perf_counter() - repeat_started
        applied = applied_outcome.report
        repeated = repeated_outcome.report
        startup_validation_started = perf_counter()
        startup = await TargetStartupValidator(store).validate("cutover")
        startup_validation_seconds = perf_counter() - startup_validation_started
        reopened_twice = NativeLibraryStore(migration_database, threading.Lock())
        reopened_invariants = await reopened_twice.validate_migrated_catalog()
        with sqlite3.connect(migration_database) as connection:
            connection.execute(
                "UPDATE library_identification_jobs SET not_before = ? "
                "WHERE state = 'queued'",
                (time() + 86_400,),
            )

        source_stopped_before_target = (
            source_process.poll() is not None
            and restored_process.poll() is not None
            and _database_accepts_writer(migration_database)
        )
        if not source_stopped_before_target:
            raise RuntimeError(
                "A scratch source writer remained before target admission."
            )
        target_process, target_log, target_url = await _start_process(
            module="target_main",
            application_root=migration_root,
            encryption_key=restored_key,
            transcript=process_transcript,
        )
        try:
            target_smoke = await _http_smoke(
                base_url=target_url,
                bearer_token=bearer_token,
                app_secret=app_secret,
                database_path=migration_database,
                target=True,
            )
        finally:
            target_stop = _stop_process(
                target_process,
                target_log,
                module="target_main",
                database_path=migration_database,
                transcript=process_transcript,
            )

        with sqlite3.connect(migration_database) as connection:
            connection.execute(
                "UPDATE library_migration_markers "
                "SET target_catalog_revision = target_catalog_revision + 1000000"
            )
        startup_refused = False
        try:
            await TargetStartupValidator(reopened_twice).validate("cutover")
        except Exception as exc:  # noqa: BLE001 - the report records only the type
            startup_refused = type(exc).__name__ == "TargetStartupInvariantError"

        final_rollback_root = scratch / "final-rollback-source"
        final_rollback = restore_complete_manifest(manifest_root, final_rollback_root)
        final_rollback_database = final_rollback_root / "cache" / "library.db"
        final_process, final_log, final_url = await _start_process(
            module="main",
            application_root=final_rollback_root,
            encryption_key=restored_key,
            transcript=process_transcript,
        )
        try:
            final_source_smoke = await _http_smoke(
                base_url=final_url,
                bearer_token=bearer_token,
                app_secret=app_secret,
                database_path=final_rollback_database,
                target=False,
            )
        finally:
            final_source_stop = _stop_process(
                final_process,
                final_log,
                module="main",
                database_path=final_rollback_database,
                transcript=process_transcript,
            )

        stop_evidence = (
            source_stop,
            restored_source_stop,
            target_stop,
            final_source_stop,
        )
        closed_source_writer_count = sum(
            not bool(item["process_exited"])
            or not bool(item["database_writer_lock_available"])
            for item in stop_evidence
        )
        no_dual_write = (
            source_stopped_before_target
            and closed_source_writer_count == 0
            and target_process.poll() is not None
        )
        target_start_seconds = next(
            float(item["elapsed_seconds"])
            for item in process_transcript
            if item["event"] == "started" and item["application"] == "target_main"
        )
        expected_downtime_seconds = sum(
            (
                float(source_stop["elapsed_seconds"]),
                float(capture["capture_seconds"]),
                manifest_validation_seconds,
                migration_prepare_seconds,
                migration_apply_seconds,
                startup_validation_seconds,
                target_start_seconds,
            )
        )
        rollback_downtime_seconds = sum(
            (
                float(target_stop["elapsed_seconds"]),
                float(final_rollback["restore_seconds"]),
                next(
                    float(item["elapsed_seconds"])
                    for item in reversed(process_transcript)
                    if item["event"] == "started" and item["application"] == "main"
                ),
            )
        )
        manifest_bytes = sum(int(entry["size_bytes"]) for entry in capture["files"])
        report = {
            "format_version": 3,
            "fixture": "generated-closed-source-with-managed-assets-v3",
            "source_commit": source_commit,
            "source_identity": source_identity,
            "prior_application": prior_application,
            "production_shape": {
                **workload_shape,
                "managed_assets": managed_asset_shape,
                "source_database_bytes_after_auth": source_database.stat().st_size,
            },
            "closed_source_writer_count": closed_source_writer_count,
            "process_transcript": process_transcript,
            "live_source_smoke": live_source_smoke,
            "manifest": {
                "file_count": len(capture["files"]),
                "bytes": sum(entry["size_bytes"] for entry in capture["files"]),
                "capture_seconds": capture["capture_seconds"],
                "validation_seconds": manifest_validation_seconds,
                "copy_throughput_bytes_per_second": manifest_bytes
                / max(float(capture["capture_seconds"]), 0.000001),
                "validated": validated["database"]["quick_check"] == "ok",
                "encryption_key_present": validated["encryption_key_present"],
                "secret_pair_valid": secret_pair_valid,
                "managed_assets": sum(
                    entry["kind"] == "managed_asset" for entry in capture["files"]
                ),
                "asset_reconciliation": capture["managed_assets"],
            },
            "source_restore": {**rollback_restore, "smoke": source_smoke},
            "migration_restore": migration_restore,
            "migration": {
                "migration_mode": "bounded_streaming",
                "applied_state": applied.state,
                "repeat_state": repeated.state,
                "idempotent": applied == repeated,
                "prepare_seconds": migration_prepare_seconds,
                "apply_seconds": migration_apply_seconds,
                "repeat_apply_seconds": migration_repeat_seconds,
                "startup_validation_seconds": startup_validation_seconds,
                "bundle_transactions": (
                    applied.identified_albums + applied.local_only_albums
                ),
                "reference_batch_transactions": ceil(
                    max(
                        sum(
                            count.mapped
                            for count in applied.reference_counts
                            if count.user_id is None
                        ),
                        1,
                    )
                    / 500
                ),
                "reference_counts": [
                    {
                        "kind": count.kind,
                        "user_id": count.user_id,
                        "source": count.source,
                        "mapped": count.mapped,
                        "unresolved": count.unresolved,
                        "duplicates": count.duplicate,
                    }
                    for count in applied.reference_counts
                ],
                "zero_source_reference_kinds": sorted(
                    count.kind
                    for count in applied.reference_counts
                    if count.user_id is None and count.source == 0
                ),
                "network_calls": applied.network_calls,
                "tag_reads": applied.tag_reads,
                "fingerprints": applied.fingerprints,
            },
            "target_startup": {
                "validated": all(
                    value == 0 for value in startup["invariants"].values()
                ),
                "reopened_invariants": reopened_invariants,
                "store_constructed_twice": True,
                "failed_invariant_refused": startup_refused,
                "smoke": target_smoke,
            },
            "full_rollback": {**final_rollback, "smoke": final_source_smoke},
            "downtime": {
                "expected_cutover_seconds_excluding_prebuilt_image": expected_downtime_seconds,
                "rollback_seconds": rollback_downtime_seconds,
                "target_start_seconds": target_start_seconds,
                "manifest_capture_seconds": capture["capture_seconds"],
                "manifest_validation_seconds": manifest_validation_seconds,
                "migration_prepare_seconds": migration_prepare_seconds,
                "migration_apply_seconds": migration_apply_seconds,
                "startup_validation_seconds": startup_validation_seconds,
            },
            "no_runtime_selector": "target_application"
            not in (Path(__file__).parents[2] / "main.py").read_text(encoding="utf-8"),
            "source_stopped_before_target": source_stopped_before_target,
            "no_final_delta_or_dual_write": no_dual_write,
            "elapsed_seconds": perf_counter() - started,
        }
        serialized = json.dumps(report, sort_keys=True)
        if key.decode() in serialized or "paired-secret-probe" in serialized:
            raise RuntimeError("The maintenance report exposed a protected value.")
        if not report["migration"]["zero_source_reference_kinds"]:
            raise RuntimeError("The rehearsal omitted explicit zero-source evidence.")
        if not no_dual_write or closed_source_writer_count:
            raise RuntimeError(
                "The rehearsal did not prove a closed-source transition."
            )

    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return report


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--file-count", type=int, default=115_000)
    parser.add_argument("--managed-asset-bytes", type=int, default=128 * 1024 * 1024)
    args = parser.parse_args()
    report = asyncio.run(
        run(
            args.output,
            source_commit=args.source_commit,
            file_count=args.file_count,
            managed_asset_bytes=args.managed_asset_bytes,
        )
    )
    print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    main()
