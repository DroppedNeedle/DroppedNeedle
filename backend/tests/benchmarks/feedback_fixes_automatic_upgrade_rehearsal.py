"""Docker rehearsal of a normal image update with no maintenance commands."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
import time
import uuid
from pathlib import Path
from typing import Any, Sequence
from urllib.error import URLError
from urllib.request import urlopen

from infrastructure.persistence.maintenance_manifest import capture_source_identity
from tests.benchmarks.feedback_fixes_cli_rehearsal import (
    _PRODUCTION_CONTAINER,
    _database_shape,
    _docker_object,
    _free_port,
    _legacy_source_image,
    _seed_source,
    _wait_for_health,
)

_REPOSITORY_ROOT = Path(__file__).parents[3]


def _compose_text(
    *,
    image: str,
    container: str,
    data_root: Path,
    scratch: Path,
    port: int,
    container_port: int = 8688,
    puid: int | None = None,
    pgid: int | None = None,
    user: str | None = None,
    read_only: bool = False,
    container_read_only: bool = False,
) -> str:
    runtime_user = f"    user: '{user}'\n" if user is not None else ""
    root_filesystem = (
        "    read_only: true\n    tmpfs:\n      - /tmp:size=64m\n"
        if container_read_only
        else ""
    )
    runtime_puid = os.getuid() if puid is None else puid
    runtime_pgid = os.getgid() if pgid is None else pgid
    mount_mode = ":ro" if read_only else ""
    return f"""services:
  droppedneedle:
    image: ${{DROPPEDNEEDLE_IMAGE:-{image}}}
    build:
      context: {_REPOSITORY_ROOT}
      dockerfile: Dockerfile
      args:
        COMMIT_TAG: automatic-upgrade-rehearsal
        BUILD_DATE: ''
        DROPPEDNEEDLE_SOURCE_REVISION: unknown
    container_name: {container}
{runtime_user}{root_filesystem}    environment:
      PUID: '{runtime_puid}'
      PGID: '{runtime_pgid}'
      PORT: '{container_port}'
      TZ: Europe/London
    ports:
      - '127.0.0.1:{port}:{container_port}'
    volumes:
      - {data_root / 'config'}:/app/config{mount_mode}
      - {data_root / 'cache'}:/app/cache{mount_mode}
      - {data_root / 'plugins'}:/app/plugins
      - {scratch}:{scratch}
"""


def _named_volume_compose_text(
    *, image: str, container: str, prefix: str, port: int, scratch: Path
) -> str:
    return f"""services:
  droppedneedle:
    image: ${{DROPPEDNEEDLE_IMAGE:-{image}}}
    container_name: {container}
    environment:
      PUID: '1000'
      PGID: '1000'
      PORT: '8688'
      TZ: Europe/London
    ports:
      - '127.0.0.1:{port}:8688'
    volumes:
      - config:/app/config
      - cache:/app/cache
      - plugins:/app/plugins
      - {scratch}:{scratch}
volumes:
  config:
    name: {prefix}-config
  cache:
    name: {prefix}-cache
  plugins:
    name: {prefix}-plugins
"""


def _file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _data_tree_snapshot(root: Path) -> dict[str, str]:
    snapshot: dict[str, str] = {}
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root).as_posix()
        snapshot[relative] = (
            f"file:{path.stat().st_size}:{_file_sha256(path)}"
            if path.is_file()
            else "directory"
        )
    return snapshot


def _wait_for_target(url: str, timeout_seconds: float = 120.0) -> None:
    deadline = time.monotonic() + timeout_seconds
    while time.monotonic() < deadline:
        try:
            with urlopen(url, timeout=2) as response:  # noqa: S310 - scratch URL
                payload = json.loads(response.read())
                if response.status == 200 and payload.get("status") == "ok":
                    return
        except (URLError, TimeoutError, OSError, ValueError, TypeError):
            pass
        time.sleep(0.25)
    raise RuntimeError(f"Scratch target did not become ready: {url}")


def _run(
    arguments: Sequence[str],
    *,
    environment: dict[str, str] | None = None,
) -> str:
    result = subprocess.run(
        list(arguments),
        cwd=_REPOSITORY_ROOT,
        env=environment,
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"Command failed ({result.returncode}): {' '.join(arguments)}\n"
            f"{result.stderr[-4000:]}"
        )
    return result.stdout


def _wait_for_log(
    compose_command: list[str], message: str, timeout_seconds: float = 10.0
) -> str:
    deadline = time.monotonic() + timeout_seconds
    logs = ""
    while time.monotonic() < deadline:
        logs = _run([*compose_command, "logs", "--no-color", "droppedneedle"])
        if message in logs:
            return logs
        time.sleep(0.1)
    return logs


def _target_shape(database: Path) -> dict[str, Any]:
    with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as connection:
        tables = {
            str(row[0])
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table'"
            ).fetchall()
        }
        provenance = connection.execute(
            "SELECT source_kind, source_key, target_kind, target_id, source_revision, "
            "imported_at, migration_run_id FROM library_migration_provenance "
            "ORDER BY source_kind, source_key"
        ).fetchall()
        return {
            "integrity": str(
                connection.execute("PRAGMA integrity_check").fetchone()[0]
            ),
            "legacy_file_count": int(
                connection.execute("SELECT COUNT(*) FROM library_files").fetchone()[0]
            )
            if "library_files" in tables
            else 0,
            "target_catalog_present": "local_tracks" in tables,
            "local_track_count": int(
                connection.execute("SELECT COUNT(*) FROM local_tracks").fetchone()[0]
            ),
            "migration_marker_count": int(
                connection.execute(
                    "SELECT COUNT(*) FROM library_migration_markers "
                    "WHERE marker = 'legacy_catalog_import_complete'"
                ).fetchone()[0]
            ),
            "migration_run_count": int(
                connection.execute(
                    "SELECT COUNT(*) FROM library_migration_runs"
                ).fetchone()[0]
            ),
            "migration_provenance_count": len(provenance),
            "migration_provenance_sha256": hashlib.sha256(
                json.dumps(provenance, separators=(",", ":")).encode()
            ).hexdigest(),
            "favorite_count": int(
                connection.execute(
                    "SELECT COUNT(*) FROM library_user_favorites"
                ).fetchone()[0]
            ),
        }


def _container_target_shape(container: str) -> dict[str, Any]:
    script = """
import json
import sqlite3

with sqlite3.connect('/app/cache/library.db') as connection:
    tables = {
        str(row[0])
        for row in connection.execute(
            "SELECT name FROM sqlite_master WHERE type = 'table'"
        ).fetchall()
    }
    print(json.dumps({
        "integrity": str(connection.execute("PRAGMA integrity_check").fetchone()[0]),
        "legacy_file_count": int(
            connection.execute("SELECT COUNT(*) FROM library_files").fetchone()[0]
        ) if "library_files" in tables else 0,
        "target_catalog_present": "local_tracks" in tables,
        "local_track_count": int(
            connection.execute("SELECT COUNT(*) FROM local_tracks").fetchone()[0]
        ),
        "migration_marker_count": int(connection.execute(
            "SELECT COUNT(*) FROM library_migration_markers "
            "WHERE marker = 'legacy_catalog_import_complete'"
        ).fetchone()[0]),
    }))
"""
    return json.loads(
        _run(["docker", "exec", container, "python", "-c", script]).strip()
    )


def _container_legacy_shape(container: str) -> dict[str, Any]:
    script = """
import json
import sqlite3

with sqlite3.connect('/app/cache/library.db') as connection:
    tables = {
        str(row[0])
        for row in connection.execute(
            "SELECT name FROM sqlite_master WHERE type = 'table'"
        ).fetchall()
    }
    print(json.dumps({
        "integrity": str(connection.execute("PRAGMA integrity_check").fetchone()[0]),
        "legacy_file_count": int(
            connection.execute("SELECT COUNT(*) FROM library_files").fetchone()[0]
        ),
        "target_catalog_present": "local_tracks" in tables,
    }))
"""
    return json.loads(
        _run(["docker", "exec", container, "python", "-c", script]).strip()
    )


def _container_upgrade_storage_shape(container: str) -> dict[str, int]:
    script = """
import json
from pathlib import Path

root = Path('/app/cache/upgrade-backups')
print(json.dumps({
    "backup_count": len(list(root.iterdir())) if root.is_dir() else 0,
    "retained_working_copy_count": len(list(root.glob('*/working')))
    if root.is_dir() else 0,
}))
"""
    return json.loads(
        _run(["docker", "exec", container, "python", "-c", script]).strip()
    )


def _remove_migrated_favorite(container: str) -> dict[str, Any]:
    script = """
import asyncio
import json
import sqlite3
import threading
from pathlib import Path

from infrastructure.persistence.native_library_store import NativeLibraryStore

database = Path('/app/cache/library.db')

async def mutate():
    with sqlite3.connect(database) as connection:
        favorite = connection.execute(
            "SELECT source_key, target_id FROM library_migration_provenance "
            "WHERE source_kind = 'favorite' ORDER BY source_key LIMIT 1"
        ).fetchone()
        provenance_before = connection.execute(
            "SELECT source_kind, source_key, target_kind, target_id, source_revision, "
            "imported_at, migration_run_id FROM library_migration_provenance "
            "ORDER BY source_kind, source_key"
        ).fetchall()
    if favorite is None:
        raise RuntimeError('The migrated rehearsal catalog has no favorite provenance.')
    user_id, item_kind, _source_id = str(favorite[0]).split(':', 2)
    target_id = str(favorite[1])
    store = NativeLibraryStore(database, threading.Lock())
    await store.remove_target_favorite(user_id, item_kind, target_id)
    with sqlite3.connect(database) as connection:
        remaining = int(connection.execute(
            "SELECT COUNT(*) FROM library_user_favorites "
            "WHERE user_id = ? AND item_kind = ? AND item_id = ?",
            (user_id, item_kind, target_id),
        ).fetchone()[0])
        provenance_after = connection.execute(
            "SELECT source_kind, source_key, target_kind, target_id, source_revision, "
            "imported_at, migration_run_id FROM library_migration_provenance "
            "ORDER BY source_kind, source_key"
        ).fetchall()
    cutover = await store.validate_migrated_catalog()
    return {
        'favorite_removed': remaining == 0,
        'provenance_unchanged': provenance_after == provenance_before,
        'cutover_unresolved_references': cutover['unresolved_references'],
    }

print(json.dumps(asyncio.run(mutate())))
"""
    return json.loads(
        _run(["docker", "exec", container, "python", "-c", script]).strip()
    )


def _backup_count(data_root: Path) -> int:
    root = data_root / "cache" / "upgrade-backups"
    return len(list(root.iterdir())) if root.is_dir() else 0


def _retained_working_copy_count(data_root: Path) -> int:
    root = data_root / "cache" / "upgrade-backups"
    return len(list(root.glob("*/working"))) if root.is_dir() else 0


def run(output: Path) -> dict[str, Any]:
    started = time.perf_counter()
    source_identity = capture_source_identity(_REPOSITORY_ROOT)
    production_before = _docker_object(_PRODUCTION_CONTAINER)
    allow_unhealthy_production = (
        os.getenv("DROPPEDNEEDLE_REHEARSAL_ALLOW_UNHEALTHY_PRODUCTION", "").strip()
        == "1"
    )
    production_was_healthy = bool(
        production_before.get("State", {}).get("Running")
        and production_before.get("State", {}).get("Health", {}).get("Status")
        == "healthy"
    )
    if not allow_unhealthy_production and not production_was_healthy:
        raise RuntimeError("The deployed source must be healthy before the rehearsal.")
    production_image = str(production_before["Image"])
    run_id = uuid.uuid4().hex[:10]
    configured_source_image = os.getenv(
        "DROPPEDNEEDLE_REHEARSAL_SOURCE_IMAGE", ""
    ).strip()
    derived_source_reference: str | None = None
    if configured_source_image:
        source_image = configured_source_image
        _docker_object(source_image)
    else:
        source_image = ""
    image = f"droppedneedle-automatic-upgrade-{run_id}:target"
    projects: list[tuple[list[str], str]] = []
    remapped_paths: list[Path] = []
    named_volumes: set[str] = set()
    target_environment = os.environ.copy()
    target_environment.pop("DROPPEDNEEDLE_IMAGE", None)

    with tempfile.TemporaryDirectory(
        prefix="feedback-fixes-auto-upgrade-"
    ) as directory:
        scratch = Path(directory)
        scratch.chmod(0o755)
        upgrade_data, database = _seed_source(scratch / "upgrade")
        upgrade_config = upgrade_data / "config" / "config.json"
        upgrade_settings = json.loads(upgrade_config.read_text(encoding="utf-8"))
        upgrade_settings["library_scan_schedule"] = {
            "scan_frequency": "daily",
            "daily_scan_time": "23:59",
        }
        upgrade_config.write_text(json.dumps(upgrade_settings), encoding="utf-8")
        upgrade_compose = scratch / "upgrade-compose.yml"
        upgrade_port = _free_port()
        upgrade_container = f"feedback-fixes-auto-upgrade-{run_id}"
        upgrade_compose.write_text(
            _compose_text(
                image=image,
                container=upgrade_container,
                data_root=upgrade_data,
                scratch=scratch,
                port=upgrade_port,
            ),
            encoding="utf-8",
        )
        upgrade_project = f"feedback-fixes-auto-upgrade-{run_id}"
        upgrade_command = [
            "docker",
            "compose",
            "-f",
            str(upgrade_compose),
            "-p",
            upgrade_project,
        ]
        projects.append((upgrade_command, upgrade_container))

        fresh_root = scratch / "fresh"
        fresh_data = fresh_root / "data"
        for name in ("config", "cache", "plugins"):
            (fresh_data / name).mkdir(parents=True, exist_ok=True)
        fresh_compose = scratch / "fresh-compose.yml"
        fresh_port = _free_port()
        fresh_container = f"feedback-fixes-auto-fresh-{run_id}"
        fresh_compose.write_text(
            _compose_text(
                image=image,
                container=fresh_container,
                data_root=fresh_data,
                scratch=scratch,
                port=fresh_port,
            ),
            encoding="utf-8",
        )
        fresh_project = f"feedback-fixes-auto-fresh-{run_id}"
        fresh_command = [
            "docker",
            "compose",
            "-f",
            str(fresh_compose),
            "-p",
            fresh_project,
        ]
        projects.append((fresh_command, fresh_container))

        try:
            build_started = time.perf_counter()
            _run(
                [*upgrade_command, "build", "droppedneedle"],
                environment=target_environment,
            )
            build_seconds = time.perf_counter() - build_started
            target_image = str(_docker_object(image)["Id"])

            if not configured_source_image:
                derived_source_reference, source_image = _legacy_source_image(
                    production_image, run_id
                )

            source_environment = os.environ.copy()
            source_environment["DROPPEDNEEDLE_IMAGE"] = source_image
            _run(
                [*upgrade_command, "up", "-d", "--no-build", "droppedneedle"],
                environment=source_environment,
            )
            _wait_for_health(f"http://127.0.0.1:{upgrade_port}/health", 60)
            source_shape = _database_shape(database)
            source_container_image = str(_docker_object(upgrade_container)["Image"])
            if source_container_image != source_image:
                raise RuntimeError("The source rehearsal did not run the old image.")
            if source_shape["target_catalog_present"]:
                raise RuntimeError("The source rehearsal was already target-shaped.")

            upgrade_started = time.perf_counter()
            _run(
                [*upgrade_command, "up", "-d", "--no-build", "droppedneedle"],
                environment=target_environment,
            )
            _wait_for_target(f"http://127.0.0.1:{upgrade_port}/health", 300)
            upgrade_seconds = time.perf_counter() - upgrade_started
            upgraded_container_image = str(_docker_object(upgrade_container)["Image"])
            if upgraded_container_image != target_image:
                raise RuntimeError("The normal update did not replace the old image.")
            completion_message = "Library upgrade complete. DroppedNeedle is ready."
            logs = _wait_for_log(upgrade_command, completion_message)
            if completion_message not in logs:
                raise RuntimeError(
                    "The normal update did not report a completed upgrade."
                )
            time.sleep(1.25)
            logs = _run([*upgrade_command, "logs", "--no-color", "droppedneedle"])
            if "Target scan supervisor iteration failed" in logs:
                raise RuntimeError(
                    "The target scan supervisor failed after the normal update."
                )
            upgraded_shape = _target_shape(database)
            if upgraded_shape["migration_marker_count"] != 1:
                raise RuntimeError(
                    "The normal update did not create its migration marker."
                )
            backups_after_upgrade = _backup_count(upgrade_data)
            retained_working_copies = _retained_working_copy_count(upgrade_data)
            if retained_working_copies != 0:
                raise RuntimeError("The completed upgrade retained its working copy.")
            baked_source_revision = _run(
                [
                    "docker",
                    "exec",
                    upgrade_container,
                    "cat",
                    "/app/.droppedneedle-source-revision",
                ]
            ).strip()
            if len(baked_source_revision) != 64:
                raise RuntimeError("The target image has no baked source fingerprint.")

            mutation = _remove_migrated_favorite(upgrade_container)
            if (
                not mutation["favorite_removed"]
                or not mutation["provenance_unchanged"]
                or mutation["cutover_unresolved_references"] < 1
            ):
                raise RuntimeError(
                    "The rehearsal did not create a normal provenance-backed mutation."
                )
            mutated_shape = _target_shape(database)
            if (
                mutated_shape["favorite_count"] != upgraded_shape["favorite_count"] - 1
                or mutated_shape["migration_run_count"]
                != upgraded_shape["migration_run_count"]
                or mutated_shape["migration_provenance_sha256"]
                != upgraded_shape["migration_provenance_sha256"]
            ):
                raise RuntimeError(
                    "The post-admission mutation changed migration audit evidence."
                )

            restart_started = time.perf_counter()
            _run([*upgrade_command, "restart", "droppedneedle"])
            _wait_for_target(f"http://127.0.0.1:{upgrade_port}/health", 120)
            restart_seconds = time.perf_counter() - restart_started
            restarted_shape = _target_shape(database)
            backups_after_restart = _backup_count(upgrade_data)
            if restarted_shape != mutated_shape or backups_after_restart != 1:
                raise RuntimeError("A normal restart repeated or changed the upgrade.")

            fresh_started = time.perf_counter()
            _run(
                [*fresh_command, "up", "-d", "--no-build", "droppedneedle"],
                environment=target_environment,
            )
            _wait_for_target(f"http://127.0.0.1:{fresh_port}/health", 120)
            fresh_seconds = time.perf_counter() - fresh_started
            fresh_shape = _target_shape(fresh_data / "cache" / "library.db")
            if fresh_shape["local_track_count"] != 0:
                raise RuntimeError(
                    "The fresh installation did not start with an empty catalog."
                )

            nonroot_root = scratch / "nonroot"
            nonroot_data, nonroot_database = _seed_source(nonroot_root)
            nonroot_compose = scratch / "nonroot-compose.yml"
            nonroot_port = _free_port()
            nonroot_container = f"feedback-fixes-auto-nonroot-{run_id}"
            nonroot_compose.write_text(
                _compose_text(
                    image=image,
                    container=nonroot_container,
                    data_root=nonroot_data,
                    scratch=scratch,
                    port=nonroot_port,
                    container_port=8799,
                    user=f"{os.getuid()}:{os.getgid()}",
                ),
                encoding="utf-8",
            )
            nonroot_command = [
                "docker",
                "compose",
                "-f",
                str(nonroot_compose),
                "-p",
                f"feedback-fixes-auto-nonroot-{run_id}",
            ]
            projects.append((nonroot_command, nonroot_container))
            _run(
                [*nonroot_command, "up", "-d", "--no-build", "droppedneedle"],
                environment=target_environment,
            )
            _wait_for_target(f"http://127.0.0.1:{nonroot_port}/health", 120)
            nonroot_shape = _target_shape(nonroot_database)

            unraid_root = scratch / "unraid"
            unraid_data, _unraid_database = _seed_source(unraid_root)
            remapped_uid = 99
            remapped_gid = 100
            _run(
                [
                    "docker",
                    "run",
                    "--rm",
                    "--entrypoint",
                    "sh",
                    "-v",
                    f"{unraid_data}:/data",
                    image,
                    "-c",
                    f"chown -R {remapped_uid}:{remapped_gid} /data",
                ]
            )
            remapped_paths.append(unraid_data)
            unraid_compose = scratch / "unraid-compose.yml"
            unraid_port = _free_port()
            unraid_container = f"feedback-fixes-auto-unraid-{run_id}"
            unraid_compose.write_text(
                _compose_text(
                    image=image,
                    container=unraid_container,
                    data_root=unraid_data,
                    scratch=scratch,
                    port=unraid_port,
                    puid=remapped_uid,
                    pgid=remapped_gid,
                ),
                encoding="utf-8",
            )
            unraid_command = [
                "docker",
                "compose",
                "-f",
                str(unraid_compose),
                "-p",
                f"feedback-fixes-auto-unraid-{run_id}",
            ]
            projects.append((unraid_command, unraid_container))
            _run(
                [*unraid_command, "up", "-d", "--no-build", "droppedneedle"],
                environment=target_environment,
            )
            try:
                _wait_for_target(f"http://127.0.0.1:{unraid_port}/health", 120)
            except RuntimeError as error:
                logs = _run([*unraid_command, "logs", "--no-color", "droppedneedle"])
                raise RuntimeError(
                    f"{error}\nUnraid-style logs:\n{logs[-4000:]}"
                ) from error
            unraid_shape = _container_target_shape(unraid_container)

            truenas_root = scratch / "truenas"
            truenas_data, _truenas_database = _seed_source(truenas_root)
            truenas_uid = 568
            truenas_gid = 568
            _run(
                [
                    "docker",
                    "run",
                    "--rm",
                    "--entrypoint",
                    "sh",
                    "-v",
                    f"{truenas_data}:/data",
                    image,
                    "-c",
                    f"chown -R {truenas_uid}:{truenas_gid} /data",
                ]
            )
            remapped_paths.append(truenas_data)
            truenas_compose = scratch / "truenas-compose.yml"
            truenas_port = _free_port()
            truenas_container = f"feedback-fixes-auto-truenas-{run_id}"
            truenas_compose.write_text(
                _compose_text(
                    image=image,
                    container=truenas_container,
                    data_root=truenas_data,
                    scratch=scratch,
                    port=truenas_port,
                    user=f"{truenas_uid}:{truenas_gid}",
                    container_read_only=True,
                ),
                encoding="utf-8",
            )
            truenas_command = [
                "docker",
                "compose",
                "-f",
                str(truenas_compose),
                "-p",
                f"feedback-fixes-auto-truenas-{run_id}",
            ]
            projects.append((truenas_command, truenas_container))
            _run(
                [*truenas_command, "up", "-d", "--no-build", "droppedneedle"],
                environment=target_environment,
            )
            try:
                _wait_for_target(f"http://127.0.0.1:{truenas_port}/health", 120)
            except RuntimeError as error:
                logs = _run([*truenas_command, "logs", "--no-color", "droppedneedle"])
                raise RuntimeError(
                    f"{error}\nTrueNAS-style logs:\n{logs[-4000:]}"
                ) from error
            truenas_shape = _container_target_shape(truenas_container)

            named_prefix = f"feedback-fixes-auto-named-{run_id}"
            named_seed_data, _named_seed_database = _seed_source(scratch / "named-seed")
            for volume_name in ("config", "cache", "plugins"):
                qualified_name = f"{named_prefix}-{volume_name}"
                named_volumes.add(qualified_name)
                _run(["docker", "volume", "create", qualified_name])
            _run(
                [
                    "docker",
                    "run",
                    "--rm",
                    "--entrypoint",
                    "sh",
                    "-v",
                    f"{named_seed_data / 'config'}:/source-config:ro",
                    "-v",
                    f"{named_seed_data / 'cache'}:/source-cache:ro",
                    "-v",
                    f"{named_seed_data / 'plugins'}:/source-plugins:ro",
                    "-v",
                    f"{named_prefix}-config:/target-config",
                    "-v",
                    f"{named_prefix}-cache:/target-cache",
                    "-v",
                    f"{named_prefix}-plugins:/target-plugins",
                    image,
                    "-c",
                    "cp -a /source-config/. /target-config/ && "
                    "cp -a /source-cache/. /target-cache/ && "
                    "cp -a /source-plugins/. /target-plugins/",
                ]
            )
            named_compose = scratch / "named-compose.yml"
            named_port = _free_port()
            named_container = f"feedback-fixes-auto-named-{run_id}"
            named_compose.write_text(
                _named_volume_compose_text(
                    image=image,
                    container=named_container,
                    prefix=named_prefix,
                    port=named_port,
                    scratch=scratch,
                ),
                encoding="utf-8",
            )
            named_command = [
                "docker",
                "compose",
                "-f",
                str(named_compose),
                "-p",
                named_prefix,
            ]
            projects.append((named_command, named_container))
            _run(
                [*named_command, "up", "-d", "--no-build", "droppedneedle"],
                environment=source_environment,
            )
            _wait_for_health(f"http://127.0.0.1:{named_port}/health", 60)
            named_source_shape = _container_legacy_shape(named_container)
            if named_source_shape["target_catalog_present"]:
                raise RuntimeError("The named-volume source was already target-shaped.")
            _run(
                [*named_command, "up", "-d", "--no-build", "droppedneedle"],
                environment=target_environment,
            )
            _wait_for_target(f"http://127.0.0.1:{named_port}/health", 120)
            named_shape = _container_target_shape(named_container)
            named_storage = _container_upgrade_storage_shape(named_container)
            _run([*named_command, "restart", "droppedneedle"])
            _wait_for_target(f"http://127.0.0.1:{named_port}/health", 120)
            named_restart_shape = _container_target_shape(named_container)
            named_restart_storage = _container_upgrade_storage_shape(named_container)
            if (
                named_restart_shape != named_shape
                or named_storage != named_restart_storage
                or named_storage["backup_count"] != 1
                or named_storage["retained_working_copy_count"] != 0
            ):
                raise RuntimeError(
                    "The named-volume migration did not persist cleanly across restart."
                )

            readonly_root = scratch / "readonly"
            readonly_data, _readonly_database = _seed_source(readonly_root)
            readonly_before = _data_tree_snapshot(readonly_data)
            readonly_compose = scratch / "readonly-compose.yml"
            readonly_container = f"feedback-fixes-auto-readonly-{run_id}"
            readonly_compose.write_text(
                _compose_text(
                    image=image,
                    container=readonly_container,
                    data_root=readonly_data,
                    scratch=scratch,
                    port=_free_port(),
                    read_only=True,
                ),
                encoding="utf-8",
            )
            readonly_command = [
                "docker",
                "compose",
                "-f",
                str(readonly_compose),
                "-p",
                f"feedback-fixes-auto-readonly-{run_id}",
            ]
            projects.append((readonly_command, readonly_container))
            _run(
                [*readonly_command, "up", "-d", "--no-build", "droppedneedle"],
                environment=target_environment,
            )
            time.sleep(2)
            readonly_logs = _run(
                [*readonly_command, "logs", "--no-color", "droppedneedle"]
            )
            readonly_after = _data_tree_snapshot(readonly_data)
            readonly_refused_unchanged = (
                "FATAL: /app/cache is not writable" in readonly_logs
                or "FATAL: /app/config is not writable" in readonly_logs
            ) and readonly_before == readonly_after
            if not readonly_refused_unchanged:
                raise RuntimeError(
                    "The read-only deployment was not refused without data changes."
                )

            production_after = _docker_object(_PRODUCTION_CONTAINER)
            production_identity_unchanged = (
                str(production_after["Id"]) == str(production_before["Id"])
                and str(production_after["Image"]) == production_image
            )
            production_unchanged = production_identity_unchanged and (
                allow_unhealthy_production
                or (
                    bool(production_after.get("State", {}).get("Running"))
                    and production_after.get("State", {})
                    .get("Health", {})
                    .get("Status")
                    == "healthy"
                )
            )
            if not production_unchanged:
                raise RuntimeError(
                    "The automatic-upgrade rehearsal changed production."
                )
            report = {
                "schema": "feedback-fixes-automatic-upgrade-rehearsal-v1",
                "passed": True,
                "normal_update_requires_special_command": False,
                "production_unchanged": True,
                "production_was_healthy": production_was_healthy,
                "unhealthy_production_allowed": allow_unhealthy_production,
                "production_container_id": str(production_before["Id"]),
                "production_image_id": production_image,
                "target_image_id": target_image,
                "source_identity": source_identity,
                "baked_source_revision": baked_source_revision,
                "source_container_image_id": source_container_image,
                "upgraded_container_image_id": upgraded_container_image,
                "source_database": source_shape,
                "upgraded_database": upgraded_shape,
                "post_admission_mutation": mutation,
                "mutated_database": mutated_shape,
                "restarted_database": restarted_shape,
                "fresh_database": fresh_shape,
                "deployment_matrix": {
                    "bind_mount_root_entrypoint": upgraded_shape,
                    "bind_mount_nonroot_custom_container_port": {
                        "container_port": 8799,
                        "database": nonroot_shape,
                    },
                    "unraid_style_puid_pgid": unraid_shape,
                    "truenas_style_nonroot_readonly_rootfs": truenas_shape,
                    "docker_named_volumes": {
                        "source": named_source_shape,
                        "upgraded": named_shape,
                        "restarted": named_restart_shape,
                        "storage": named_storage,
                    },
                    "readonly_refused_unchanged": readonly_refused_unchanged,
                },
                "backup_count_after_upgrade": backups_after_upgrade,
                "backup_count_after_restart": backups_after_restart,
                "retained_working_copy_count": retained_working_copies,
                "timings_seconds": {
                    "build": build_seconds,
                    "automatic_upgrade_and_start": upgrade_seconds,
                    "already_upgraded_restart": restart_seconds,
                    "fresh_install_start": fresh_seconds,
                    "total": time.perf_counter() - started,
                },
            }
        finally:
            for command, _container in reversed(projects):
                subprocess.run(
                    [*command, "down", "--volumes", "--remove-orphans"],
                    cwd=_REPOSITORY_ROOT,
                    check=False,
                    capture_output=True,
                    text=True,
                )
            for volume in sorted(named_volumes):
                subprocess.run(
                    ["docker", "volume", "rm", "-f", volume],
                    cwd=_REPOSITORY_ROOT,
                    check=False,
                    capture_output=True,
                    text=True,
                )
            for path in remapped_paths:
                subprocess.run(
                    [
                        "docker",
                        "run",
                        "--rm",
                        "--entrypoint",
                        "sh",
                        "-v",
                        f"{path}:/data",
                        image,
                        "-c",
                        f"chown -R {os.getuid()}:{os.getgid()} /data",
                    ],
                    cwd=_REPOSITORY_ROOT,
                    check=False,
                    capture_output=True,
                    text=True,
                )
            subprocess.run(
                ["docker", "image", "rm", "-f", image],
                cwd=_REPOSITORY_ROOT,
                check=False,
                capture_output=True,
                text=True,
            )
            if derived_source_reference is not None:
                subprocess.run(
                    ["docker", "image", "rm", "-f", derived_source_reference],
                    cwd=_REPOSITORY_ROOT,
                    check=False,
                    capture_output=True,
                    text=True,
                )
            shutil.rmtree(scratch / "unused", ignore_errors=True)

    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return report


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    report = run(arguments.output)
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
