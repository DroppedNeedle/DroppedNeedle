"""Automatic one-time upgrade used by normal Docker image startup."""

from __future__ import annotations

import asyncio
import functools
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
from collections.abc import AsyncIterator, Callable
from contextlib import asynccontextmanager, contextmanager
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
_PUBLISH_VERIFY_ATTEMPTS = 10
_PUBLISH_VERIFY_INTERVAL_SECONDS = 0.25
_MARKER = "legacy_catalog_import_complete"
_SOURCE_REVISION_PATH = Path("/app/.droppedneedle-source-revision")
_ADMISSION_TOKEN_ENV = "DROPPEDNEEDLE_TARGET_ADMISSION_TOKEN"
_FAILURE_EVIDENCE_FILE = "automatic-upgrade-failure-evidence.json"
_ADMISSION_HEARTBEAT_INTERVAL_SECONDS = 5.0
_ADMISSION_PROGRESS_LOG_INTERVAL_SECONDS = 60.0
_TARGET_STARTUP_HARD_TIMEOUT_SECONDS = 86_400.0
_TARGET_STARTUP_STAGES = frozenset(
    {
        "configuration",
        "policy_recovery",
        "catalog_validation",
        "admission",
        "data_ratchets",
        "management_recovery",
        "operational_runtime",
    }
)


class AutomaticUpgradeError(RuntimeError):
    """Raised after a failed automatic upgrade has restored its inputs."""


class _WorkingMigrationError(AutomaticUpgradeError):
    def __init__(self, message: str, evidence: dict[str, Any] | None = None) -> None:
        super().__init__(message)
        self.evidence = evidence


class _UpgradeHealthHandler(BaseHTTPRequestHandler):
    def __init__(self, *args: Any, health_path: str, **kwargs: Any) -> None:
        self.health_path = health_path
        super().__init__(*args, **kwargs)

    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        if self.path == self.health_path:
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
def _upgrade_health_server(port: int, base_path: str):
    handler = functools.partial(
        _UpgradeHealthHandler, health_path=f"{base_path}/health"
    )
    server = ThreadingHTTPServer(("0.0.0.0", port), handler)
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
    try:
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
    except OSError:
        # Docker Desktop Windows bind-mounts can flap with transient ENOENT
        # right after a rename; treat as not-yet-verified so the caller retries.
        return None
    return digest.hexdigest()


def _wait_for_content(path: Path, expected_sha256: str | None) -> bool:
    if expected_sha256 is None:
        # Fail-closed by design: an unverifiable expectation must never match,
        # not even an equally unreadable destination.
        return False
    for attempt in range(_PUBLISH_VERIFY_ATTEMPTS):
        try:
            if _sha256(path) == expected_sha256:
                return True
        except OSError:
            # Defense-in-depth (belt-and-braces: _sha256 already swallows
            # OSError): a transient read failure is a missed attempt, not an
            # abort of the verification budget.
            pass
        if attempt + 1 < _PUBLISH_VERIFY_ATTEMPTS:
            time.sleep(_PUBLISH_VERIFY_INTERVAL_SECONDS)
    return False


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


def _quick_check_failure(database: Path) -> str | None:
    """F2/H2: physical integrity gate for the working copy, read-only so the
    probe itself can never dirty a database it is about to vouch for."""
    try:
        with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as connection:
            rows = connection.execute("PRAGMA quick_check").fetchall()
    except sqlite3.Error as error:
        return f"the integrity probe failed: {error}"
    problems = [str(row[0]) for row in rows if str(row[0]) != "ok"]
    if not problems:
        return None
    return problems[0]


def _sqlite_backup(source: Path, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    with (
        sqlite3.connect(f"file:{source}?mode=ro", uri=True) as source_connection,
        sqlite3.connect(destination) as destination_connection,
    ):
        source_connection.backup(destination_connection)
    try:
        destination.chmod(source.stat().st_mode & 0o777)
        with destination.open("rb") as handle:
            os.fsync(handle.fileno())
    except OSError:
        # A bind-mount flap here must not leak FileNotFoundError: the content
        # verification in _replace_database stays authoritative and raises the
        # verifiable OSError if the bytes never settle.
        logger.warning("automatic_upgrade.backup_durability_fsync_unavailable")


def _write_state(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    try:
        atomic_write_json(path, payload)
    except OSError:
        logger.warning("automatic_upgrade.state_rename_failed_using_direct_write")
        _write_state_direct(path, payload)
    with path.open("rb") as handle:
        os.fsync(handle.fileno())
    _fsync_directory(path.parent)
    if _wait_for_state(path, payload):
        return
    logger.warning("automatic_upgrade.state_write_result_stale_using_direct_write")
    _write_state_direct(path, payload)
    with path.open("rb") as handle:
        os.fsync(handle.fileno())
    _fsync_directory(path.parent)
    if not _wait_for_state(path, payload):
        raise OSError(
            "The upgrade state file could not be verified after writing: " + str(path)
        )


def _write_state_direct(path: Path, payload: dict[str, Any]) -> None:
    path.write_text(json.dumps(payload), encoding="utf-8")


def _wait_for_state(path: Path, payload: dict[str, Any]) -> bool:
    for attempt in range(_PUBLISH_VERIFY_ATTEMPTS):
        if _read_state(path) == payload:
            return True
        if attempt + 1 < _PUBLISH_VERIFY_ATTEMPTS:
            time.sleep(_PUBLISH_VERIFY_INTERVAL_SECONDS)
    return False


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


def _copy_file_in_place(source: Path, destination: Path) -> None:
    try:
        with source.open("rb") as source_handle, destination.open(
            "wb"
        ) as target_handle:
            shutil.copyfileobj(source_handle, target_handle, length=1024 * 1024)
            target_handle.flush()
            os.fsync(target_handle.fileno())
        shutil.copystat(source, destination)
    except OSError as error:
        # A bind-mount flap mid-copy must surface as the verifiable install
        # failure, never as a bare FileNotFoundError from the copy itself.
        raise OSError(
            "The upgraded file could not be verified after installation: "
            + str(destination)
        ) from error
    _fsync_directory(destination.parent)


def _replace_file(source: Path, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_name(
        f".{destination.name}.upgrade-{uuid.uuid4().hex}.tmp"
    )
    try:
        staged = False
        for attempt in range(_PUBLISH_VERIFY_ATTEMPTS):
            try:
                with source.open("rb") as source_handle, temporary.open(
                    "wb"
                ) as target_handle:
                    shutil.copyfileobj(source_handle, target_handle, length=1024 * 1024)
                    target_handle.flush()
                    os.fsync(target_handle.fileno())
                shutil.copystat(source, temporary)
                staged = True
                break
            except OSError:
                # A source/temp-side flap retries within the verification budget;
                # exhaustion below surfaces as the verifiable install failure.
                if attempt + 1 < _PUBLISH_VERIFY_ATTEMPTS:
                    time.sleep(_PUBLISH_VERIFY_INTERVAL_SECONDS)
        if not staged:
            raise OSError(
                "The upgraded file could not be verified after installation: "
                + str(destination)
            )
        expected = _sha256(temporary)
        try:
            os.replace(temporary, destination)
        except OSError:
            logger.warning("automatic_upgrade.file_rename_failed_using_copy_fallback")
            renamed = False
        else:
            _fsync_directory(destination.parent)
            renamed = _wait_for_content(destination, expected)
            if not renamed:
                logger.warning(
                    "automatic_upgrade.file_rename_result_stale_using_copy_fallback"
                )
        if not renamed:
            if expected is None:
                # The staged temporary flapped while hashing; the source was
                # readable moments earlier, so re-anchor verification to it.
                expected = _sha256(source)
            if expected is None:
                # Still unverifiable: verification could never succeed, so fail
                # without touching the destination further.
                raise OSError(
                    "The upgraded file could not be verified after installation: "
                    + str(destination)
                )
            _copy_file_in_place(source, destination)
            if not _wait_for_content(destination, expected):
                raise OSError(
                    "The upgraded file could not be verified after installation: "
                    + str(destination)
                )
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
        expected = _sha256(temporary)
        # From stage `promoting` onward the manifest-verified backup is
        # authoritative: these sidecars are quarantined rather than destroyed
        # so a crash in this window leaves the previous WAL recoverable by
        # hand, while every restart path restores from the backup wholesale.
        quarantined = _quarantine_database_sidecars(destination)
        try:
            os.replace(temporary, destination)
        except OSError:
            logger.warning("automatic_upgrade.file_rename_failed_using_copy_fallback")
            renamed = False
        else:
            _fsync_directory(destination.parent)
            renamed = _wait_for_content(destination, expected)
            if not renamed:
                logger.warning(
                    "automatic_upgrade.file_rename_result_stale_using_copy_fallback"
                )
        if not renamed:
            if expected is None:
                # The staged temporary flapped while hashing; the source was
                # readable moments earlier, so re-anchor verification to it.
                expected = _sha256(source)
            if expected is None:
                # Still unverifiable: verification could never succeed, so fail
                # without touching the destination further.
                raise OSError(
                    "The upgraded library database could not be verified "
                    "after installation."
                )
            # truncate first: a backup onto an existing database rewrites header
            # counters, so it would never hash-match the temporary
            try:
                with destination.open("wb") as destination_handle:
                    destination_handle.truncate(0)
                    destination_handle.flush()
                    os.fsync(destination_handle.fileno())
                _sqlite_backup(source, destination)
            except OSError as error:
                # A bind-mount flap here must surface as the verifiable install
                # failure, never as a bare FileNotFoundError from the copy.
                raise OSError(
                    "The upgraded library database could not be verified "
                    "after installation."
                ) from error
            _fsync_directory(destination.parent)
            if not _wait_for_content(destination, expected):
                raise OSError(
                    "The upgraded library database could not be verified "
                    "after installation."
                )
        for sibling in quarantined:
            sibling.unlink(missing_ok=True)
    finally:
        temporary.unlink(missing_ok=True)


def _quarantine_database_sidecars(destination: Path) -> list[Path]:
    """Rename live -wal/-shm sidecars to sibling quarantine names instead of
    destroying them, so a crash before the swap preserves the previous WAL.
    Filesystems without atomic rename keep the legacy unlink behavior."""
    quarantined: list[Path] = []
    for suffix in ("-wal", "-shm"):
        sidecar = Path(f"{destination}{suffix}")
        if not sidecar.exists():
            continue
        sibling = destination.with_name(
            f".{destination.name}{suffix}.upgrade-{uuid.uuid4().hex}.quarantine"
        )
        try:
            os.replace(sidecar, sibling)
        except OSError:
            logger.warning("automatic_upgrade.database_sidecar_quarantine_unavailable")
            sidecar.unlink(missing_ok=True)
        else:
            quarantined.append(sibling)
    return quarantined


def _discard_quarantined_sidecars(destination: Path) -> None:
    """Sweep superseded quarantine siblings once a replacement or restored
    database is verified in place; leftovers from crashed windows die here."""
    for suffix in ("-wal", "-shm"):
        for sibling in destination.parent.glob(
            f".{destination.name}{suffix}.upgrade-*.quarantine"
        ):
            sibling.unlink(missing_ok=True)


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
    _discard_quarantined_sidecars(database)


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
    quick_check = _quick_check_failure(working_database)
    if quick_check is not None:
        raise AutomaticUpgradeError(
            "The upgraded library database failed its PRAGMA quick_check "
            f"integrity gate: {quick_check}"
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



def _write_unexpected_child_failure(
    error_type: str,
    reconciliation: Any,
    migrator: Any,
) -> None:
    """F3/F4: unexpected child failures leave sanitized evidence carrying the
    last batch cursor and the reconciliation outcome."""
    failure: dict[str, Any] = {
        "reason": "migration_failed",
        "error_type": error_type,
    }
    snapshot_source = getattr(migrator, "progress_snapshot", None)
    snapshot = snapshot_source() if callable(snapshot_source) else None
    if snapshot is not None:
        failure["batch_progress"] = snapshot
    if reconciliation is not None:
        reconciliation_evidence = reconciliation.evidence()
        if reconciliation_evidence is not None:
            failure["path_reconciliation"] = reconciliation_evidence
    try:
        _write_state(get_settings().cache_dir / _FAILURE_EVIDENCE_FILE, failure)
    except OSError:
        logger.error("automatic_upgrade.failure_state_write_failed")



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
    from services.native.legacy_path_reconciler import LegacyPathReconciler
    from services.native.target_startup_validator import TargetStartupValidator

    print("[upgrade] Preparing migrated settings and library roots.", flush=True)
    migrate_legacy_config()
    preferences = get_preferences_service()
    typed_settings = preferences.get_typed_library_settings()
    store = get_native_library_store()
    reconciliation: Any = None
    migrator: BoundedLegacyCatalogMigrator | None = None
    try:
        reconciler = LegacyPathReconciler(store, typed_settings)
        try:
            reconciliation = await reconciler.reconcile(
                emit_progress=lambda message: print(f"[upgrade] {message}", flush=True)
            )
        finally:
            await reconciler.aclose()
        if reconciliation.mode == "exact":
            preferences.retarget_library_roots_for_upgrade(
                dict(reconciliation.root_retargets)
            )
            get_library_policy_resolver.cache_clear()
        resolver = get_library_policy_resolver()
        if reconciliation.mode in {"exact", "remapped"}:
            print(
                "[upgrade] Reconciled legacy library paths "
                f"for {reconciliation.library_file_count:,} catalog files and "
                f"{reconciliation.review_row_count:,} review rows.",
                flush=True,
            )
        migrator = BoundedLegacyCatalogMigrator(
            store,
            resolver,
            emit_progress=lambda message: print(message, flush=True),
            path_projector=(
                reconciliation.project if reconciliation.mode == "remapped" else None
            ),
            skip_unmappable_paths=True,
        )
        outcome = await migrator.migrate(MIGRATION_ID)
        report = outcome.report
        if outcome.skipped_counts:
            skipped = ", ".join(
                f"{kind}={count:,}"
                for kind, count in sorted(outcome.skipped_counts.items())
            )
            print(
                f"[upgrade] Left {sum(outcome.skipped_counts.values()):,} legacy records "
                f"pending ({skipped}). Re-add their library roots and they will be "
                "imported automatically.",
                flush=True,
            )
        if outcome.blocker_count:
            blocker_reason_counts = {
                key: value
                for key, value in outcome.blocker_reason_counts.items()
                if value
            }
            failure_evidence: dict[str, Any] = {
                "reason": "unresolved_references",
                "blocker_count": outcome.blocker_count,
                "unresolved_reference_counts": {
                    count.kind: count.unresolved
                    for count in report.reference_counts
                    if count.user_id is None and count.unresolved
                },
                "blocker_reason_counts": blocker_reason_counts,
            }
            if outcome.blocker_details:
                failure_evidence["details"] = outcome.blocker_details
            reconciliation_evidence = reconciliation.evidence()
            if reconciliation_evidence is not None:
                failure_evidence["path_reconciliation"] = reconciliation_evidence
            _write_state(
                get_settings().cache_dir / _FAILURE_EVIDENCE_FILE,
                failure_evidence,
            )
            reference_summary = ", ".join(
                f"{count.kind}={count.unresolved:,}"
                for count in report.reference_counts
                if count.user_id is None and count.unresolved
            )
            reason_summary = ", ".join(
                f"{key}={value:,}"
                for key, value in sorted(blocker_reason_counts.items())
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
            blocked_error = AutomaticUpgradeError(
                "The existing library contains references that cannot be upgraded safely."
            )
            blocked_error.evidence = failure_evidence
            raise blocked_error
        if (
            report.embedded_art_reads
            or report.network_calls
            or report.tag_reads
            or report.fingerprints
        ):
            # F3/H3a: this abort previously raised bare, leaving no record of
            # which forbidden-work counter tripped.
            forbidden_failure: dict[str, Any] = {
                "reason": "forbidden_work",
                "network_calls": report.network_calls,
                "tag_reads": report.tag_reads,
                "fingerprints": report.fingerprints,
                "embedded_art_reads": report.embedded_art_reads,
            }
            reconciliation_evidence = reconciliation.evidence()
            if reconciliation_evidence is not None:
                forbidden_failure["path_reconciliation"] = reconciliation_evidence
            _write_state(
                get_settings().cache_dir / _FAILURE_EVIDENCE_FILE,
                forbidden_failure,
            )
            forbidden_error = AutomaticUpgradeError(
                "The library upgrade attempted work that is not allowed during startup."
            )
            forbidden_error.evidence = forbidden_failure
            raise forbidden_error
        print("[upgrade] Running independent target startup validation.", flush=True)
        validation = await TargetStartupValidator(
            get_native_library_store(),
            lambda: {root.id for root in resolver.settings.library_roots},
            emit_progress=lambda message: print(f"[upgrade] {message}", flush=True),
        ).validate("cutover")
    except Exception as error:  # noqa: BLE001 - every child failure leaves evidence
        if not isinstance(getattr(error, "evidence", None), dict):
            _write_unexpected_child_failure(
                type(error).__name__, reconciliation, migrator
            )
        raise
    print("[upgrade] Working-copy migration checks passed.", flush=True)
    settings = get_settings()
    provenance_counts = await store.get_migration_provenance_counts(MIGRATION_ID)
    evidence = {
        "source_revision": report.source_revision,
        "root_revision": report.root_revision,
        "reference_counts": dict(sorted(provenance_counts.items())),
        "invariants": validation["invariants"],
        "network_calls": report.network_calls,
        "tag_reads": report.tag_reads,
        "fingerprints": report.fingerprints,
        "embedded_art_reads": report.embedded_art_reads,
        "source_sha256": _sha256(settings.library_db_path),
        "config_sha256": _sha256(settings.config_file_path),
        "image_version": _image_version(),
    }
    if outcome.phase_timings_ms:
        evidence["phase_timings_ms"] = outcome.phase_timings_ms
    reconciliation_evidence = reconciliation.evidence()
    if reconciliation_evidence is not None:
        evidence["path_reconciliation"] = reconciliation_evidence
    if outcome.skipped_counts:
        evidence["skipped"] = dict(outcome.skipped_counts)
    return evidence


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
        failure_evidence = (
            state.get("failure_evidence") if isinstance(state, dict) else None
        )
        reason = (
            str(failure_evidence.get("reason"))
            if isinstance(failure_evidence, dict)
            and failure_evidence.get("reason") is not None
            else None
        )
        parts = [
            "A previous attempt by this image to upgrade the library failed. "
            "Your database and settings are unchanged."
        ]
        if reason is not None:
            parts.append(f"Failure reason: {reason}.")
        parts.append(f"More detail: {state_path}.")
        parts.append(
            "Switch back to your previous image to keep running, or install a "
            "corrected image, which retries the upgrade on startup. Fixing the "
            "failing data yourself and restarting also retries it."
        )
        raise AutomaticUpgradeError(" ".join(parts))

    print(
        "[upgrade] Preparing the library for this DroppedNeedle version. "
        "Very large libraries may take several hours. Keep the container running; "
        "progress will be logged.",
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
                # F3/H3c: never leave a bare `promoting` record behind - the
                # next boot must see that this install could not be restored.
                try:
                    _write_state(
                        state_path,
                        {
                            "format_version": 1,
                            "upgrade_id": UPGRADE_ID,
                            "stage": "promoting",
                            "image_version": image_version,
                            "backup_directory": str(backup.directory),
                            "restore_failed": True,
                            "restore_error_type": type(restore_error).__name__,
                        },
                    )
                except OSError:
                    logger.error("automatic_upgrade.failure_state_write_failed")
                raise AutomaticUpgradeError(
                    "The library upgrade failed and its backup could not be restored. "
                    "Do not start an older image against this database."
                ) from restore_error
        try:
            restored_signature = _current_signature(database, config)
        except OSError:
            # Defense-in-depth (_sha256 already swallows OSError): the live files
            # may vanish mid-failure on a flapping bind-mount; that must not mask
            # the original error or skip the state write.
            logger.warning("automatic_upgrade.failure_signature_unavailable")
            restored_signature = {"database_sha256": None, "config_sha256": None}
        failure = {
            "format_version": 1,
            "upgrade_id": UPGRADE_ID,
            "stage": "failed",
            "image_version": image_version,
            "backup_directory": str(backup.directory),
            "error_type": type(error).__name__,
            "error_message": str(error),
            "restored_signature": restored_signature,
        }
        failure_evidence = getattr(error, "evidence", None)
        if not isinstance(failure_evidence, dict):
            # F3/H3b: failures without their own evidence still record why.
            failure_evidence = {
                "reason": "unhandled_exception",
                "error_type": type(error).__name__,
            }
        failure["failure_evidence"] = failure_evidence
        _remove_working_copy(backup)
        try:
            _write_state(state_path, failure)
        except OSError:
            logger.error("automatic_upgrade.failure_state_write_failed")
        logger.error(
            "automatic_upgrade.failed error_type=%s error_message=%s",
            type(error).__name__,
            str(error),
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


def _target_ready(port: int, base_path: str) -> bool:
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=1)
    try:
        connection.request("GET", f"{base_path}/health")
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


def _admission_progress_path(settings: Settings, token: str) -> Path:
    validated_path, _admitted_path = _admission_paths(settings, token)
    return validated_path.with_name(f"{token}.progress.json")


def target_startup_admission_pending() -> bool:
    return bool(os.getenv(_ADMISSION_TOKEN_ENV, "").strip())


@asynccontextmanager
async def target_startup_progress(
    settings: Settings, stage: str
) -> AsyncIterator[None]:
    token = os.getenv(_ADMISSION_TOKEN_ENV, "").strip()
    if not token:
        yield
        return
    if stage not in _TARGET_STARTUP_STAGES:
        raise ValueError(f"Unsupported target startup progress stage: {stage}")
    path = _admission_progress_path(settings, token)
    started = time.monotonic()
    sequence = 0

    async def write_progress() -> None:
        nonlocal sequence
        sequence += 1
        await asyncio.to_thread(
            _write_state,
            path,
            {
                "format_version": 1,
                "upgrade_id": UPGRADE_ID,
                "token": token,
                "stage": stage,
                "sequence": sequence,
                "elapsed_seconds": round(time.monotonic() - started, 3),
            },
        )

    await write_progress()

    stopped = asyncio.Event()

    async def heartbeat() -> None:
        while not stopped.is_set():
            try:
                await asyncio.wait_for(
                    stopped.wait(), timeout=_ADMISSION_HEARTBEAT_INTERVAL_SECONDS
                )
            except TimeoutError:
                await write_progress()

    task = asyncio.create_task(heartbeat())
    try:
        yield
    finally:
        stopped.set()
        await task


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


def _target_progress(path: Path, token: str) -> dict[str, Any] | None:
    state = _read_state(path)
    if state is None or state.get("token") != token:
        return None
    stage = state.get("stage")
    sequence = state.get("sequence")
    elapsed_seconds = state.get("elapsed_seconds")
    if (
        stage not in _TARGET_STARTUP_STAGES
        or not isinstance(sequence, int)
        or isinstance(sequence, bool)
        or sequence < 1
        or not isinstance(elapsed_seconds, (int, float))
        or isinstance(elapsed_seconds, bool)
        or elapsed_seconds < 0
    ):
        return None
    return {
        "stage": str(stage),
        "sequence": sequence,
        "elapsed_seconds": float(elapsed_seconds),
    }


def _restore_after_target_startup_failure(
    settings: Settings,
    *,
    error_type: str,
    failure_evidence: dict[str, Any] | None = None,
) -> None:
    state_path = settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json"
    state = _read_state(state_path)
    if state is None or state.get("stage") != "promoted_pending_startup":
        return
    backup = _load_upgrade_backup(settings, state.get("backup_directory"))
    restore_upgrade_backup(settings, backup)
    _remove_working_copy(backup)
    failure = {
        "format_version": 1,
        "upgrade_id": UPGRADE_ID,
        "stage": "failed",
        "image_version": _image_version(),
        "backup_directory": str(backup.directory),
        "error_type": error_type,
        "restored_signature": _current_signature(
            settings.library_db_path, settings.config_file_path
        ),
    }
    if failure_evidence:
        failure["failure_evidence"] = failure_evidence
    _write_state(state_path, failure)


def _record_post_admission_startup_failure(
    settings: Settings,
    *,
    error_type: str,
    last_stage: str,
    startup_started: float,
    returncode: int | None,
) -> None:
    """F8/H8 ops semantics: a post-admission target startup failure
    deliberately keeps ``stage="completed"`` - the upgraded database is KEPT,
    not rolled back - and appends a ``target_startup_failure`` evidence
    object to the state record instead of flipping the stage to ``failed``.
    Operators must inspect the full state record (or grep for
    ``target_startup_failure``): grepping for ``failed`` misses this failure
    class entirely. A later healthy boot clears the flag via
    ``_clear_post_admission_startup_failure``, returning the record to a
    plain completed state."""
    state_path = settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json"
    state = _read_state(state_path)
    if state is None or state.get("stage") != "completed":
        return
    evidence: dict[str, Any] = {
        "error_type": error_type,
        "last_stage": last_stage,
        "elapsed_seconds": round(time.monotonic() - startup_started, 3),
    }
    if returncode is not None:
        evidence["returncode"] = returncode
    try:
        _write_state(state_path, {**state, "target_startup_failure": evidence})
    except OSError:
        logger.error("automatic_upgrade.target_start_failure_state_write_failed")


def _clear_post_admission_startup_failure(settings: Settings) -> None:
    state_path = settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json"
    state = _read_state(state_path)
    if (
        state is None
        or state.get("stage") != "completed"
        or "target_startup_failure" not in state
    ):
        return
    updated = dict(state)
    updated.pop("target_startup_failure")
    try:
        _write_state(state_path, updated)
    except OSError:
        logger.error("automatic_upgrade.target_start_failure_state_clear_failed")


def _terminate_target_process(process: Any) -> None:
    if process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


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
    progress_path: Path | None = None
    environment = os.environ.copy()
    if admission_pending:
        validated_path, admitted_path = _admission_paths(settings, token)
        progress_path = _admission_progress_path(settings, token)
        validated_path.unlink(missing_ok=True)
        admitted_path.unlink(missing_ok=True)
        progress_path.unlink(missing_ok=True)
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
    startup_started = time.monotonic()
    hard_deadline = startup_started + _TARGET_STARTUP_HARD_TIMEOUT_SECONDS
    last_progress_key: tuple[str, int] | None = None
    last_stage = "process_start"
    last_progress_log = startup_started

    def observe_progress(idle_deadline: float) -> float:
        nonlocal last_progress_key, last_progress_log, last_stage
        if progress_path is None:
            return idle_deadline
        progress = _target_progress(progress_path, token)
        if progress is None:
            return idle_deadline
        key = (str(progress["stage"]), int(progress["sequence"]))
        now = time.monotonic()
        if key != last_progress_key:
            idle_deadline = now + admission_timeout_seconds
            last_progress_key = key
        stage = str(progress["stage"])
        if stage != last_stage:
            last_stage = stage
            last_progress_log = now
            print(
                f"[upgrade] DroppedNeedle startup: {stage.replace('_', ' ')}.",
                flush=True,
            )
        elif now - last_progress_log >= _ADMISSION_PROGRESS_LOG_INTERVAL_SECONDS:
            last_progress_log = now
            print(
                "[upgrade] DroppedNeedle startup is still working: "
                f"{stage.replace('_', ' ')} "
                f"({time.monotonic() - startup_started:,.0f}s elapsed).",
                flush=True,
            )
        return idle_deadline

    try:
        if admission_pending:
            assert validated_path is not None
            assert admitted_path is not None
            idle_deadline = time.monotonic() + admission_timeout_seconds
            while True:
                if process.poll() is not None:
                    admission_error = "TargetProcessExited"
                    break
                idle_deadline = observe_progress(idle_deadline)
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
                        admission_state = _read_state(
                            settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json"
                        )
                        promotion_committed = (
                            admission_state is not None
                            and admission_state.get("stage") == "completed"
                        )
                        break
                    target_released = True
                    break
                now = time.monotonic()
                if now >= hard_deadline:
                    admission_error = "TargetStartupHardTimeout"
                    break
                if now >= idle_deadline:
                    admission_error = "TargetStartupTimeout"
                    break
                time.sleep(0.05)

        if promotion_committed and not target_released:
            _terminate_target_process(process)
            _record_post_admission_startup_failure(
                settings,
                error_type=admission_error,
                last_stage=last_stage,
                startup_started=startup_started,
                returncode=process.returncode,
            )
            print(
                "[upgrade] ERROR: The library upgrade was installed, but DroppedNeedle "
                "could not continue startup. Restart DroppedNeedle. If the problem "
                "repeats, install a newer image.",
                flush=True,
            )
            return 1

        if promotion_committed:
            readiness_idle_deadline = time.monotonic() + admission_timeout_seconds
            target_became_ready = False
            while process.poll() is None:
                readiness_idle_deadline = observe_progress(readiness_idle_deadline)
                if _target_ready(port, settings.base_path):
                    target_became_ready = True
                    _clear_post_admission_startup_failure(settings)
                    if progress_path is not None:
                        progress_path.unlink(missing_ok=True)
                    print(
                        "[upgrade] Library upgrade complete. DroppedNeedle is ready.",
                        flush=True,
                    )
                    break
                now = time.monotonic()
                hard_timeout = now >= hard_deadline
                idle_timeout = (
                    progress_path is not None and now >= readiness_idle_deadline
                )
                if hard_timeout or idle_timeout:
                    error_type = (
                        "TargetStartupHardTimeout"
                        if hard_timeout
                        else "TargetReadinessTimeout"
                    )
                    _terminate_target_process(process)
                    _record_post_admission_startup_failure(
                        settings,
                        error_type=error_type,
                        last_stage=last_stage,
                        startup_started=startup_started,
                        returncode=process.returncode,
                    )
                    if hard_timeout:
                        print(
                            "[upgrade] ERROR: The library upgrade was installed, but "
                            "DroppedNeedle startup exceeded the safety time limit during "
                            f"{last_stage.replace('_', ' ')}. Restart DroppedNeedle. If "
                            "the problem repeats, install a newer image.",
                            flush=True,
                        )
                    else:
                        print(
                            "[upgrade] ERROR: The library upgrade was installed, but "
                            "DroppedNeedle stopped making startup progress during "
                            f"{last_stage.replace('_', ' ')}. Restart DroppedNeedle. If "
                            "the problem repeats, install a newer image.",
                            flush=True,
                        )
                    return 1
                time.sleep(0.25)
            exit_code = process.wait()
            if target_became_ready or forwarded_signal is not None:
                return exit_code
            _record_post_admission_startup_failure(
                settings,
                error_type="TargetProcessExitedBeforeReadiness",
                last_stage=last_stage,
                startup_started=startup_started,
                returncode=exit_code,
            )
            print(
                "[upgrade] ERROR: DroppedNeedle exited before it was ready "
                f"during {last_stage.replace('_', ' ')} (exit code {exit_code}).",
                flush=True,
            )
            return exit_code or 1

        _terminate_target_process(process)
        try:
            elapsed_seconds = round(time.monotonic() - startup_started, 3)
            failure_evidence: dict[str, Any] = {
                "last_stage": last_stage,
                "elapsed_seconds": elapsed_seconds,
            }
            if process.returncode is not None:
                failure_evidence["returncode"] = process.returncode
            _restore_after_target_startup_failure(
                settings,
                error_type=admission_error,
                failure_evidence=failure_evidence,
            )
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
                "[upgrade] ERROR: DroppedNeedle startup failed during "
                f"{last_stage.replace('_', ' ')} ({admission_error}) after "
                f"{time.monotonic() - startup_started:,.0f}s. The previous database "
                "and settings were restored.",
                flush=True,
            )
        return process.returncode or 1
    finally:
        if validated_path is not None:
            validated_path.unlink(missing_ok=True)
        if admitted_path is not None:
            admitted_path.unlink(missing_ok=True)
        if progress_path is not None:
            progress_path.unlink(missing_ok=True)
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
            with _upgrade_health_server(
                _container_port(settings), settings.base_path
            ):
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
