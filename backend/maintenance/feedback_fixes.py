"""Staged, fail-closed Feedback Fixes production replacement and rollback CLI.

The command is inert until an operator runs a stage with the unique authorization
challenge written by ``prepare``. It never combines stop, migration, build, and start.
"""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import os
import sqlite3
import subprocess
import sys
import time
import uuid
from pathlib import Path
from typing import Any, Callable, Sequence
from urllib.error import URLError
from urllib.request import urlopen

import msgspec

from infrastructure.persistence.maintenance_manifest import (
    MaintenanceManifestError,
    capture_complete_manifest,
    capture_source_identity,
    derive_managed_assets,
    restore_complete_manifest_in_place,
    validate_complete_manifest,
)


SERVICE_NAME = "droppedneedle"
STATE_FORMAT_VERSION = 2
_SOURCE_REVISION_LABEL = "org.droppedneedle.source-revision"
_AUTOMATIC_TARGET_COMMAND = [
    "python",
    "-m",
    "maintenance.automatic_upgrade",
    "--start-target",
]
_ACTIVE_WORK_QUERIES = (
    ("legacy_scan", "scan_state", "status = 'scanning'"),
    ("drop_import", "drop_import_jobs", "status = 'processing'"),
    (
        "download_import",
        "download_tasks",
        "status IN ('queued','downloading','processing')",
    ),
    (
        "target_scan",
        "library_scan_runs",
        "state IN ('queued','discovering','indexing','reconciling','pausing','stopping')",
    ),
    (
        "identification",
        "library_identification_jobs",
        "state IN ('queued','running')",
    ),
    (
        "library_operation",
        "library_operation_jobs",
        "state IN ('queued','running','ready')",
    ),
)


class MaintenanceStageError(RuntimeError):
    """A production maintenance stage is out of order or unsafe."""


CommandRunner = Callable[[Sequence[str], Path | None, dict[str, str] | None], str]


def _run_command(
    command: Sequence[str],
    cwd: Path | None = None,
    environment: dict[str, str] | None = None,
) -> str:
    try:
        result = subprocess.run(
            list(command),
            cwd=cwd,
            env=environment,
            check=True,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError) as exc:
        stderr = (
            exc.stderr.strip() if isinstance(exc, subprocess.CalledProcessError) else ""
        )
        detail = f" ({stderr[-500:]})" if stderr else ""
        raise MaintenanceStageError(
            f"Command failed: {' '.join(command)}{detail}"
        ) from exc
    return result.stdout


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _read_state(path: Path) -> dict[str, Any]:
    try:
        state = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise MaintenanceStageError(
            "The maintenance state is missing or unreadable."
        ) from exc
    if (
        not isinstance(state, dict)
        or state.get("format_version") != STATE_FORMAT_VERSION
    ):
        raise MaintenanceStageError("The maintenance state format is unsupported.")
    return state


def _write_state(path: Path, state: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp")
    temporary.write_text(
        json.dumps(state, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    temporary.chmod(0o600)
    os.replace(temporary, path)


def _record(state: dict[str, Any], event: str, **evidence: object) -> None:
    state.setdefault("events", []).append(
        {"event": event, "recorded_at": time.time(), **evidence}
    )


def _require_stage(state: dict[str, Any], *allowed: str) -> None:
    if state.get("stage") not in allowed:
        raise MaintenanceStageError(
            f"Stage {state.get('stage')!r} cannot perform this operation; expected "
            + ", ".join(allowed)
            + "."
        )


def _require_authorization(state: dict[str, Any], supplied: str | None) -> None:
    expected = state.get("authorization_challenge")
    if not isinstance(expected, str) or supplied != expected:
        raise MaintenanceStageError(
            "The unique offline-replacement authorization challenge does not match."
        )


def _docker_inspect(
    target: str, *, runner: CommandRunner, image: bool = False
) -> dict[str, Any]:
    command = (
        ["docker", "image", "inspect", target]
        if image
        else ["docker", "inspect", target]
    )
    try:
        payload = json.loads(runner(command, None, None))
    except (json.JSONDecodeError, TypeError) as exc:
        raise MaintenanceStageError(
            "Docker returned unreadable inspection metadata."
        ) from exc
    if not isinstance(payload, list) or not payload or not isinstance(payload[0], dict):
        raise MaintenanceStageError("Docker inspection returned no object.")
    return payload[0]


def _container_running(inspect: dict[str, Any]) -> bool:
    state = inspect.get("State")
    return bool(isinstance(state, dict) and state.get("Running"))


def _container_shape(inspect: dict[str, Any]) -> dict[str, Any]:
    config = inspect.get("Config") if isinstance(inspect.get("Config"), dict) else {}
    mounts = inspect.get("Mounts") if isinstance(inspect.get("Mounts"), list) else []
    network_settings = (
        inspect.get("NetworkSettings")
        if isinstance(inspect.get("NetworkSettings"), dict)
        else {}
    )
    networks = (
        network_settings.get("Networks")
        if isinstance(network_settings.get("Networks"), dict)
        else {}
    )
    return {
        "environment_sha256": hashlib.sha256(
            json.dumps(sorted(config.get("Env") or []), separators=(",", ":")).encode()
        ).hexdigest(),
        "mounts": sorted(
            [
                {
                    "destination": str(item.get("Destination") or ""),
                    "source": str(item.get("Source") or ""),
                    "type": str(item.get("Type") or ""),
                    "rw": bool(item.get("RW")),
                }
                for item in mounts
                if isinstance(item, dict)
            ],
            key=lambda item: item["destination"],
        ),
        "networks": sorted(str(name) for name in networks),
    }


def _require_source_identity(state: dict[str, Any], message: str) -> None:
    if (
        capture_source_identity(Path(state["repository_root"]))
        != state["source_identity"]
    ):
        raise MaintenanceStageError(message)


def _runtime(state: dict[str, Any]) -> dict[str, Any]:
    runtime = state.get("runtime")
    if not isinstance(runtime, dict):
        raise MaintenanceStageError("The maintenance runtime configuration is missing.")
    return runtime


def _compose_command(state: dict[str, Any], *arguments: str) -> list[str]:
    runtime = _runtime(state)
    command = ["docker", "compose", "-f", runtime["compose_file"]]
    project = runtime.get("compose_project")
    if isinstance(project, str) and project:
        command.extend(["-p", project])
    command.extend(arguments)
    return command


def _service_name(state: dict[str, Any]) -> str:
    return str(_runtime(state)["service_name"])


def _container_name(state: dict[str, Any]) -> str:
    return str(_runtime(state)["container_name"])


def _health_url(state: dict[str, Any]) -> str:
    return str(_runtime(state)["health_url"])


def _target_image_metadata(
    image: dict[str, Any], state: dict[str, Any]
) -> dict[str, Any]:
    image_id = str(image.get("Id") or "")
    config = image.get("Config") if isinstance(image.get("Config"), dict) else {}
    labels = config.get("Labels") if isinstance(config.get("Labels"), dict) else {}
    entrypoint = [str(value) for value in config.get("Entrypoint") or []]
    command = [str(value) for value in config.get("Cmd") or []]
    if not image_id.startswith("sha256:"):
        raise MaintenanceStageError("The target build has no immutable image ID.")
    if (
        labels.get(_SOURCE_REVISION_LABEL)
        != state["source_identity"]["application_revision"]
    ):
        raise MaintenanceStageError(
            "The target image is not bound to the prepared application revision."
        )
    runs_target_directly = any("target_main:app" in value for value in command)
    runs_target_upgrade = command == _AUTOMATIC_TARGET_COMMAND
    if not runs_target_directly and not runs_target_upgrade:
        raise MaintenanceStageError(
            "The target image does not run the target application launcher."
        )
    return {
        "image_id": image_id,
        "entrypoint": entrypoint,
        "command": command,
        "source_revision": labels[_SOURCE_REVISION_LABEL],
    }


def _database_writer_probe(database_path: Path) -> tuple[bool, str | None]:
    try:
        with sqlite3.connect(database_path, timeout=0) as connection:
            connection.execute("BEGIN IMMEDIATE")
            connection.rollback()
        return True, None
    except sqlite3.OperationalError as exc:
        return False, str(exc)


def _database_holders(database_path: Path) -> list[int]:
    targets = {
        str(database_path.resolve()),
        str(Path(str(database_path) + "-wal").resolve()),
        str(Path(str(database_path) + "-shm").resolve()),
    }
    holders: set[int] = set()
    proc = Path("/proc")
    for process in proc.iterdir():
        if not process.name.isdigit() or int(process.name) == os.getpid():
            continue
        file_descriptors = process / "fd"
        try:
            descriptors = list(file_descriptors.iterdir())
        except (FileNotFoundError, PermissionError, OSError):
            continue
        for descriptor in descriptors:
            try:
                if os.readlink(descriptor) in targets:
                    holders.add(int(process.name))
                    break
            except (FileNotFoundError, PermissionError, OSError):
                continue
    return sorted(holders)


def closed_writer_evidence(database_path: Path) -> dict[str, Any]:
    holders = _database_holders(database_path)
    lock_available, writer_error = _database_writer_probe(database_path)
    holder_processes = []
    for pid in holders:
        try:
            name = (
                (Path("/proc") / str(pid) / "comm").read_text(encoding="utf-8").strip()
            )
        except (FileNotFoundError, PermissionError, OSError, UnicodeError):
            name = "unknown"
        holder_processes.append({"pid": pid, "name": name})
    return {
        "database_writer_lock_available": lock_available,
        "database_writer_error": writer_error,
        "database_holder_pids": holders,
        "database_holder_processes": holder_processes,
        "closed": lock_available and not holders,
    }


def _wait_for_closed_writer(
    database_path: Path, timeout_seconds: float = 30.0
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout_seconds
    evidence = closed_writer_evidence(database_path)
    while not evidence["closed"] and time.monotonic() < deadline:
        time.sleep(0.1)
        evidence = closed_writer_evidence(database_path)
    return evidence


def active_work_evidence(database_path: Path) -> dict[str, int]:
    counts = {kind: 0 for kind, _, _ in _ACTIVE_WORK_QUERIES}
    with sqlite3.connect(f"file:{database_path}?mode=ro", uri=True) as connection:
        tables = {
            str(row[0])
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table'"
            ).fetchall()
        }
        for kind, table, predicate in _ACTIVE_WORK_QUERIES:
            if table in tables:
                counts[kind] = int(
                    connection.execute(
                        f'SELECT COUNT(*) FROM "{table}" WHERE {predicate}'
                    ).fetchone()[0]
                )
    return counts


def _prior_application(
    *,
    container: dict[str, Any],
    image: dict[str, Any],
    runtime: dict[str, Any],
    maintenance_id: str,
) -> dict[str, Any]:
    image_id = str(image.get("Id") or "")
    if not image_id.startswith("sha256:"):
        raise MaintenanceStageError(
            "The deployed application image has no immutable ID."
        )
    short = image_id.removeprefix("sha256:")[:16]
    image_config = image.get("Config") if isinstance(image.get("Config"), dict) else {}
    container_config = (
        container.get("Config") if isinstance(container.get("Config"), dict) else {}
    )
    shape = _container_shape(container)
    state = {"runtime": runtime}
    image_prefix = str(runtime["image_tag_prefix"])
    return {
        "container_id": str(container.get("Id") or ""),
        "container_name": str(container.get("Name") or "").lstrip("/"),
        "image_id": image_id,
        "image_reference": str(container_config.get("Image") or ""),
        "rollback_image_reference": (
            f"{image_prefix}:feedback-fixes-rollback-" f"{maintenance_id[:8]}-{short}"
        ),
        "repo_digests": sorted(str(value) for value in image.get("RepoDigests") or []),
        "entrypoint": [str(value) for value in image_config.get("Entrypoint") or []],
        "command": [str(value) for value in image_config.get("Cmd") or []],
        "launch_command": _compose_command(
            state, "up", "-d", "--no-build", str(runtime["service_name"])
        ),
        "compose_config_sha256": _sha256(Path(runtime["compose_file"])),
        "container_environment_sha256": shape["environment_sha256"],
        "mounts": shape["mounts"],
        "networks": shape["networks"],
        "image_created": str(image.get("Created") or ""),
    }


def prepare(
    *,
    state_path: Path,
    repository_root: Path,
    data_root: Path,
    manifest_root: Path,
    compose_file: Path | None = None,
    compose_project: str | None = None,
    service_name: str = SERVICE_NAME,
    container_name: str | None = None,
    target_build_reference: str = "droppedneedle:local",
    image_tag_prefix: str = "droppedneedle",
    health_url: str = "http://127.0.0.1:8688/health",
    runner: CommandRunner = _run_command,
) -> dict[str, Any]:
    """Record immutable rollback and exact source evidence without changing Docker."""

    if state_path.exists():
        raise MaintenanceStageError("The maintenance state already exists.")
    repository = repository_root.resolve(strict=True)
    data = data_root.resolve(strict=True)
    resolved_manifest = manifest_root.resolve()
    resolved_state = state_path.resolve()
    if resolved_manifest.is_relative_to(data) or data.is_relative_to(resolved_manifest):
        raise MaintenanceStageError(
            "The maintenance manifest must be outside the application data root."
        )
    if resolved_state.is_relative_to(data) or resolved_state.is_relative_to(
        resolved_manifest
    ):
        raise MaintenanceStageError(
            "The maintenance state must be outside the data and manifest roots."
        )
    database = data / "cache/library.db"
    config = data / "config/config.json"
    environment = data / "config/.env"
    resolved_compose = (compose_file or repository / "docker-compose.yml").resolve(
        strict=True
    )
    runtime = {
        "compose_file": str(resolved_compose),
        "compose_project": compose_project,
        "service_name": service_name,
        "container_name": container_name or service_name,
        "target_build_reference": target_build_reference,
        "image_tag_prefix": image_tag_prefix,
        "health_url": health_url,
    }
    for required in (database, config, environment, resolved_compose):
        if not required.is_file():
            raise MaintenanceStageError(
                f"Required maintenance input is missing: {required}"
            )
    container = _docker_inspect(str(runtime["container_name"]), runner=runner)
    if not _container_running(container):
        raise MaintenanceStageError(
            "The source container is not running during prepare."
        )
    image = _docker_inspect(
        str(container.get("Image") or ""), runner=runner, image=True
    )
    active = active_work_evidence(database)
    if any(active.values()):
        raise MaintenanceStageError(
            "A scan, identification, import, repair, or reorganization workload is active."
        )
    source_identity = capture_source_identity(repository)
    maintenance_id = str(uuid.uuid4())
    prior = _prior_application(
        container=container,
        image=image,
        runtime=runtime,
        maintenance_id=maintenance_id,
    )
    challenge = f"AUTHORIZE-{uuid.uuid4()}"
    state: dict[str, Any] = {
        "format_version": STATE_FORMAT_VERSION,
        "maintenance_id": maintenance_id,
        "stage": "prepared",
        "authorization_challenge": challenge,
        "repository_root": str(repository),
        "data_root": str(data),
        "database_path": str(database),
        "config_path": str(config),
        "environment_path": str(environment),
        "manifest_root": str(resolved_manifest),
        "runtime": runtime,
        "source_identity": source_identity,
        "prior_application": prior,
        "active_work": active,
        "events": [],
    }
    _record(
        state,
        "prepared",
        source_revision=source_identity["application_revision"],
        prior_image_id=prior["image_id"],
        active_work=active,
    )
    _write_state(state_path, state)
    return state


def stop(
    *, state_path: Path, authorization: str, runner: CommandRunner = _run_command
) -> dict[str, Any]:
    """Stop only the application after both immutable images have been retained."""

    state = _read_state(state_path)
    _require_stage(state, "built")
    _require_authorization(state, authorization)
    repository = Path(state["repository_root"])
    prior = state["prior_application"]
    _require_source_identity(
        state, "The application worktree changed before the source stop."
    )
    active = active_work_evidence(Path(state["database_path"]))
    if any(active.values()):
        raise MaintenanceStageError(
            "A scan, identification, import, repair, or reorganization workload "
            "started after prepare."
        )
    runner(_compose_command(state, "down", "--remove-orphans"), repository, None)
    try:
        container = _docker_inspect(_container_name(state), runner=runner)
    except MaintenanceStageError:
        container = {}
    if container and (
        _container_running(container) or container.get("Id") != prior["container_id"]
    ):
        raise MaintenanceStageError(
            "The prepared source container did not stop cleanly."
        )
    pinned = _docker_inspect(
        prior["rollback_image_reference"], runner=runner, image=True
    )
    if pinned.get("Id") != prior["image_id"]:
        raise MaintenanceStageError("The immutable rollback image was not retained.")
    writer = _wait_for_closed_writer(Path(state["database_path"]))
    if not writer["closed"]:
        raise MaintenanceStageError(
            "A database writer remains after container stop: "
            + json.dumps(writer, sort_keys=True)
        )
    state["stage"] = "stopped"
    _record(
        state,
        "stopped",
        writer=writer,
        active_work=active,
        source_container_removed=not container,
        rollback_image_retained=True,
    )
    _write_state(state_path, state)
    return state


def capture(
    *, state_path: Path, authorization: str, runner: CommandRunner = _run_command
) -> dict[str, Any]:
    """Capture and validate a closed-source recovery unit."""

    state = _read_state(state_path)
    _require_stage(state, "stopped")
    _require_authorization(state, authorization)
    prior = state["prior_application"]
    try:
        container = _docker_inspect(_container_name(state), runner=runner)
    except MaintenanceStageError:
        container = {}
    if container and (
        _container_running(container) or container.get("Id") != prior["container_id"]
    ):
        raise MaintenanceStageError("The stopped source container identity changed.")
    pinned = _docker_inspect(
        prior["rollback_image_reference"], runner=runner, image=True
    )
    if pinned.get("Id") != prior["image_id"]:
        raise MaintenanceStageError(
            "The recorded rollback image is no longer available."
        )
    writer = closed_writer_evidence(Path(state["database_path"]))
    if not writer["closed"]:
        raise MaintenanceStageError(
            "Manifest capture requires a closed database source."
        )
    current_identity = capture_source_identity(Path(state["repository_root"]))
    if current_identity != state["source_identity"]:
        raise MaintenanceStageError("The application worktree changed after prepare.")
    state["stage"] = "capturing"
    _record(state, "capture_started", writer=writer)
    _write_state(state_path, state)
    manifest = capture_complete_manifest(
        source_root=Path(state["data_root"]),
        database_path=Path(state["database_path"]),
        config_path=Path(state["config_path"]),
        environment_path=Path(state["environment_path"]),
        destination=Path(state["manifest_root"]),
        application_source_root=Path(state["repository_root"]),
        prior_application=prior,
        closed_source_confirmed=True,
    )
    validate_complete_manifest(
        Path(state["manifest_root"]),
        expected_source_identity=state["source_identity"],
        expected_prior_application=prior,
    )
    state["stage"] = "captured"
    state["manifest"] = {
        "format_version": manifest["format_version"],
        "database_sha256": next(
            entry["sha256"]
            for entry in manifest["files"]
            if entry["kind"] == "sqlite_backup"
        ),
        "file_count": len(manifest["files"]),
        "total_bytes": sum(int(entry["size_bytes"]) for entry in manifest["files"]),
        "capture_seconds": manifest["capture_seconds"],
    }
    _record(state, "captured", writer=writer, manifest=state["manifest"])
    _write_state(state_path, state)
    return state


def _environment_key(path: Path) -> str:
    for raw in path.read_text(encoding="utf-8").splitlines():
        if raw.strip().startswith("DATA_ENC_KEY="):
            value = raw.split("=", 1)[1].strip()
            if value:
                return value
    raise MaintenanceStageError("The paired DATA_ENC_KEY is missing.")


async def _migrate_database(state: dict[str, Any]) -> dict[str, Any]:
    os.environ["ROOT_APP_DIR"] = state["data_root"]
    os.environ["DATA_ENC_KEY"] = _environment_key(Path(state["environment_path"]))
    from core.dependencies.cache_providers import get_native_library_store
    from core.dependencies.service_providers import (
        get_legacy_catalog_importer,
        get_library_policy_resolver,
    )
    from services.native.legacy_catalog_importer import REFERENCE_KINDS
    from services.native.target_startup_validator import TargetStartupValidator

    importer = get_legacy_catalog_importer()
    migration_id = f"feedback-fixes-{state['maintenance_id']}"
    prepare_started = time.perf_counter()
    plan, dry_run = await importer.prepare(migration_id)
    prepare_seconds = time.perf_counter() - prepare_started
    if plan.blockers:
        raise MaintenanceStageError(
            "The production migration dry run contains unresolved blockers."
        )
    apply_started = time.perf_counter()
    applied = await importer.apply(
        migration_id, expected_source_revision=plan.source_revision
    )
    apply_seconds = time.perf_counter() - apply_started
    validation_started = time.perf_counter()
    resolver = get_library_policy_resolver()
    startup = await TargetStartupValidator(
        get_native_library_store(),
        lambda: {root.id for root in resolver.settings.library_roots},
    ).validate("cutover")
    validation_seconds = time.perf_counter() - validation_started
    manifest = validate_complete_manifest(Path(state["manifest_root"]))
    post_migration_assets = derive_managed_assets(
        Path(state["data_root"]), Path(state["database_path"])
    )
    if (
        post_migration_assets["source_sha256"]
        != manifest["managed_assets"]["source_sha256"]
        or post_migration_assets["source_file_count"]
        != manifest["managed_assets"]["source_file_count"]
    ):
        raise MaintenanceStageError(
            "The migrated catalog requires managed assets outside the closed manifest."
        )
    zero_source_kinds = sorted(
        count.kind
        for count in applied.reference_counts
        if count.user_id is None and count.source == 0
    )
    reported_kinds = sorted(
        count.kind for count in applied.reference_counts if count.user_id is None
    )
    if reported_kinds != sorted(REFERENCE_KINDS):
        raise MaintenanceStageError(
            "The migration report omitted one or more known reference kinds."
        )
    return {
        "migration_id": migration_id,
        "source_revision": plan.source_revision,
        "root_revision": plan.root_revision,
        "prepare_seconds": prepare_seconds,
        "apply_seconds": apply_seconds,
        "validation_seconds": validation_seconds,
        "dry_run": msgspec.to_builtins(dry_run),
        "applied": msgspec.to_builtins(applied),
        "startup_marker": msgspec.to_builtins(startup),
        "reported_reference_kinds": reported_kinds,
        "zero_source_reference_kinds": zero_source_kinds,
        "network_calls": applied.network_calls,
        "tag_reads": applied.tag_reads,
        "fingerprints": applied.fingerprints,
        "managed_asset_reconciliation": {
            "required_file_count": post_migration_assets["source_file_count"],
            "required_sha256": post_migration_assets["source_sha256"],
            "missing_references": post_migration_assets["missing_references"],
        },
    }


def migrate(*, state_path: Path, authorization: str) -> dict[str, Any]:
    """Apply the idempotent target migration while the source remains stopped."""

    state = _read_state(state_path)
    _require_stage(state, "captured")
    _require_authorization(state, authorization)
    _require_source_identity(
        state, "The application worktree changed before the production migration."
    )
    validate_complete_manifest(
        Path(state["manifest_root"]),
        expected_source_identity=state["source_identity"],
        expected_prior_application=state["prior_application"],
    )
    writer = closed_writer_evidence(Path(state["database_path"]))
    if not writer["closed"]:
        raise MaintenanceStageError("Migration requires a closed database source.")
    migration = asyncio.run(_migrate_database(state))
    _require_source_identity(
        state, "The application worktree changed while the production migration ran."
    )
    state["stage"] = "migrated"
    state["migration"] = migration
    _record(
        state,
        "migrated",
        writer=writer,
        migration_id=migration["migration_id"],
        prepare_seconds=migration["prepare_seconds"],
        apply_seconds=migration["apply_seconds"],
        validation_seconds=migration["validation_seconds"],
        zero_source_reference_kinds=migration["zero_source_reference_kinds"],
    )
    _write_state(state_path, state)
    return state


def build(
    *, state_path: Path, authorization: str, runner: CommandRunner = _run_command
) -> dict[str, Any]:
    """Pin the source and build the target while the source keeps serving traffic."""

    state = _read_state(state_path)
    _require_stage(state, "prepared")
    _require_authorization(state, authorization)
    repository = Path(state["repository_root"])
    _require_source_identity(
        state, "The application worktree changed before target build."
    )
    prior = state["prior_application"]
    runner(
        [
            "docker",
            "image",
            "tag",
            prior["image_id"],
            prior["rollback_image_reference"],
        ],
        repository,
        None,
    )
    runner(
        [
            *_compose_command(state, "build"),
            "--no-cache",
            "--build-arg",
            (
                "DROPPEDNEEDLE_SOURCE_REVISION="
                + state["source_identity"]["application_revision"]
            ),
            _service_name(state),
        ],
        repository,
        None,
    )
    _require_source_identity(
        state, "The application worktree changed while the target image was built."
    )
    image = _docker_inspect(
        str(_runtime(state)["target_build_reference"]), runner=runner, image=True
    )
    target = _target_image_metadata(image, state)
    short = target["image_id"].removeprefix("sha256:")[:16]
    target["image_reference"] = (
        f"{_runtime(state)['image_tag_prefix']}:feedback-fixes-target-"
        f"{state['maintenance_id'][:8]}-{short}"
    )
    runner(
        ["docker", "image", "tag", target["image_id"], target["image_reference"]],
        repository,
        None,
    )
    pinned_target = _docker_inspect(
        target["image_reference"], runner=runner, image=True
    )
    if _target_image_metadata(pinned_target, state)["image_id"] != target["image_id"]:
        raise MaintenanceStageError("The immutable target image was not retained.")
    pinned = _docker_inspect(
        prior["rollback_image_reference"], runner=runner, image=True
    )
    if pinned.get("Id") != prior["image_id"]:
        raise MaintenanceStageError("The immutable rollback image was not retained.")
    state["stage"] = "built"
    state["target_application"] = target
    _record(
        state,
        "built",
        target_image_id=target["image_id"],
        target_image_reference=target["image_reference"],
        source_revision=target["source_revision"],
        source_remained_running=True,
        rollback_image_retained=True,
    )
    _write_state(state_path, state)
    return state


def _wait_for_health(url: str, timeout_seconds: float = 120.0) -> int:
    deadline = time.monotonic() + timeout_seconds
    while time.monotonic() < deadline:
        try:
            with urlopen(url, timeout=2) as response:  # noqa: S310 - fixed operator URL
                if response.status == 200:
                    return response.status
        except (URLError, TimeoutError, OSError):
            pass
        time.sleep(0.25)
    raise MaintenanceStageError("The application did not pass its health check.")


def start_target(
    *, state_path: Path, authorization: str, runner: CommandRunner = _run_command
) -> dict[str, Any]:
    """Start the validated target image after all offline stages have passed."""

    state = _read_state(state_path)
    _require_stage(state, "migrated")
    _require_authorization(state, authorization)
    _require_source_identity(
        state, "The application worktree changed before target admission."
    )
    if not closed_writer_evidence(Path(state["database_path"]))["closed"]:
        raise MaintenanceStageError(
            "Target startup requires the source to remain stopped."
        )
    target = state["target_application"]
    pinned_target = _docker_inspect(
        target["image_reference"], runner=runner, image=True
    )
    if _target_image_metadata(pinned_target, state) != {
        key: target[key]
        for key in ("image_id", "entrypoint", "command", "source_revision")
    }:
        raise MaintenanceStageError(
            "The pinned target image metadata changed before startup."
        )
    asyncio.run(_validate_target_database(state))
    _require_source_identity(
        state, "The application worktree changed during target startup validation."
    )
    repository = Path(state["repository_root"])
    environment = os.environ.copy()
    environment["DROPPEDNEEDLE_IMAGE"] = target["image_id"]
    runner(
        _compose_command(state, "up", "-d", "--no-deps", _service_name(state)),
        repository,
        environment,
    )
    container = _docker_inspect(_container_name(state), runner=runner)
    if (
        not _container_running(container)
        or container.get("Image") != target["image_id"]
    ):
        raise MaintenanceStageError(
            "The running target does not match the built image."
        )
    health_status = _wait_for_health(_health_url(state))
    state["stage"] = "target_started"
    _record(
        state,
        "target_started",
        container_id=container.get("Id"),
        image_id=container.get("Image"),
        health_status=health_status,
    )
    _write_state(state_path, state)
    return state


def _require_prior_launch_recipe(state: dict[str, Any]) -> None:
    prior = state["prior_application"]
    if _sha256(Path(_runtime(state)["compose_file"])) != prior["compose_config_sha256"]:
        raise MaintenanceStageError(
            "The Compose recipe changed after prepare; exact source restart is refused."
        )


def _validate_running_prior(state: dict[str, Any], running: dict[str, Any]) -> None:
    prior = state["prior_application"]
    config = running.get("Config") if isinstance(running.get("Config"), dict) else {}
    shape = _container_shape(running)
    if (
        not _container_running(running)
        or running.get("Image") != prior["image_id"]
        or list(config.get("Entrypoint") or []) != prior["entrypoint"]
        or list(config.get("Cmd") or []) != prior["command"]
        or shape["environment_sha256"] != prior["container_environment_sha256"]
        or shape["mounts"] != prior["mounts"]
        or shape["networks"] != prior["networks"]
    ):
        raise MaintenanceStageError(
            "The restored source does not match its recorded image and runtime shape."
        )


def _start_prior_application(
    state: dict[str, Any], *, runner: CommandRunner
) -> dict[str, Any]:
    _require_prior_launch_recipe(state)
    repository = Path(state["repository_root"])
    prior = state["prior_application"]
    pinned = _docker_inspect(
        prior["rollback_image_reference"], runner=runner, image=True
    )
    if pinned.get("Id") != prior["image_id"]:
        raise MaintenanceStageError("The immutable prior image is unavailable.")
    environment = os.environ.copy()
    environment["DROPPEDNEEDLE_IMAGE"] = prior["image_id"]
    runner(prior["launch_command"], repository, environment)
    running = _docker_inspect(_container_name(state), runner=runner)
    _validate_running_prior(state, running)
    return {
        "container_id": running.get("Id"),
        "image_id": running.get("Image"),
        "health_status": _wait_for_health(_health_url(state)),
    }


def resume_source(
    *, state_path: Path, authorization: str, runner: CommandRunner = _run_command
) -> dict[str, Any]:
    """Resume the pinned source before a complete manifest exists."""

    state = _read_state(state_path)
    _require_stage(state, "built", "stopped", "capturing")
    _require_authorization(state, authorization)
    complete_manifest = False
    try:
        validate_complete_manifest(
            Path(state["manifest_root"]),
            expected_source_identity=state["source_identity"],
            expected_prior_application=state["prior_application"],
        )
        complete_manifest = True
    except MaintenanceManifestError:
        complete_manifest = False
    try:
        running = _docker_inspect(_container_name(state), runner=runner)
    except MaintenanceStageError:
        running = {}
    if complete_manifest:
        if _container_running(running):
            runner(
                _compose_command(state, "stop", _service_name(state)),
                Path(state["repository_root"]),
                None,
            )
        writer = _wait_for_closed_writer(Path(state["database_path"]))
        if not writer["closed"]:
            raise MaintenanceStageError(
                "Source recovery restore requires every writer to be stopped."
            )
        restore_complete_manifest_in_place(
            Path(state["manifest_root"]),
            Path(state["data_root"]),
            closed_source_confirmed=True,
        )
        evidence = _start_prior_application(state, runner=runner)
        evidence["restored_manifest"] = True
    elif _container_running(running):
        _require_prior_launch_recipe(state)
        _validate_running_prior(state, running)
        evidence = {
            "container_id": running.get("Id"),
            "image_id": running.get("Image"),
            "health_status": _wait_for_health(_health_url(state)),
            "restored_manifest": False,
        }
    else:
        evidence = _start_prior_application(state, runner=runner)
        evidence["restored_manifest"] = False
    state["stage"] = "source_resumed"
    _record(state, "source_resumed", **evidence)
    _write_state(state_path, state)
    return state


async def _validate_target_database(state: dict[str, Any]) -> dict[str, Any]:
    os.environ["ROOT_APP_DIR"] = state["data_root"]
    os.environ["DATA_ENC_KEY"] = _environment_key(Path(state["environment_path"]))
    from core.dependencies.cache_providers import get_native_library_store
    from core.dependencies.service_providers import get_library_policy_resolver
    from services.native.target_startup_validator import TargetStartupValidator

    resolver = get_library_policy_resolver()
    return await TargetStartupValidator(
        get_native_library_store(),
        lambda: {root.id for root in resolver.settings.library_roots},
    ).validate("cutover")


def rollback(
    *, state_path: Path, authorization: str, runner: CommandRunner = _run_command
) -> dict[str, Any]:
    """Stop the target, restore the whole manifest, and start the pinned source image."""

    state = _read_state(state_path)
    _require_stage(state, "captured", "migrated", "target_started")
    _require_authorization(state, authorization)
    repository = Path(state["repository_root"])
    prior = state["prior_application"]
    _require_prior_launch_recipe(state)
    pinned = _docker_inspect(
        prior["rollback_image_reference"], runner=runner, image=True
    )
    if pinned.get("Id") != prior["image_id"]:
        raise MaintenanceStageError(
            "The immutable prior image is unavailable for rollback."
        )
    try:
        container = _docker_inspect(_container_name(state), runner=runner)
    except MaintenanceStageError:
        container = {}
    if container and _container_running(container):
        runner(_compose_command(state, "stop", _service_name(state)), repository, None)
    writer = _wait_for_closed_writer(Path(state["database_path"]))
    if not writer["closed"]:
        raise MaintenanceStageError(
            "Rollback restore requires every writer to be stopped."
        )
    restored = restore_complete_manifest_in_place(
        Path(state["manifest_root"]),
        Path(state["data_root"]),
        closed_source_confirmed=True,
    )
    source = _start_prior_application(state, runner=runner)
    state["stage"] = "rolled_back"
    _record(
        state,
        "rolled_back",
        writer=writer,
        restore=restored,
        image_id=source["image_id"],
        container_id=source["container_id"],
        health_status=source["health_status"],
    )
    _write_state(state_path, state)
    return state


def _safe_summary(state: dict[str, Any]) -> dict[str, Any]:
    return {
        "maintenance_id": state.get("maintenance_id"),
        "stage": state.get("stage"),
        "authorization_challenge": state.get("authorization_challenge"),
        "source_revision": (state.get("source_identity") or {}).get(
            "application_revision"
        ),
        "prior_image_id": (state.get("prior_application") or {}).get("image_id"),
        "manifest": state.get("manifest"),
        "migration": {
            key: state.get("migration", {}).get(key)
            for key in (
                "migration_id",
                "prepare_seconds",
                "apply_seconds",
                "validation_seconds",
                "zero_source_reference_kinds",
            )
        }
        if state.get("migration")
        else None,
        "events": state.get("events", []),
    }


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Staged Feedback Fixes offline replacement and rollback"
    )
    parser.add_argument(
        "stage",
        choices=(
            "prepare",
            "stop",
            "capture",
            "migrate",
            "build",
            "start-target",
            "resume-source",
            "rollback",
            "status",
        ),
    )
    parser.add_argument("--state", type=Path, required=True)
    parser.add_argument("--authorization")
    parser.add_argument("--repository-root", type=Path, default=Path.cwd())
    parser.add_argument("--data-root", type=Path)
    parser.add_argument("--manifest-root", type=Path)
    parser.add_argument("--compose-file", type=Path)
    parser.add_argument("--compose-project")
    parser.add_argument("--service-name", default=SERVICE_NAME)
    parser.add_argument("--container-name")
    parser.add_argument("--target-build-reference", default="droppedneedle:local")
    parser.add_argument("--image-tag-prefix", default="droppedneedle")
    parser.add_argument("--health-url", default="http://127.0.0.1:8688/health")
    return parser


def main() -> None:
    args = _parser().parse_args()
    if args.stage == "prepare":
        if args.data_root is None or args.manifest_root is None:
            raise SystemExit("prepare requires --data-root and --manifest-root")
        state = prepare(
            state_path=args.state,
            repository_root=args.repository_root,
            data_root=args.data_root,
            manifest_root=args.manifest_root,
            compose_file=args.compose_file,
            compose_project=args.compose_project,
            service_name=args.service_name,
            container_name=args.container_name,
            target_build_reference=args.target_build_reference,
            image_tag_prefix=args.image_tag_prefix,
            health_url=args.health_url,
        )
    elif args.stage == "status":
        state = _read_state(args.state)
    else:
        if not args.authorization:
            raise SystemExit(f"{args.stage} requires --authorization")
        function = {
            "stop": stop,
            "capture": capture,
            "migrate": migrate,
            "build": build,
            "start-target": start_target,
            "resume-source": resume_source,
            "rollback": rollback,
        }[args.stage]
        state = function(
            state_path=args.state,
            authorization=args.authorization,
        )
    print(json.dumps(_safe_summary(state), indent=2, sort_keys=True))


if __name__ == "__main__":
    try:
        main()
    except (MaintenanceStageError, MaintenanceManifestError) as exc:
        print(f"Feedback Fixes maintenance refused: {exc}", file=sys.stderr)
        raise SystemExit(2) from exc
