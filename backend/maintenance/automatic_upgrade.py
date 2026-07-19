"""Automatic one-time upgrade used by normal Docker image startup."""

from __future__ import annotations

import asyncio
import hashlib
import http.client
import json
import logging
import os
import signal
import shutil
import sqlite3
import subprocess
import sys
import time
import uuid
from collections.abc import Callable
from contextlib import contextmanager
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from threading import Thread
from typing import Any

from core.config import Settings, get_settings, migrate_legacy_config
from infrastructure.file_utils import atomic_write_json

logger = logging.getLogger(__name__)

UPGRADE_ID = "feedback-fixes-v1"
MIGRATION_ID = "automatic-feedback-fixes-v1"
_MARKER = "legacy_catalog_import_complete"
_SOURCE_REVISION_PATH = Path("/app/.droppedneedle-source-revision")
_ADMISSION_TOKEN_ENV = "DROPPEDNEEDLE_TARGET_ADMISSION_TOKEN"
_FAILURE_EVIDENCE_FILE = "automatic-upgrade-failure-evidence.json"


class AutomaticUpgradeError(RuntimeError):
    """Raised after a failed automatic upgrade has restored its inputs."""


class _WorkingMigrationError(AutomaticUpgradeError):
    def __init__(self, message: str, evidence: dict[str, Any] | None = None) -> None:
        super().__init__(message)
        self.evidence = evidence


class _UpgradeHealthHandler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        if self.path == "/health":
            body = b'{"status":"upgrading"}'
            self.send_response(200)
        else:
            body = b'{"error":"DroppedNeedle is upgrading its library"}'
            self.send_response(503)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, _format: str, *args: object) -> None:
        return


@contextmanager
def _upgrade_health_server(port: int):
    server = ThreadingHTTPServer(("0.0.0.0", port), _UpgradeHealthHandler)
    thread = Thread(target=server.serve_forever, name="upgrade-health", daemon=True)
    thread.start()
    try:
        yield
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


@dataclass(frozen=True)
class UpgradeBackup:
    directory: Path
    database: Path | None
    config: Path | None
    database_existed: bool
    config_existed: bool


def _sha256(path: Path) -> str | None:
    if not path.is_file():
        return None
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _database_has_marker(database: Path) -> bool:
    if not database.is_file():
        return False
    try:
        with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as connection:
            present = connection.execute(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' "
                "AND name = 'library_migration_markers'"
            ).fetchone()
            if present is None:
                return False
            return (
                connection.execute(
                    "SELECT 1 FROM library_migration_markers WHERE marker = ?",
                    (_MARKER,),
                ).fetchone()
                is not None
            )
    except sqlite3.Error:
        return False


def _sqlite_backup(source: Path, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    with (
        sqlite3.connect(f"file:{source}?mode=ro", uri=True) as source_connection,
        sqlite3.connect(destination) as destination_connection,
    ):
        source_connection.backup(destination_connection)
    destination.chmod(source.stat().st_mode & 0o777)
    with destination.open("rb") as handle:
        os.fsync(handle.fileno())


def _write_state(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    atomic_write_json(path, payload)
    with path.open("rb") as handle:
        os.fsync(handle.fileno())
    _fsync_directory(path.parent)


def _read_state(path: Path) -> dict[str, Any] | None:
    if not path.is_file():
        return None
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError, TypeError):
        return None
    return payload if isinstance(payload, dict) else None


def _current_signature(database: Path, config: Path) -> dict[str, str | None]:
    return {
        "database_sha256": _sha256(database),
        "config_sha256": _sha256(config),
    }


def _image_version() -> str:
    configured = os.getenv("DROPPEDNEEDLE_SOURCE_REVISION", "").strip()
    if configured and configured != "unknown":
        return configured
    try:
        baked = _SOURCE_REVISION_PATH.read_text(encoding="utf-8").strip()
    except OSError:
        baked = ""
    return baked or os.getenv("COMMIT_TAG", "unknown")


def _failed_attempt_matches(
    state_path: Path,
    *,
    database: Path,
    config: Path,
    image_version: str,
) -> bool:
    payload = _read_state(state_path)
    if payload is None:
        return False
    return (
        payload.get("stage") == "failed"
        and payload.get("image_version") == image_version
        and payload.get("restored_signature") == _current_signature(database, config)
    )


def _completed_install_is_verified_rollback(
    settings: Settings, state: dict[str, Any]
) -> bool:
    try:
        backup = _load_upgrade_backup(settings, state.get("backup_directory"))
    except AutomaticUpgradeError:
        return False
    if not backup.database_existed or backup.database is None:
        return False
    expected = {
        "database_sha256": _sha256(backup.database),
        "config_sha256": _sha256(backup.config) if backup.config is not None else None,
    }
    return expected == _current_signature(
        settings.library_db_path, settings.config_file_path
    )


def capture_upgrade_backup(settings: Settings) -> UpgradeBackup:
    database = settings.library_db_path
    config = settings.config_file_path
    database_existed = database.is_file()
    config_existed = config.is_file()
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    directory = (
        settings.cache_dir
        / "upgrade-backups"
        / f"{UPGRADE_ID}-{stamp}-{uuid.uuid4().hex[:8]}"
    )
    directory.mkdir(parents=True, exist_ok=False)
    backup_database = directory / "library.db" if database_existed else None
    backup_config = directory / "config.json" if config_existed else None
    if backup_database is not None:
        _sqlite_backup(database, backup_database)
    if backup_config is not None:
        _replace_file(config, backup_config)
    _write_state(
        directory / "manifest.json",
        {
            "format_version": 1,
            "upgrade_id": UPGRADE_ID,
            "database_existed": database_existed,
            "config_existed": config_existed,
            "database_sha256": _sha256(backup_database)
            if backup_database is not None
            else None,
            "config_sha256": _sha256(backup_config)
            if backup_config is not None
            else None,
        },
    )
    return UpgradeBackup(
        directory=directory,
        database=backup_database,
        config=backup_config,
        database_existed=database_existed,
        config_existed=config_existed,
    )


def _load_upgrade_backup(settings: Settings, directory_value: object) -> UpgradeBackup:
    if not isinstance(directory_value, str) or not directory_value:
        raise AutomaticUpgradeError("The interrupted upgrade record is incomplete.")
    directory = Path(directory_value).resolve()
    backup_root = (settings.cache_dir / "upgrade-backups").resolve()
    if not directory.is_relative_to(backup_root):
        raise AutomaticUpgradeError("The interrupted upgrade backup path is invalid.")
    try:
        manifest = json.loads((directory / "manifest.json").read_text(encoding="utf-8"))
    except (OSError, ValueError, TypeError) as error:
        raise AutomaticUpgradeError(
            "The interrupted upgrade backup manifest is unavailable."
        ) from error
    if not isinstance(manifest, dict) or manifest.get("upgrade_id") != UPGRADE_ID:
        raise AutomaticUpgradeError("The interrupted upgrade backup is invalid.")
    database_existed = manifest.get("database_existed") is True
    config_existed = manifest.get("config_existed") is True
    database = directory / "library.db" if database_existed else None
    config = directory / "config.json" if config_existed else None
    if database is not None and _sha256(database) != manifest.get("database_sha256"):
        raise AutomaticUpgradeError("The interrupted database backup is incomplete.")
    if config is not None and _sha256(config) != manifest.get("config_sha256"):
        raise AutomaticUpgradeError("The interrupted settings backup is incomplete.")
    return UpgradeBackup(
        directory=directory,
        database=database,
        config=config,
        database_existed=database_existed,
        config_existed=config_existed,
    )


def _fsync_directory(directory: Path) -> None:
    try:
        descriptor = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
    except OSError:
        return
    try:
        os.fsync(descriptor)
    except OSError:
        logger.warning("automatic_upgrade.directory_fsync_unavailable")
    finally:
        os.close(descriptor)


def _replace_file(source: Path, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_name(
        f".{destination.name}.upgrade-{uuid.uuid4().hex}.tmp"
    )
    try:
        with source.open("rb") as source_handle, temporary.open("wb") as target_handle:
            shutil.copyfileobj(source_handle, target_handle, length=1024 * 1024)
            target_handle.flush()
            os.fsync(target_handle.fileno())
        shutil.copystat(source, temporary)
        os.replace(temporary, destination)
        _fsync_directory(destination.parent)
    finally:
        temporary.unlink(missing_ok=True)


def _replace_database(source: Path, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_name(
        f".{destination.name}.upgrade-{uuid.uuid4().hex}.tmp"
    )
    try:
        _sqlite_backup(source, temporary)
        with temporary.open("rb") as handle:
            os.fsync(handle.fileno())
        for suffix in ("-wal", "-shm"):
            Path(f"{destination}{suffix}").unlink(missing_ok=True)
        os.replace(temporary, destination)
        _fsync_directory(destination.parent)
    finally:
        temporary.unlink(missing_ok=True)


def restore_upgrade_backup(settings: Settings, backup: UpgradeBackup) -> None:
    database = settings.library_db_path
    config = settings.config_file_path
    if backup.database_existed:
        if backup.database is None or not backup.database.is_file():
            raise AutomaticUpgradeError("The database upgrade backup is incomplete.")
        _replace_database(backup.database, database)
    else:
        for suffix in ("-wal", "-shm"):
            Path(f"{database}{suffix}").unlink(missing_ok=True)
        database.unlink(missing_ok=True)
    if backup.config_existed:
        if backup.config is None or not backup.config.is_file():
            raise AutomaticUpgradeError("The settings upgrade backup is incomplete.")
        _replace_file(backup.config, config)
    else:
        config.unlink(missing_ok=True)


def prepare_working_copy(settings: Settings, backup: UpgradeBackup) -> Path:
    working = backup.directory / "working"
    working_cache = working / "cache"
    working_config = working / "config"
    working_cache.mkdir(parents=True, exist_ok=False)
    working_config.mkdir(parents=True, exist_ok=False)
    if backup.database is not None:
        shutil.copy2(backup.database, working_cache / "library.db")
    if backup.config is not None:
        shutil.copy2(backup.config, working_config / "config.json")
    environment_file = settings.config_file_path.parent / ".env"
    if environment_file.is_file():
        shutil.copy2(environment_file, working_config / ".env")
    return working


def _remove_working_copy(backup: UpgradeBackup) -> None:
    try:
        shutil.rmtree(backup.directory / "working")
    except FileNotFoundError:
        return
    except OSError:
        logger.warning("automatic_upgrade.working_copy_cleanup_failed")


def promote_working_copy(settings: Settings, working: Path) -> None:
    working_database = working / "cache" / "library.db"
    working_config = working / "config" / "config.json"
    if not working_database.is_file() or not _database_has_marker(working_database):
        raise AutomaticUpgradeError(
            "The checked library upgrade is missing its completion marker."
        )
    if working_config.is_file():
        _replace_file(working_config, settings.config_file_path)
    _replace_database(working_database, settings.library_db_path)


def _restore_interrupted_upgrade(settings: Settings, state_path: Path) -> None:
    state = _read_state(state_path)
    if state is None or state.get("stage") not in {
        "running",
        "migrating",
        "promoting",
        "promoted_pending_startup",
    }:
        return
    backup = _load_upgrade_backup(settings, state.get("backup_directory"))
    source_unchanged = state.get("stage") == "migrating" and state.get(
        "source_signature"
    ) == _current_signature(settings.library_db_path, settings.config_file_path)
    if not source_unchanged:
        try:
            restore_upgrade_backup(settings, backup)
        except (OSError, sqlite3.Error, AutomaticUpgradeError) as error:
            raise AutomaticUpgradeError(
                "DroppedNeedle found an interrupted library upgrade but could not "
                "restore its safety backup. Do not start another image against this "
                "database."
            ) from error
    _remove_working_copy(backup)
    _write_state(
        state_path,
        {
            "format_version": 1,
            "upgrade_id": UPGRADE_ID,
            "stage": (
                "interrupted_unchanged" if source_unchanged else "interrupted_restored"
            ),
            "backup_directory": str(backup.directory),
            "restored_signature": _current_signature(
                settings.library_db_path, settings.config_file_path
            ),
        },
    )
    if source_unchanged:
        message = (
            "[upgrade] Found an interrupted upgrade; the original data is unchanged."
        )
    else:
        message = (
            "[upgrade] Restored an interrupted library upgrade from its safety backup."
        )
    print(message, flush=True)


def _run_working_migration(working: Path) -> dict[str, Any]:
    environment = os.environ.copy()
    environment.update(
        {
            "ROOT_APP_DIR": str(working),
            "CACHE_DIR": str(working / "cache"),
            "LIBRARY_DB_PATH": str(working / "cache" / "library.db"),
            "CONFIG_FILE_PATH": str(working / "config" / "config.json"),
        }
    )
    result = subprocess.run(
        [sys.executable, "-m", "maintenance.automatic_upgrade", "--migrate-working"],
        env=environment,
        check=False,
    )
    if result.returncode != 0:
        failure_evidence = _read_state(working / "cache" / _FAILURE_EVIDENCE_FILE)
        if failure_evidence is None:
            failure_evidence = {
                "reason": "working_process_exited",
                "returncode": result.returncode,
            }
        raise _WorkingMigrationError(
            "The copied library database did not pass its upgrade checks.",
            failure_evidence,
        )
    evidence_path = working / "cache" / "automatic-upgrade-evidence.json"
    try:
        evidence = json.loads(evidence_path.read_text(encoding="utf-8"))
    except (OSError, ValueError, TypeError) as error:
        raise AutomaticUpgradeError(
            "The copied library database did not produce its upgrade report."
        ) from error
    if not isinstance(evidence, dict):
        raise AutomaticUpgradeError("The library upgrade report is invalid.")
    return evidence


async def _perform_target_migration() -> dict[str, Any]:
    from core.dependencies.cache_providers import (
        get_native_library_store,
        get_preferences_service,
    )
    from core.dependencies.service_providers import (
        get_library_policy_resolver,
    )
    from services.native.bounded_legacy_catalog_migrator import (
        BoundedLegacyCatalogMigrator,
    )
    from services.native.target_startup_validator import TargetStartupValidator

    print("[upgrade] Preparing migrated settings and library roots.", flush=True)
    migrate_legacy_config()
    preferences = get_preferences_service()
    preferences.get_typed_library_settings()
    resolver = get_library_policy_resolver()
    outcome = await BoundedLegacyCatalogMigrator(
        get_native_library_store(),
        resolver,
        emit_progress=lambda message: print(message, flush=True),
    ).migrate(MIGRATION_ID)
    report = outcome.report
    if outcome.blocker_count:
        blocker_reason_counts = {
            key: value
            for key, value in outcome.blocker_reason_counts.items()
            if value
        }
        _write_state(
            get_settings().cache_dir / _FAILURE_EVIDENCE_FILE,
            {
                "reason": "unresolved_references",
                "blocker_count": outcome.blocker_count,
                "unresolved_reference_counts": {
                    count.kind: count.unresolved
                    for count in report.reference_counts
                    if count.user_id is None and count.unresolved
                },
                "blocker_reason_counts": blocker_reason_counts,
            },
        )
        reference_summary = ", ".join(
            f"{count.kind}={count.unresolved:,}"
            for count in report.reference_counts
            if count.user_id is None and count.unresolved
        )
        reason_summary = ", ".join(
            f"{key}={value:,}" for key, value in sorted(blocker_reason_counts.items())
        )
        details = "; ".join(
            part
            for part in (
                f"references: {reference_summary}" if reference_summary else "",
                f"reasons: {reason_summary}" if reason_summary else "",
            )
            if part
        )
        noun = "record" if outcome.blocker_count == 1 else "records"
        detail_suffix = f": {details}" if details else ""
        print(
            f"[upgrade] Migration checks found {outcome.blocker_count:,} unresolved "
            f"{noun}{detail_suffix}.",
            flush=True,
        )
        raise AutomaticUpgradeError(
            "The existing library contains references that cannot be upgraded safely."
        )
    if (
        report.embedded_art_reads
        or report.network_calls
        or report.tag_reads
        or report.fingerprints
    ):
        raise AutomaticUpgradeError(
            "The library upgrade attempted work that is not allowed during startup."
        )
    print("[upgrade] Running independent target startup validation.", flush=True)
    validation = await TargetStartupValidator(
        get_native_library_store(),
        lambda: {root.id for root in resolver.settings.library_roots},
    ).validate("cutover")
    print("[upgrade] Working-copy migration checks passed.", flush=True)
    return {
        "source_revision": report.source_revision,
        "root_revision": report.root_revision,
        "reference_counts": len(report.reference_counts),
        "invariants": validation["invariants"],
        "network_calls": report.network_calls,
        "tag_reads": report.tag_reads,
        "fingerprints": report.fingerprints,
        "embedded_art_reads": report.embedded_art_reads,
    }


def run_automatic_copy_upgrade(
    settings: Settings,
    *,
    runner: Callable[[Path], dict[str, Any]] = _run_working_migration,
    require_target_admission: bool = False,
) -> str:
    database = settings.library_db_path
    config = settings.config_file_path
    state_path = settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json"
    image_version = _image_version()
    state = _read_state(state_path)
    marker_present = _database_has_marker(database)
    if (
        state is not None
        and state.get("stage") == "completed"
        and not marker_present
        and not _completed_install_is_verified_rollback(settings, state)
    ):
        raise AutomaticUpgradeError(
            "This installation was upgraded previously, but its target library "
            "database is now missing or incomplete. Restore a verified backup before "
            "starting DroppedNeedle."
        )
    _restore_interrupted_upgrade(settings, state_path)
    if _database_has_marker(database):
        return "ready"
    if _failed_attempt_matches(
        state_path,
        database=database,
        config=config,
        image_version=image_version,
    ):
        raise AutomaticUpgradeError(
            "This image already tried the library upgrade and left your previous data "
            "in place. Install a newer image before trying again."
        )

    print(
        "[upgrade] Preparing the library for this DroppedNeedle version. "
        "Large libraries may take a few minutes.",
        flush=True,
    )
    backup: UpgradeBackup | None = None
    try:
        backup = capture_upgrade_backup(settings)
        working = prepare_working_copy(settings, backup)
        _write_state(
            state_path,
            {
                "format_version": 1,
                "upgrade_id": UPGRADE_ID,
                "stage": "migrating",
                "image_version": image_version,
                "backup_directory": str(backup.directory),
                "source_signature": _current_signature(database, config),
            },
        )
    except (OSError, sqlite3.Error) as error:
        if backup is not None:
            shutil.rmtree(backup.directory, ignore_errors=True)
        raise AutomaticUpgradeError(
            "DroppedNeedle could not create the safety backup. Check that the config "
            "and cache volumes are writable and have enough free space. No data was changed."
        ) from error

    assert backup is not None
    promotion_started = False
    try:
        evidence = runner(working)
        _write_state(
            state_path,
            {
                "format_version": 1,
                "upgrade_id": UPGRADE_ID,
                "stage": "promoting",
                "image_version": image_version,
                "backup_directory": str(backup.directory),
            },
        )
        promotion_started = True
        promote_working_copy(settings, working)
        if not _database_has_marker(database):
            raise AutomaticUpgradeError(
                "The upgraded library was not installed completely."
            )
        completed = {
            "format_version": 1,
            "upgrade_id": UPGRADE_ID,
            "stage": (
                "promoted_pending_startup" if require_target_admission else "completed"
            ),
            "image_version": image_version,
            "backup_directory": str(backup.directory),
            "evidence": evidence,
        }
        _write_state(state_path, completed)
    except Exception as error:  # noqa: BLE001 - all failures must leave source safe
        if promotion_started:
            try:
                restore_upgrade_backup(settings, backup)
            except (OSError, sqlite3.Error, AutomaticUpgradeError) as restore_error:
                logger.critical(
                    "automatic_upgrade.restore_failed",
                    extra={"error_type": type(restore_error).__name__},
                )
                raise AutomaticUpgradeError(
                    "The library upgrade failed and its backup could not be restored. "
                    "Do not start an older image against this database."
                ) from restore_error
        failure = {
            "format_version": 1,
            "upgrade_id": UPGRADE_ID,
            "stage": "failed",
            "image_version": image_version,
            "backup_directory": str(backup.directory),
            "error_type": type(error).__name__,
            "restored_signature": _current_signature(database, config),
        }
        failure_evidence = getattr(error, "evidence", None)
        if isinstance(failure_evidence, dict):
            failure["failure_evidence"] = failure_evidence
        _remove_working_copy(backup)
        try:
            _write_state(state_path, failure)
        except OSError:
            logger.error("automatic_upgrade.failure_state_write_failed")
        logger.error(
            "automatic_upgrade.failed error_type=%s",
            type(error).__name__,
        )
        raise AutomaticUpgradeError(
            "The library upgrade could not be completed. Your previous database and "
            "settings remain in place. Your music files were not changed."
        ) from error

    _remove_working_copy(backup)
    if require_target_admission:
        print(
            "[upgrade] Checked library upgrade installed. Verifying DroppedNeedle startup.",
            flush=True,
        )
    else:
        print("[upgrade] Library upgrade complete.", flush=True)
    return "upgraded"


def _container_port(settings: Settings) -> int:
    return int(os.getenv("PORT", str(settings.port)))


def _config_path_before_settings() -> Path:
    configured = os.getenv("CONFIG_FILE_PATH", "").strip()
    if configured:
        return Path(configured)
    root = Path(os.getenv("ROOT_APP_DIR", "/app"))
    return root / "config" / "config.json"


def _target_ready(port: int) -> bool:
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=1)
    try:
        connection.request("GET", "/health")
        response = connection.getresponse()
        payload = json.loads(response.read())
        return response.status == 200 and payload.get("status") == "ok"
    except (OSError, ValueError, TypeError, http.client.HTTPException):
        return False
    finally:
        connection.close()


def _admission_paths(settings: Settings, token: str) -> tuple[Path, Path]:
    if len(token) != 32 or any(
        character not in "0123456789abcdef" for character in token
    ):
        raise AutomaticUpgradeError("The target startup admission token is invalid.")
    root = settings.cache_dir / "target-startup-admission"
    return root / f"{token}.validated.json", root / f"{token}.admitted.json"


def target_startup_admission_pending() -> bool:
    return bool(os.getenv(_ADMISSION_TOKEN_ENV, "").strip())


async def await_target_startup_admission(settings: Settings) -> None:
    """Pause target lifespan after validation until the parent commits promotion."""

    token = os.getenv(_ADMISSION_TOKEN_ENV, "").strip()
    if not token:
        return
    validated_path, admitted_path = _admission_paths(settings, token)
    _write_state(
        validated_path,
        {"format_version": 1, "upgrade_id": UPGRADE_ID, "token": token},
    )
    deadline = time.monotonic() + 300
    while time.monotonic() < deadline:
        admitted = _read_state(admitted_path)
        if admitted is not None and admitted.get("token") == token:
            validated_path.unlink(missing_ok=True)
            admitted_path.unlink(missing_ok=True)
            return
        await asyncio.sleep(0.05)
    raise AutomaticUpgradeError(
        "The target application was not admitted after startup validation."
    )


def _target_command(port: int) -> list[str]:
    return [
        sys.executable,
        "-m",
        "uvicorn",
        "target_main:app",
        "--host",
        "0.0.0.0",
        "--port",
        str(port),
        "--loop",
        "uvloop",
        "--http",
        "httptools",
        "--workers",
        "1",
    ]


def _complete_target_admission(settings: Settings) -> None:
    state_path = settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json"
    state = _read_state(state_path)
    if state is None or state.get("stage") != "promoted_pending_startup":
        raise AutomaticUpgradeError(
            "The checked library upgrade has no pending startup record."
        )
    _write_state(
        state_path,
        {
            **state,
            "stage": "completed",
            "target_admitted_at": time.time(),
        },
    )


def _target_validation_complete(path: Path, token: str) -> bool:
    state = _read_state(path)
    return state is not None and state.get("token") == token


def _restore_after_target_startup_failure(
    settings: Settings, *, error_type: str
) -> None:
    state_path = settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json"
    state = _read_state(state_path)
    if state is None or state.get("stage") != "promoted_pending_startup":
        return
    backup = _load_upgrade_backup(settings, state.get("backup_directory"))
    restore_upgrade_backup(settings, backup)
    _remove_working_copy(backup)
    _write_state(
        state_path,
        {
            "format_version": 1,
            "upgrade_id": UPGRADE_ID,
            "stage": "failed",
            "image_version": _image_version(),
            "backup_directory": str(backup.directory),
            "error_type": error_type,
            "restored_signature": _current_signature(
                settings.library_db_path, settings.config_file_path
            ),
        },
    )


def run_target_supervisor(
    settings: Settings,
    *,
    command: list[str] | None = None,
    admission_timeout_seconds: float = 300.0,
) -> int:
    port = _container_port(settings)
    state = _read_state(settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json")
    admission_pending = (
        state is not None and state.get("stage") == "promoted_pending_startup"
    )
    token = uuid.uuid4().hex if admission_pending else ""
    validated_path: Path | None = None
    admitted_path: Path | None = None
    environment = os.environ.copy()
    if admission_pending:
        validated_path, admitted_path = _admission_paths(settings, token)
        validated_path.unlink(missing_ok=True)
        admitted_path.unlink(missing_ok=True)
        environment[_ADMISSION_TOKEN_ENV] = token
    try:
        process = subprocess.Popen(command or _target_command(port), env=environment)
    except OSError:
        try:
            _restore_after_target_startup_failure(
                settings, error_type="TargetProcessStartError"
            )
        except (OSError, sqlite3.Error, AutomaticUpgradeError):
            logger.critical("automatic_upgrade.target_start_restore_failed")
        print(
            "[upgrade] ERROR: DroppedNeedle could not start after the library upgrade.",
            flush=True,
        )
        return 1

    forwarded_signal: int | None = None
    previous_handlers: dict[int, Any] = {}

    def forward(signum: int, _frame: Any) -> None:
        nonlocal forwarded_signal
        forwarded_signal = signum
        if process.poll() is None:
            process.send_signal(signum)

    for signum in (signal.SIGTERM, signal.SIGINT):
        previous_handlers[signum] = signal.signal(signum, forward)

    promotion_committed = not admission_pending
    target_released = not admission_pending
    admission_error = "TargetStartupError"
    deadline = time.monotonic() + admission_timeout_seconds
    try:
        if admission_pending:
            assert validated_path is not None
            assert admitted_path is not None
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    admission_error = "TargetProcessExited"
                    break
                if _target_validation_complete(validated_path, token):
                    try:
                        _complete_target_admission(settings)
                        promotion_committed = True
                        _write_state(
                            admitted_path,
                            {
                                "format_version": 1,
                                "upgrade_id": UPGRADE_ID,
                                "token": token,
                            },
                        )
                    except (OSError, AutomaticUpgradeError):
                        admission_error = "TargetAdmissionWriteError"
                        break
                    target_released = True
                    break
                time.sleep(0.05)
            else:
                admission_error = "TargetStartupTimeout"

        if promotion_committed and not target_released:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=10)
            print(
                "[upgrade] ERROR: The upgraded library is safe, but DroppedNeedle "
                "could not continue startup. Restart with a corrected image.",
                flush=True,
            )
            return 1

        if promotion_committed:
            while process.poll() is None and time.monotonic() < deadline:
                if _target_ready(port):
                    print(
                        "[upgrade] Library upgrade complete. DroppedNeedle is ready.",
                        flush=True,
                    )
                    break
                time.sleep(0.25)
            return process.wait()

        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        try:
            _restore_after_target_startup_failure(settings, error_type=admission_error)
        except (OSError, sqlite3.Error, AutomaticUpgradeError):
            logger.critical("automatic_upgrade.target_start_restore_failed")
            print(
                "[upgrade] ERROR: Target startup failed and the safety backup could "
                "not be restored. Do not start another image against this database.",
                flush=True,
            )
            return 1
        if forwarded_signal is None:
            print(
                "[upgrade] ERROR: Target startup failed. DroppedNeedle restored the "
                "previous database and settings.",
                flush=True,
            )
        return process.returncode or 1
    finally:
        if validated_path is not None:
            validated_path.unlink(missing_ok=True)
        if admitted_path is not None:
            admitted_path.unlink(missing_ok=True)
        for signum, handler in previous_handlers.items():
            signal.signal(signum, handler)


def main() -> int:
    config_path_before_settings = _config_path_before_settings()
    config_existed_before_settings = config_path_before_settings.is_file()
    settings = get_settings()
    remove_generated_config_on_failure = (
        not config_existed_before_settings
        and settings.config_file_path == config_path_before_settings
    )
    if sys.argv[1:] == ["--migrate-working"]:
        try:
            evidence = asyncio.run(_perform_target_migration())
            _write_state(
                settings.cache_dir / "automatic-upgrade-evidence.json", evidence
            )
        except Exception as error:  # noqa: BLE001 - parent reports a safe summary
            failure_path = settings.cache_dir / _FAILURE_EVIDENCE_FILE
            if _read_state(failure_path) is None:
                try:
                    _write_state(
                        failure_path,
                        {
                            "reason": "working_migration_error",
                            "error_type": type(error).__name__,
                        },
                    )
                except OSError:
                    logger.error("automatic_upgrade.failure_state_write_failed")
            logger.error(
                "automatic_upgrade.working_copy_failed error_type=%s",
                type(error).__name__,
            )
            return 1
        return 0
    start_target = sys.argv[1:] == ["--start-target"]
    if sys.argv[1:] not in ([], ["--start-target"]):
        print("[upgrade] ERROR: Unknown startup option.", flush=True)
        return 2
    try:
        state = _read_state(settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json")
        needs_upgrade = not _database_has_marker(settings.library_db_path) or (
            state is not None
            and state.get("stage")
            in {"running", "migrating", "promoting", "promoted_pending_startup"}
        )
        if needs_upgrade:
            with _upgrade_health_server(_container_port(settings)):
                run_automatic_copy_upgrade(
                    settings, require_target_admission=start_target
                )
    except AutomaticUpgradeError as error:
        if remove_generated_config_on_failure:
            settings.config_file_path.unlink(missing_ok=True)
        print(f"[upgrade] ERROR: {error}", flush=True)
        return 1
    except OSError:
        if remove_generated_config_on_failure:
            settings.config_file_path.unlink(missing_ok=True)
        print(
            "[upgrade] ERROR: DroppedNeedle could not open its temporary health check "
            "while upgrading.",
            flush=True,
        )
        return 1
    if not start_target:
        return 0
    result = run_target_supervisor(settings)
    final_state = _read_state(
        settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json"
    )
    if (
        result != 0
        and remove_generated_config_on_failure
        and final_state is not None
        and final_state.get("stage") == "failed"
    ):
        settings.config_file_path.unlink(missing_ok=True)
    return result


if __name__ == "__main__":
    raise SystemExit(main())
