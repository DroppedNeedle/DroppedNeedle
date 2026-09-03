"""Hostile-filesystem publish tests for the automatic upgrade path.

Deterministic tmp_path-only suite: every test zeroes the publish
intervals (no real sleeps) and drives failures with monkeypatch,
asserting observable behavior (return values, file bytes, raised
messages) rather than implementation details.
"""

import errno
import os
import sqlite3
import stat
from pathlib import Path

import pytest

import maintenance.automatic_upgrade as automatic_upgrade
from core.config import Settings


def _settings(root: Path) -> Settings:
    return Settings(
        root_app_dir=root,
        cache_dir=root / "cache",
        library_db_path=root / "cache" / "library.db",
        config_file_path=root / "config" / "config.json",
    )


def _patch_publish_intervals(monkeypatch: pytest.MonkeyPatch) -> None:
    """Zero both publish verification budgets so the suite never sleeps."""
    monkeypatch.setattr(automatic_upgrade, "_PUBLISH_VERIFY_INTERVAL_SECONDS", 0)
    monkeypatch.setattr(automatic_upgrade, "_PUBLISH_SETTLE_INTERVAL_SECONDS", 0)


def _write_unmigrated_database(path: Path, value: str = "original") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(path) as connection:
        connection.execute("CREATE TABLE source_value (value TEXT NOT NULL)")
        connection.execute("INSERT INTO source_value VALUES (?)", (value,))


def _mark_migrated(path: Path) -> None:
    with sqlite3.connect(path) as connection:
        connection.execute(
            "CREATE TABLE IF NOT EXISTS library_migration_markers "
            "(marker TEXT PRIMARY KEY)"
        )
        connection.execute(
            "INSERT OR REPLACE INTO library_migration_markers VALUES "
            "('legacy_catalog_import_complete')"
        )


def _source_value(path: Path) -> str:
    with sqlite3.connect(path) as connection:
        return str(connection.execute("SELECT value FROM source_value").fetchone()[0])


def _has_marker(path: Path) -> bool:
    if not path.is_file():
        return False
    try:
        with sqlite3.connect(path) as connection:
            row = connection.execute(
                "SELECT 1 FROM library_migration_markers "
                "WHERE marker = 'legacy_catalog_import_complete'"
            ).fetchone()
    except sqlite3.Error:
        return False
    return row is not None


def test_stale_read_after_rename_recovers_within_budget(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The rename lands but the first N destination reads hash stale; the
    publish retries past the stale window and succeeds with exact bytes."""
    source = tmp_path / "source.txt"
    destination = tmp_path / "destination.txt"
    source.write_bytes(b"published-content")
    _patch_publish_intervals(monkeypatch)
    real_sha256 = automatic_upgrade._sha256
    stale_reads = 3
    destination_reads = 0

    def flaky_sha256(path: Path) -> str | None:
        nonlocal destination_reads
        if Path(path) == destination:
            destination_reads += 1
            if destination_reads <= stale_reads:
                return "stale-bytes"
        return real_sha256(Path(path))

    monkeypatch.setattr(automatic_upgrade, "_sha256", flaky_sha256)

    automatic_upgrade._replace_file(source, destination)

    assert destination.read_bytes() == b"published-content"
    assert destination_reads > stale_reads


def test_permanently_stale_after_rename_fails_closed_without_copy(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The rename lands but destination reads stay stale past the settle
    budget: a verifiable OSError, the copy engine never runs, and the
    installed bytes are kept non-zero."""
    source = tmp_path / "source.db"
    destination = tmp_path / "destination.db"
    _write_unmigrated_database(source)
    _mark_migrated(source)
    _write_unmigrated_database(destination, value="outdated")
    _patch_publish_intervals(monkeypatch)
    real_sha256 = automatic_upgrade._sha256

    def always_stale(path: Path) -> str | None:
        if Path(path) == destination:
            return "stale-bytes"
        return real_sha256(Path(path))

    monkeypatch.setattr(automatic_upgrade, "_sha256", always_stale)
    copy_calls: list[tuple[str, str]] = []

    def tripwire_copy(source_path: Path, destination_path: Path) -> None:
        copy_calls.append((str(source_path), str(destination_path)))
        raise AssertionError("stale-after-success must never copy")

    monkeypatch.setattr(
        automatic_upgrade, "_copy_database_bytes_in_place", tripwire_copy
    )
    monkeypatch.setattr(automatic_upgrade, "_overwrite_bytes_in_place", tripwire_copy)

    with pytest.raises(OSError) as exc_info:
        automatic_upgrade._replace_database(source, destination)

    assert (
        str(exc_info.value)
        == "The upgraded library database could not be verified after installation."
    )
    assert not isinstance(exc_info.value, FileNotFoundError)
    assert copy_calls == []
    assert destination.stat().st_size > 0
    assert _source_value(destination) == "original"
    assert _has_marker(destination)


def test_locked_handle_retry_reaches_success_without_zero_byte_live_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """os.replace raises PermissionError once (Windows sharing-violation
    shape) then succeeds: the publish completes and the live file keeps its
    new bytes, never an empty file."""
    source = tmp_path / "source.db"
    destination = tmp_path / "destination.db"
    _write_unmigrated_database(source)
    _mark_migrated(source)
    _write_unmigrated_database(destination, value="outdated")
    _patch_publish_intervals(monkeypatch)
    real_replace = os.replace
    replace_calls: list[tuple[str, str]] = []

    def locked_once(
        src: object, dst: object, *args: object, **kwargs: object
    ) -> None:
        replace_calls.append((str(src), str(dst)))
        if len(replace_calls) == 1:
            raise PermissionError(errno.EACCES, "sharing violation", str(dst))
        real_replace(src, dst)  # type: ignore[arg-type]

    monkeypatch.setattr(automatic_upgrade.os, "replace", locked_once)

    automatic_upgrade._replace_database(source, destination)

    # One rename attempt (locked), then the no-truncate copy fallback installs
    # the bytes: success after a failed rename proves the fallback ran.
    assert len(replace_calls) == 1
    assert destination.stat().st_size > 0
    assert _source_value(destination) == "original"
    assert _has_marker(destination)


def test_locked_sidecar_quarantine_fails_closed_and_retains_wal(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A locked -wal cannot be quarantined: publish fails closed, the live
    WAL is retained byte-identical (never deleted), and the successfully
    quarantined sibling is kept rather than swept."""
    source = tmp_path / "source.db"
    destination = tmp_path / "destination.db"
    _write_unmigrated_database(source)
    _mark_migrated(source)
    _write_unmigrated_database(destination, value="outdated")
    live_wal = Path(f"{destination}-wal")
    live_wal.write_bytes(b"wal-frames")
    live_shm = Path(f"{destination}-shm")
    live_shm.write_bytes(b"shm-frames")
    _patch_publish_intervals(monkeypatch)
    real_replace = os.replace

    def locked_wal(
        src: object, dst: object, *args: object, **kwargs: object
    ) -> object:
        if str(src).endswith("-wal"):
            raise PermissionError(errno.EACCES, "file locked", str(src))
        return real_replace(src, dst)  # type: ignore[arg-type]

    monkeypatch.setattr(automatic_upgrade.os, "replace", locked_wal)

    with pytest.raises(OSError) as exc_info:
        automatic_upgrade._replace_database(source, destination)

    assert (
        str(exc_info.value)
        == "The upgraded library database could not be verified after installation."
    )
    assert not isinstance(exc_info.value, FileNotFoundError)
    assert live_wal.read_bytes() == b"wal-frames"
    assert not live_shm.exists()
    quarantined = list(
        destination.parent.glob(f".{destination.name}-shm.upgrade-*.quarantine")
    )
    assert len(quarantined) == 1
    assert quarantined[0].read_bytes() == b"shm-frames"


def test_torn_read_flaps_recover_to_success(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Destination reads fail outright K times (a torn/unreadable window),
    then verify: the flap is absorbed by the budget and bytes land intact."""
    source = tmp_path / "source.txt"
    destination = tmp_path / "destination.txt"
    source.write_bytes(b"published-content")
    _patch_publish_intervals(monkeypatch)
    real_sha256 = automatic_upgrade._sha256
    torn_reads = 3
    destination_reads = 0

    def torn_sha256(path: Path) -> str | None:
        nonlocal destination_reads
        if Path(path) == destination:
            destination_reads += 1
            if destination_reads <= torn_reads:
                raise OSError(errno.EIO, "torn read", str(path))
        return real_sha256(Path(path))

    monkeypatch.setattr(automatic_upgrade, "_sha256", torn_sha256)

    automatic_upgrade._replace_file(source, destination)

    assert destination.read_bytes() == b"published-content"
    assert destination_reads > torn_reads


def test_torn_destination_that_never_settles_fails_closed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The installed destination stays unreadable past the budget: a
    verifiable OSError, the installed bytes kept non-zero, and no fallback
    copy overwriting them."""
    source = tmp_path / "source.db"
    destination = tmp_path / "destination.db"
    _write_unmigrated_database(source)
    _mark_migrated(source)
    _write_unmigrated_database(destination, value="outdated")
    _patch_publish_intervals(monkeypatch)
    real_sha256 = automatic_upgrade._sha256

    def always_torn(path: Path) -> str | None:
        if Path(path) == destination:
            return None
        return real_sha256(Path(path))

    monkeypatch.setattr(automatic_upgrade, "_sha256", always_torn)
    copy_calls: list[tuple[str, str]] = []

    def tripwire_copy(source_path: Path, destination_path: Path) -> None:
        copy_calls.append((str(source_path), str(destination_path)))
        raise AssertionError("unreadable-after-success must never copy")

    monkeypatch.setattr(
        automatic_upgrade, "_copy_database_bytes_in_place", tripwire_copy
    )
    monkeypatch.setattr(automatic_upgrade, "_overwrite_bytes_in_place", tripwire_copy)

    with pytest.raises(OSError) as exc_info:
        automatic_upgrade._replace_database(source, destination)

    assert (
        str(exc_info.value)
        == "The upgraded library database could not be verified after installation."
    )
    assert not isinstance(exc_info.value, FileNotFoundError)
    assert copy_calls == []
    assert destination.stat().st_size > 0
    assert _source_value(destination) == "original"
    assert _has_marker(destination)


def test_fsync_and_copystat_eperm_still_publishes(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Directory-fsync EPERM plus copystat EPERM are metadata-only failures:
    the file publish still settles with exact bytes."""
    source = tmp_path / "source.txt"
    destination = tmp_path / "nested" / "destination.txt"
    source.write_bytes(b"published-content")
    _patch_publish_intervals(monkeypatch)
    real_fsync = os.fsync
    dir_fsync_hits: list[int] = []

    def hostile_fsync(fd: int) -> None:
        try:
            is_dir = stat.S_ISDIR(os.fstat(fd).st_mode)
        except OSError:
            real_fsync(fd)
            return
        if is_dir:
            dir_fsync_hits.append(fd)
            raise PermissionError(errno.EPERM, "directory fsync unavailable")
        real_fsync(fd)

    monkeypatch.setattr(automatic_upgrade.os, "fsync", hostile_fsync)
    copystat_calls: list[tuple[str, str]] = []

    def hostile_copystat(src: object, dst: object) -> None:
        copystat_calls.append((str(src), str(dst)))
        raise PermissionError(errno.EPERM, "metadata unavailable")

    monkeypatch.setattr(automatic_upgrade.shutil, "copystat", hostile_copystat)

    automatic_upgrade._replace_file(source, destination)

    assert destination.read_bytes() == b"published-content"
    assert copystat_calls != []
    assert dir_fsync_hits != []


def test_wal_reappearance_beside_verified_database_fails_closed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A -wal recreated beside the hash-matched database after the swap is
    invisible to content checks, so publish fails closed; the WAL is
    retained, and the raised message stays generic with no paths."""
    source = tmp_path / "source.db"
    destination = tmp_path / "destination.db"
    _write_unmigrated_database(source)
    _mark_migrated(source)
    _write_unmigrated_database(destination, value="outdated")
    _patch_publish_intervals(monkeypatch)
    real_replace = os.replace

    def reappearing_replace(
        src: object, dst: object, *args: object, **kwargs: object
    ) -> object:
        result = real_replace(src, dst)  # type: ignore[arg-type]
        if Path(str(dst)) == destination:
            Path(f"{destination}-wal").write_bytes(b"reappeared-wal-frames")
        return result

    monkeypatch.setattr(automatic_upgrade.os, "replace", reappearing_replace)

    with pytest.raises(OSError) as exc_info:
        automatic_upgrade._replace_database(source, destination)

    assert (
        str(exc_info.value)
        == "The upgraded library database could not be verified after installation."
    )
    assert not isinstance(exc_info.value, FileNotFoundError)
    assert str(tmp_path) not in str(exc_info.value)
    assert Path(f"{destination}-wal").read_bytes() == b"reappeared-wal-frames"
    assert destination.stat().st_size > 0
    assert _source_value(destination) == "original"


def test_fresh_bootstrap_reaches_ready_without_replace(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Starting from an absent live database, the fresh install upgrades;
    the follow-up boot reports ready without invoking os.replace or the
    migration runner."""
    settings = _settings(tmp_path)
    assert not settings.library_db_path.exists()
    _patch_publish_intervals(monkeypatch)

    def migrate(working: Path) -> dict[str, object]:
        working_database = working / "cache" / "library.db"
        _write_unmigrated_database(working_database, "migrated")
        _mark_migrated(working_database)
        (working / "config" / "config.json").write_text(
            '{"name":"after"}', encoding="utf-8"
        )
        return {"passed": True}

    assert (
        automatic_upgrade.run_automatic_copy_upgrade(settings, runner=migrate)
        == "upgraded"
    )
    assert _source_value(settings.library_db_path) == "migrated"
    assert _has_marker(settings.library_db_path)
    assert settings.library_db_path.stat().st_size > 0
    assert settings.config_file_path.read_text(encoding="utf-8") == '{"name":"after"}'

    def forbidden_replace(
        src: object, dst: object, *args: object, **kwargs: object
    ) -> None:
        raise AssertionError("ready path must not replace")

    def unexpected_runner(working: Path) -> dict[str, object]:
        raise AssertionError("migration should not run")

    monkeypatch.setattr(automatic_upgrade.os, "replace", forbidden_replace)

    assert (
        automatic_upgrade.run_automatic_copy_upgrade(settings, runner=unexpected_runner)
        == "ready"
    )
    assert _source_value(settings.library_db_path) == "migrated"
