import errno
import json
import os
import shutil
import socket
import sqlite3
import subprocess
import sys
import asyncio
import threading
import time
from contextlib import contextmanager
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import HTTPError
from urllib.request import urlopen

import pytest
from unittest.mock import AsyncMock, MagicMock

import maintenance.automatic_upgrade as automatic_upgrade

from core.config import Settings
from maintenance.automatic_upgrade import (
    AutomaticUpgradeError,
    UPGRADE_ID,
    _target_ready,
    _upgrade_health_server,
    run_automatic_copy_upgrade,
    run_target_supervisor,
)
from tests.infrastructure.test_legacy_catalog_importer import _create_source


def _settings(root: Path) -> Settings:
    return Settings(
        root_app_dir=root,
        cache_dir=root / "cache",
        library_db_path=root / "cache" / "library.db",
        config_file_path=root / "config" / "config.json",
    )


def _free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return int(probe.getsockname()[1])


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


def test_upgrade_backs_up_and_marks_existing_installation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)
    settings.config_file_path.parent.mkdir(parents=True)
    settings.config_file_path.write_text('{"name":"before"}', encoding="utf-8")
    monkeypatch.setenv("COMMIT_TAG", "test-version")

    def migrate(working: Path) -> dict[str, object]:
        working_database = working / "cache" / "library.db"
        with sqlite3.connect(working_database) as connection:
            connection.execute("UPDATE source_value SET value = 'migrated'")
        (working / "config" / "config.json").write_text(
            '{"name":"after"}', encoding="utf-8"
        )
        _mark_migrated(working_database)
        return {"passed": True}

    result = run_automatic_copy_upgrade(settings, runner=migrate)

    assert result == "upgraded"
    assert _source_value(settings.library_db_path) == "migrated"
    state = json.loads(
        (settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    backup = Path(state["backup_directory"])
    assert state["stage"] == "completed"
    assert _source_value(backup / "library.db") == "original"
    assert (backup / "config.json").read_text(encoding="utf-8") == '{"name":"before"}'
    assert not (backup / "working").exists()


def test_backup_captures_committed_wal_rows(tmp_path: Path) -> None:
    settings = _settings(tmp_path)
    settings.library_db_path.parent.mkdir(parents=True)
    with sqlite3.connect(settings.library_db_path) as connection:
        assert connection.execute("PRAGMA journal_mode=WAL").fetchone()[0] == "wal"
        connection.execute("CREATE TABLE wal_value (value TEXT NOT NULL)")
        connection.execute("INSERT INTO wal_value VALUES ('committed')")
        connection.commit()
        assert Path(f"{settings.library_db_path}-wal").is_file()

        backup = automatic_upgrade.capture_upgrade_backup(settings)

        with sqlite3.connect(backup.database) as copied:
            assert (
                copied.execute("SELECT value FROM wal_value").fetchone()[0]
                == "committed"
            )


def test_failed_working_copy_keeps_database_and_settings_and_does_not_retry(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)
    settings.config_file_path.parent.mkdir(parents=True)
    settings.config_file_path.write_text('{"name":"before"}', encoding="utf-8")
    monkeypatch.setenv("COMMIT_TAG", "broken-version")
    attempts = 0

    def unexpected_restore(_settings: Settings, _backup: object) -> None:
        raise AssertionError("source restore is unnecessary before promotion")

    monkeypatch.setattr(automatic_upgrade, "restore_upgrade_backup", unexpected_restore)

    def fail(working: Path) -> dict[str, object]:
        nonlocal attempts
        attempts += 1
        with sqlite3.connect(working / "cache" / "library.db") as connection:
            connection.execute("UPDATE source_value SET value = 'partial'")
            connection.execute("CREATE TABLE partial_target (id INTEGER)")
        (working / "config" / "config.json").write_text(
            '{"name":"partial"}', encoding="utf-8"
        )
        raise RuntimeError("simulated failure")

    with pytest.raises(AutomaticUpgradeError, match="previous database"):
        run_automatic_copy_upgrade(settings, runner=fail)

    assert _source_value(settings.library_db_path) == "original"
    assert settings.config_file_path.read_text(encoding="utf-8") == '{"name":"before"}'
    with sqlite3.connect(settings.library_db_path) as connection:
        assert (
            connection.execute(
                "SELECT 1 FROM sqlite_master WHERE name = 'partial_target'"
            ).fetchone()
            is None
        )

    with pytest.raises(AutomaticUpgradeError, match="previous attempt by this image"):
        run_automatic_copy_upgrade(settings, runner=fail)
    assert attempts == 1


def test_failed_working_migration_records_sanitized_reference_counts(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)
    monkeypatch.setenv("COMMIT_TAG", "diagnostic-version")
    evidence = {
        "reason": "unresolved_references",
        "blocker_count": 3,
        "unresolved_reference_counts": {"history": 1, "playlist_track": 2},
    }

    def fail(_working: Path) -> dict[str, object]:
        raise automatic_upgrade._WorkingMigrationError("checked failure", evidence)

    with pytest.raises(AutomaticUpgradeError, match="previous database"):
        run_automatic_copy_upgrade(settings, runner=fail)

    state = json.loads(
        (settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    assert state["failure_evidence"] == evidence
    assert state["error_type"] == "_WorkingMigrationError"
    assert state["error_message"] == "checked failure"
    assert "source_key" not in json.dumps(state["failure_evidence"])


def test_working_process_failure_reads_aggregate_evidence(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    working = tmp_path / "working"
    cache = working / "cache"
    cache.mkdir(parents=True)
    evidence = {
        "reason": "unresolved_references",
        "blocker_count": 2,
        "unresolved_reference_counts": {"jellyfin_id_map": 2},
    }
    automatic_upgrade._write_state(
        cache / automatic_upgrade._FAILURE_EVIDENCE_FILE, evidence
    )
    monkeypatch.setattr(
        automatic_upgrade.subprocess,
        "run",
        lambda *args, **kwargs: subprocess.CompletedProcess(args[0], 1, "", ""),
    )

    with pytest.raises(automatic_upgrade._WorkingMigrationError) as error:
        automatic_upgrade._run_working_migration(working)

    assert error.value.evidence == evidence


def test_killed_working_process_records_sanitized_exit_evidence(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    working = tmp_path / "working"
    (working / "cache").mkdir(parents=True)
    monkeypatch.setattr(
        automatic_upgrade.subprocess,
        "run",
        lambda *args, **kwargs: subprocess.CompletedProcess(args[0], -9),
    )

    with pytest.raises(automatic_upgrade._WorkingMigrationError) as error:
        automatic_upgrade._run_working_migration(working)

    assert error.value.evidence == {
        "reason": "working_process_exited",
        "returncode": -9,
    }


def test_working_process_records_sanitized_exception_type(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)

    async def fail() -> dict[str, object]:
        raise MemoryError("private allocation detail")

    monkeypatch.setattr(sys, "argv", ["automatic_upgrade", "--migrate-working"])
    monkeypatch.setattr(automatic_upgrade, "get_settings", lambda: settings)
    monkeypatch.setattr(automatic_upgrade, "_perform_target_migration", fail)

    assert automatic_upgrade.main() == 1
    evidence = json.loads(
        (settings.cache_dir / automatic_upgrade._FAILURE_EVIDENCE_FILE).read_text()
    )
    assert evidence == {
        "reason": "working_migration_error",
        "error_type": "MemoryError",
    }
    assert "private allocation detail" not in json.dumps(evidence)


def test_failed_fresh_install_removes_partially_created_files(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    monkeypatch.setenv("COMMIT_TAG", "fresh-broken")

    def fail(working: Path) -> dict[str, object]:
        _write_unmigrated_database(working / "cache" / "library.db", "partial")
        (working / "config" / "config.json").write_text("{}", encoding="utf-8")
        raise RuntimeError("simulated failure")

    with pytest.raises(AutomaticUpgradeError, match="previous database"):
        run_automatic_copy_upgrade(settings, runner=fail)

    assert not settings.library_db_path.exists()
    assert not settings.config_file_path.exists()


def test_completed_installation_skips_migration(tmp_path: Path) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)
    _mark_migrated(settings.library_db_path)

    def unexpected(_working: Path) -> dict[str, object]:
        raise AssertionError("migration should not run")

    assert run_automatic_copy_upgrade(settings, runner=unexpected) == "ready"
    assert not (settings.cache_dir / "upgrade-backups").exists()


@pytest.mark.parametrize("damage", ["missing", "zeroed"])
def test_completed_installation_refuses_missing_or_zeroed_target_database(
    tmp_path: Path, damage: str
) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)

    def migrate(working: Path) -> dict[str, object]:
        _mark_migrated(working / "cache" / "library.db")
        return {"passed": True}

    assert run_automatic_copy_upgrade(settings, runner=migrate) == "upgraded"
    settings.library_db_path.unlink()
    if damage == "zeroed":
        settings.library_db_path.touch()

    with pytest.raises(AutomaticUpgradeError, match="upgraded previously"):
        run_automatic_copy_upgrade(settings, runner=migrate)


def test_verified_legacy_backup_can_be_rolled_forward_again(tmp_path: Path) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)

    def migrate(working: Path) -> dict[str, object]:
        _mark_migrated(working / "cache" / "library.db")
        return {"passed": True}

    assert run_automatic_copy_upgrade(settings, runner=migrate) == "upgraded"
    state = json.loads(
        (settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    backup = automatic_upgrade._load_upgrade_backup(settings, state["backup_directory"])
    automatic_upgrade.restore_upgrade_backup(settings, backup)

    assert run_automatic_copy_upgrade(settings, runner=migrate) == "upgraded"


def _run_real_upgrade(root: Path) -> subprocess.CompletedProcess[str]:
    environment = os.environ.copy()
    environment.update(
        {
            "ROOT_APP_DIR": str(root),
            "DATA_ENC_KEY": "bm90LWEtcmVhbC1rZXktZm9yLXRlc3RzLW9ubHk=",
            "COMMIT_TAG": "automatic-upgrade-test",
        }
    )
    return subprocess.run(
        [sys.executable, "-m", "maintenance.automatic_upgrade"],
        cwd=Path(__file__).parents[2],
        env=environment,
        check=False,
        capture_output=True,
        text=True,
        timeout=60,
    )


def _insert_identityless_legacy_files(
    database: Path, music: Path, *, count: int
) -> list[str]:
    ids = [f"99999999-9999-4999-8999-{index:012d}" for index in range(count)]
    with sqlite3.connect(database) as connection:
        connection.executemany(
            "INSERT INTO library_files "
            "(id, release_group_mbid, release_mbid, recording_mbid, disc_number, "
            "track_number, track_title, artist_name, album_artist_name, album_title, "
            "file_path, file_size_bytes, file_mtime, duration_seconds, file_format, "
            "source, is_compilation, tagged_at, imported_at) "
            "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            [
                (
                    file_id,
                    None,
                    None,
                    None,
                    1,
                    index + 1,
                    f"Local Track {index + 1}",
                    "Local Artist",
                    "Local Artist",
                    "Identityless Album",
                    str(music / "Identityless Album" / f"{index + 1:02d}.flac"),
                    1_000 + index,
                    20.0 + index,
                    180.0,
                    "flac",
                    "manual_review",
                    0,
                    21.0,
                    20.0,
                )
                for index, file_id in enumerate(ids)
            ],
        )
    return ids


def test_real_legacy_installation_upgrades_once_with_normal_startup(
    tmp_path: Path,
) -> None:
    root = tmp_path / "app"
    music = tmp_path / "Music"
    music.mkdir(parents=True)
    database = root / "cache" / "library.db"
    database.parent.mkdir(parents=True)
    _create_source(database, music)
    config = root / "config" / "config.json"
    config.parent.mkdir(parents=True)
    config.write_text(
        json.dumps({"library_settings": {"library_paths": [str(music)]}}),
        encoding="utf-8",
    )

    first = _run_real_upgrade(root)
    second = _run_real_upgrade(root)

    assert first.returncode == 0, first.stdout + first.stderr
    assert second.returncode == 0, second.stdout + second.stderr
    assert "Library upgrade complete" in first.stdout
    assert "Preparing the library" not in second.stdout
    with sqlite3.connect(database) as connection:
        assert (
            connection.execute("SELECT COUNT(*) FROM local_tracks").fetchone()[0] == 4
        )
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_migration_markers "
                "WHERE marker = 'legacy_catalog_import_complete'"
            ).fetchone()[0]
            == 1
        )
    backups = list((root / "cache" / "upgrade-backups").iterdir())
    assert len(backups) == 1
    state = json.loads(
        (root / "cache" / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    assert state["evidence"]["embedded_art_reads"] == 0


def test_real_upgrade_remaps_absent_legacy_paths_to_configured_root(
    tmp_path: Path,
) -> None:
    root = tmp_path / "app"
    historical_music = Path("/legacy-droppedneedle-test") / tmp_path.name / "Music"
    current_music = tmp_path / "Current" / "Music"
    compilation = current_music / "Compilation"
    compilation.mkdir(parents=True)
    (compilation / "01.flac").write_bytes(b"a" * 100)
    (compilation / "02.flac").write_bytes(b"b" * 200)
    database = root / "cache" / "library.db"
    database.parent.mkdir(parents=True)
    _create_source(database, historical_music)
    config = root / "config" / "config.json"
    config.parent.mkdir(parents=True)
    config.write_text(
        json.dumps({"library_settings": {"library_paths": [str(current_music)]}}),
        encoding="utf-8",
    )

    result = _run_real_upgrade(root)

    assert result.returncode == 0, result.stdout + result.stderr
    with sqlite3.connect(database) as connection:
        target_paths = [
            str(row[0])
            for row in connection.execute(
                "SELECT file_path FROM local_tracks ORDER BY file_path"
            ).fetchall()
        ]
        legacy_paths = [
            str(row[0])
            for row in connection.execute(
                "SELECT file_path FROM library_files ORDER BY file_path"
            ).fetchall()
        ]
    assert all(path.startswith(str(current_music)) for path in target_paths)
    assert all(path.startswith(str(historical_music)) for path in legacy_paths)
    state = json.loads(
        (root / "cache" / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    reconciliation = state["evidence"]["path_reconciliation"]
    assert reconciliation["mode"] == "remapped"
    assert reconciliation["library_file_count"] == 2
    assert reconciliation["review_row_count"] == 4
    assert len(reconciliation["root_ids"]) == 1


def test_real_upgrade_retargets_root_to_present_legacy_location(
    tmp_path: Path,
) -> None:
    root = tmp_path / "app"
    historical_base = Path.home() / f".droppedneedle-upgrade-test-{tmp_path.name}"
    historical_music = historical_base / "Music"
    configured_music = tmp_path / "Configured" / "Music"
    compilation = historical_music / "Compilation"
    compilation.mkdir(parents=True)
    (compilation / "01.flac").write_bytes(b"a" * 100)
    (compilation / "02.flac").write_bytes(b"b" * 200)
    database = root / "cache" / "library.db"
    database.parent.mkdir(parents=True)
    _create_source(database, historical_music)
    config = root / "config" / "config.json"
    config.parent.mkdir(parents=True)
    config.write_text(
        json.dumps({"library_settings": {"library_paths": [str(configured_music)]}}),
        encoding="utf-8",
    )

    try:
        result = _run_real_upgrade(root)
    finally:
        shutil.rmtree(historical_base, ignore_errors=True)

    assert result.returncode == 0, result.stdout + result.stderr
    with sqlite3.connect(database) as connection:
        target_paths = [
            str(row[0])
            for row in connection.execute(
                "SELECT file_path FROM local_tracks ORDER BY file_path"
            ).fetchall()
        ]
    assert all(path.startswith(str(historical_music)) for path in target_paths)
    persisted = json.loads(config.read_text(encoding="utf-8"))
    saved_root_paths = [
        entry["path"] for entry in persisted["library_settings"]["library_roots"]
    ]
    assert saved_root_paths == [str(historical_music)]
    state = json.loads(
        (root / "cache" / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    reconciliation = state["evidence"]["path_reconciliation"]
    assert reconciliation["mode"] == "exact"
    assert reconciliation["library_file_count"] == 2
    assert reconciliation["review_row_count"] == 4
    assert len(reconciliation["root_ids"]) == 1


def test_real_upgrade_records_sanitized_pending_path_reconciliation(
    tmp_path: Path,
) -> None:
    root = tmp_path / "app"
    historical_music = Path("/legacy-droppedneedle-test") / tmp_path.name / "Music"
    configured_music = tmp_path / "Current" / "Music"
    configured_music.mkdir(parents=True)
    database = root / "cache" / "library.db"
    database.parent.mkdir(parents=True)
    _create_source(database, historical_music)
    config = root / "config" / "config.json"
    config.parent.mkdir(parents=True)
    config.write_text(
        json.dumps({"library_settings": {"library_paths": [str(configured_music)]}}),
        encoding="utf-8",
    )

    result = _run_real_upgrade(root)

    assert result.returncode == 0, result.stdout + result.stderr
    state = json.loads(
        (root / "cache" / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    reconciliation = state["evidence"]["path_reconciliation"]
    assert reconciliation == {
        "mode": "blocked",
        "library_file_count": 2,
        "review_row_count": 4,
        "failure_reason": "unverified_path_remap",
    }
    assert state["evidence"]["skipped"]["library_file"] == 2
    assert state["evidence"]["skipped"]["review_row"] == 4
    serialized = json.dumps(reconciliation)
    assert str(historical_music) not in serialized
    assert str(configured_music) not in serialized


def test_real_upgrade_skips_unresolvable_references(
    tmp_path: Path,
) -> None:
    root = tmp_path / "app"
    music = tmp_path / "Music"
    music.mkdir(parents=True)
    database = root / "cache" / "library.db"
    database.parent.mkdir(parents=True)
    _create_source(database, music)
    with sqlite3.connect(database) as connection:
        connection.execute(
            "INSERT INTO user_favorites VALUES (?, ?, ?, ?)",
            ("alice", "album", "private-missing-reference", 5),
        )
    config = root / "config" / "config.json"
    config.parent.mkdir(parents=True)
    config.write_text(
        json.dumps({"library_settings": {"library_paths": [str(music)]}}),
        encoding="utf-8",
    )

    result = _run_real_upgrade(root)

    assert result.returncode == 0, result.stdout + result.stderr
    state = json.loads(
        (root / "cache" / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    assert state["evidence"]["skipped"]["favorite"] == 1
    serialized = json.dumps(state["evidence"])
    assert "alice" not in serialized
    assert "private-missing-reference" not in serialized
    assert str(music) not in serialized
    with sqlite3.connect(database) as connection:
        assert connection.execute(
            "SELECT COUNT(*) FROM user_favorites WHERE item_id = ?",
            ("private-missing-reference",),
        ).fetchone() == (1,)


def test_real_upgrade_retains_unresolved_history_and_completes(
    tmp_path: Path,
) -> None:
    root = tmp_path / "app"
    music = tmp_path / "Music"
    music.mkdir(parents=True)
    database = root / "cache" / "library.db"
    database.parent.mkdir(parents=True)
    _create_source(database, music)
    with sqlite3.connect(database) as connection:
        connection.execute(
            "INSERT INTO play_history VALUES (?,?,?,?,?,?,?,?,?,?)",
            (
                "gone-track-history",
                "alice",
                "Gone Track",
                "Gone Artist",
                "Gone Album",
                None,
                None,
                180000,
                None,
                "2025-01-01T00:00:00Z",
            ),
        )
    config = root / "config" / "config.json"
    config.parent.mkdir(parents=True)
    config.write_text(
        json.dumps({"library_settings": {"library_paths": [str(music)]}}),
        encoding="utf-8",
    )

    first = _run_real_upgrade(root)
    second = _run_real_upgrade(root)

    assert first.returncode == 0, first.stdout + first.stderr
    assert "Library upgrade complete" in first.stdout
    assert second.returncode == 0, second.stdout + second.stderr
    assert "Preparing the library" not in second.stdout
    assert automatic_upgrade._database_has_marker(database)
    with sqlite3.connect(database) as connection:
        retained = connection.execute(
            "SELECT local_track_id, local_album_id, local_artist_id, track_name, "
            "artist_name, album_name, recording_mbid, release_group_mbid, duration_ms, "
            "source, played_at FROM library_play_history WHERE id = ?",
            ("gone-track-history",),
        ).fetchone()
        unresolved = connection.execute(
            "SELECT COUNT(*) FROM library_migration_provenance "
            "WHERE source_kind = 'history' AND source_key = ?",
            ("gone-track-history",),
        ).fetchone()[0]
    assert retained == (
        None,
        None,
        None,
        "Gone Track",
        "Gone Artist",
        "Gone Album",
        None,
        None,
        180000,
        None,
        "2025-01-01T00:00:00Z",
    )
    assert unresolved == 0


def test_real_upgrade_preserves_ten_identityless_legacy_library_files(
    tmp_path: Path,
) -> None:
    root = tmp_path / "app"
    music = tmp_path / "Music"
    music.mkdir(parents=True)
    database = root / "cache" / "library.db"
    database.parent.mkdir(parents=True)
    _create_source(database, music)
    legacy_ids = _insert_identityless_legacy_files(database, music, count=10)
    config = root / "config" / "config.json"
    config.parent.mkdir(parents=True)
    config.write_text(
        json.dumps({"library_settings": {"library_paths": [str(music)]}}),
        encoding="utf-8",
    )

    result = _run_real_upgrade(root)

    assert result.returncode == 0, result.stdout + result.stderr
    assert "Migrating local-only catalog tracks: 10/10 (100%)" in result.stdout
    assert "Working-copy migration checks passed" in result.stdout
    with sqlite3.connect(database) as connection:
        migrated = connection.execute(
            f"SELECT id, local_album_id FROM local_tracks WHERE id IN "
            f"({','.join('?' for _ in legacy_ids)}) ORDER BY id",
            legacy_ids,
        ).fetchall()
        reviews = connection.execute(
            "SELECT COUNT(*) FROM library_identification_reviews "
            "WHERE reason_code = 'legacy_missing_release_group_id'"
        ).fetchone()[0]
        album_identities = connection.execute(
            "SELECT COUNT(*) FROM local_album_external_identities "
            "WHERE local_album_id = ?",
            (migrated[0][1],),
        ).fetchone()[0]
    assert [row[0] for row in migrated] == sorted(legacy_ids)
    assert len({row[1] for row in migrated}) == 1
    assert reviews == 10
    assert album_identities == 0


def test_real_fresh_installation_initializes_without_user_steps(tmp_path: Path) -> None:
    root = tmp_path / "fresh"

    result = _run_real_upgrade(root)

    assert result.returncode == 0, result.stdout + result.stderr
    database = root / "cache" / "library.db"
    with sqlite3.connect(database) as connection:
        assert (
            connection.execute("SELECT COUNT(*) FROM local_tracks").fetchone()[0] == 0
        )
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_migration_markers "
                "WHERE marker = 'legacy_catalog_import_complete'"
            ).fetchone()[0]
            == 1
        )


def test_docker_image_runs_automatic_upgrade_before_target_application() -> None:
    dockerfile = (Path(__file__).parents[3] / "Dockerfile").read_text(encoding="utf-8")

    assert (
        'CMD ["python", "-m", "maintenance.automatic_upgrade", "--start-target"]'
        in dockerfile
    )
    assert "find /app -type f" in dockerfile
    assert "find /app/backend" not in dockerfile
    assert automatic_upgrade._target_command(8688)[-2:] == ["--workers", "1"]


def test_upgrade_health_endpoint_keeps_existing_orchestrators_waiting() -> None:
    port = _free_port()

    with _upgrade_health_server(port, ""):
        with urlopen(f"http://127.0.0.1:{port}/health", timeout=2) as response:
            assert response.status == 200
            assert json.loads(response.read()) == {"status": "upgrading"}
        with pytest.raises(HTTPError) as error:
            urlopen(f"http://127.0.0.1:{port}/api/v1/library", timeout=2)

    assert error.value.code == 503


def test_upgrade_health_endpoint_serves_only_prefixed_health_under_base_path() -> None:
    port = _free_port()

    with _upgrade_health_server(port, "/music"):
        with urlopen(f"http://127.0.0.1:{port}/music/health", timeout=2) as response:
            assert response.status == 200
            assert json.loads(response.read()) == {"status": "upgrading"}
        for path in ("/health", "/music/api/v1/library"):
            with pytest.raises(HTTPError) as error:
                urlopen(f"http://127.0.0.1:{port}{path}", timeout=2)
            assert error.value.code == 503


@contextmanager
def _operational_target(port: int, base_path: str):
    """Serve an ok health payload exactly where the operational app would."""
    health_path = f"{base_path}/health"

    class OperationalHandler(BaseHTTPRequestHandler):
        def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
            ready = self.path == health_path
            body = b'{"status":"ok"}' if ready else b'{"error":"not found"}'
            self.send_response(200 if ready else 404)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, _format: str, *args: object) -> None:
            return

    server = ThreadingHTTPServer(("127.0.0.1", port), OperationalHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


@pytest.mark.parametrize("base_path,other", [("", "/music"), ("/music", "")])
def test_target_ready_requires_normalized_base_health(
    base_path: str, other: str
) -> None:
    port = _free_port()

    with _operational_target(port, base_path):
        assert _target_ready(port, base_path)
        assert not _target_ready(port, other)


def test_copy_upgrade_promotes_only_after_the_working_database_passes(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)
    settings.config_file_path.parent.mkdir(parents=True)
    settings.config_file_path.write_text('{"name":"before"}', encoding="utf-8")
    monkeypatch.setenv("COMMIT_TAG", "copy-success")

    def migrate(working: Path) -> dict[str, object]:
        database = working / "cache" / "library.db"
        with sqlite3.connect(database) as connection:
            connection.execute("UPDATE source_value SET value = 'migrated'")
        (working / "config" / "config.json").write_text(
            '{"name":"after"}', encoding="utf-8"
        )
        _mark_migrated(database)
        assert _source_value(settings.library_db_path) == "original"
        assert (
            settings.config_file_path.read_text(encoding="utf-8") == '{"name":"before"}'
        )
        return {"passed": True}

    assert run_automatic_copy_upgrade(settings, runner=migrate) == "upgraded"
    assert _source_value(settings.library_db_path) == "migrated"
    assert settings.config_file_path.read_text(encoding="utf-8") == '{"name":"after"}'


def test_process_kill_during_copy_migration_leaves_source_untouched(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)
    settings.config_file_path.parent.mkdir(parents=True)
    settings.config_file_path.write_text('{"name":"before"}', encoding="utf-8")
    monkeypatch.setenv("COMMIT_TAG", "copy-killed")

    class SimulatedProcessKill(BaseException):
        pass

    def killed(working: Path) -> dict[str, object]:
        with sqlite3.connect(working / "cache" / "library.db") as connection:
            connection.execute("UPDATE source_value SET value = 'partial'")
        (working / "config" / "config.json").write_text(
            '{"name":"partial"}', encoding="utf-8"
        )
        raise SimulatedProcessKill

    with pytest.raises(SimulatedProcessKill):
        run_automatic_copy_upgrade(settings, runner=killed)

    assert _source_value(settings.library_db_path) == "original"
    assert settings.config_file_path.read_text(encoding="utf-8") == '{"name":"before"}'

    def unexpected_restore(_settings: Settings, _backup: object) -> None:
        raise AssertionError("an interrupted working-copy migration changed no source")

    monkeypatch.setattr(automatic_upgrade, "restore_upgrade_backup", unexpected_restore)

    def retry(working: Path) -> dict[str, object]:
        assert _source_value(settings.library_db_path) == "original"
        assert (
            settings.config_file_path.read_text(encoding="utf-8") == '{"name":"before"}'
        )
        database = working / "cache" / "library.db"
        with sqlite3.connect(database) as connection:
            connection.execute("UPDATE source_value SET value = 'migrated'")
        _mark_migrated(database)
        return {"passed": True}

    assert run_automatic_copy_upgrade(settings, runner=retry) == "upgraded"
    assert _source_value(settings.library_db_path) == "migrated"


def test_process_kill_between_config_and_database_promotion_recovers_first(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)
    settings.config_file_path.parent.mkdir(parents=True)
    settings.config_file_path.write_text('{"name":"before"}', encoding="utf-8")
    monkeypatch.setenv("COMMIT_TAG", "promotion-killed")

    class SimulatedProcessKill(BaseException):
        pass

    original_replace_database = automatic_upgrade._replace_database

    def killed_replace_database(_source: Path, _destination: Path) -> None:
        raise SimulatedProcessKill

    def migrate(working: Path) -> dict[str, object]:
        database = working / "cache" / "library.db"
        _mark_migrated(database)
        (working / "config" / "config.json").write_text(
            '{"name":"after"}', encoding="utf-8"
        )
        return {"passed": True}

    monkeypatch.setattr(automatic_upgrade, "_replace_database", killed_replace_database)
    with pytest.raises(SimulatedProcessKill):
        run_automatic_copy_upgrade(settings, runner=migrate)
    assert _source_value(settings.library_db_path) == "original"
    assert settings.config_file_path.read_text(encoding="utf-8") == '{"name":"after"}'

    monkeypatch.setattr(
        automatic_upgrade, "_replace_database", original_replace_database
    )

    def retry(working: Path) -> dict[str, object]:
        assert _source_value(settings.library_db_path) == "original"
        assert (
            settings.config_file_path.read_text(encoding="utf-8") == '{"name":"before"}'
        )
        database = working / "cache" / "library.db"
        _mark_migrated(database)
        return {"passed": True}

    assert run_automatic_copy_upgrade(settings, runner=retry) == "upgraded"


def test_process_kill_before_pending_startup_journal_restores_before_retry(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)
    settings.config_file_path.parent.mkdir(parents=True)
    settings.config_file_path.write_text('{"name":"before"}', encoding="utf-8")

    class SimulatedProcessKill(BaseException):
        pass

    original_write_state = automatic_upgrade._write_state

    def killed_write_state(path: Path, payload: dict[str, object]) -> None:
        if payload.get("stage") == "promoted_pending_startup":
            raise SimulatedProcessKill
        original_write_state(path, payload)

    def migrate(working: Path) -> dict[str, object]:
        database = working / "cache" / "library.db"
        with sqlite3.connect(database) as connection:
            connection.execute("UPDATE source_value SET value = 'migrated'")
        (working / "config" / "config.json").write_text(
            '{"name":"after"}', encoding="utf-8"
        )
        _mark_migrated(database)
        return {"passed": True}

    monkeypatch.setattr(automatic_upgrade, "_write_state", killed_write_state)
    with pytest.raises(SimulatedProcessKill):
        run_automatic_copy_upgrade(
            settings, runner=migrate, require_target_admission=True
        )
    assert _source_value(settings.library_db_path) == "migrated"

    monkeypatch.setattr(automatic_upgrade, "_write_state", original_write_state)

    def retry(working: Path) -> dict[str, object]:
        assert _source_value(settings.library_db_path) == "original"
        assert (
            settings.config_file_path.read_text(encoding="utf-8") == '{"name":"before"}'
        )
        database = working / "cache" / "library.db"
        _mark_migrated(database)
        return {"passed": True}

    assert run_automatic_copy_upgrade(settings, runner=retry) == "upgraded"


def test_target_exit_before_validation_restores_promoted_inputs(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)
    settings.config_file_path.parent.mkdir(parents=True)
    settings.config_file_path.write_text('{"name":"before"}', encoding="utf-8")
    monkeypatch.setenv("COMMIT_TAG", "target-start-failure")

    def migrate(working: Path) -> dict[str, object]:
        database = working / "cache" / "library.db"
        with sqlite3.connect(database) as connection:
            connection.execute("UPDATE source_value SET value = 'migrated'")
        (working / "config" / "config.json").write_text(
            '{"name":"after"}', encoding="utf-8"
        )
        _mark_migrated(database)
        return {"passed": True}

    run_automatic_copy_upgrade(settings, runner=migrate, require_target_admission=True)

    result = run_target_supervisor(
        settings,
        command=[sys.executable, "-c", "raise SystemExit(7)"],
        admission_timeout_seconds=1,
    )

    assert result != 0
    assert _source_value(settings.library_db_path) == "original"
    assert settings.config_file_path.read_text(encoding="utf-8") == '{"name":"before"}'
    assert not automatic_upgrade._database_has_marker(settings.library_db_path)
    state = json.loads(
        (settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    assert state["stage"] == "failed"


def test_target_clean_exit_before_readiness_is_still_a_failure(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    settings = _settings(tmp_path)
    monkeypatch.setattr(
        automatic_upgrade, "_target_ready", lambda _port, _base_path: False
    )

    result = run_target_supervisor(
        settings,
        command=[sys.executable, "-c", "raise SystemExit(0)"],
        admission_timeout_seconds=1,
    )

    assert result == 1
    assert "exited before it was ready" in capsys.readouterr().out


def test_target_validation_commits_before_releasing_operational_startup(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)

    def migrate(working: Path) -> dict[str, object]:
        _mark_migrated(working / "cache" / "library.db")
        return {"passed": True}

    run_automatic_copy_upgrade(settings, runner=migrate, require_target_admission=True)

    class FakeProcess:
        returncode = 0

        def poll(self) -> None:
            return None

        def wait(self, timeout: float | None = None) -> int:
            return 0

        def send_signal(self, _signum: int) -> None:
            return

        def terminate(self) -> None:
            return

        def kill(self) -> None:
            return

    def start(_command: list[str], *, env: dict[str, str]) -> FakeProcess:
        token = env[automatic_upgrade._ADMISSION_TOKEN_ENV]
        validated, _admitted = automatic_upgrade._admission_paths(settings, token)
        automatic_upgrade._write_state(validated, {"token": token})
        return FakeProcess()

    monkeypatch.setattr(automatic_upgrade.subprocess, "Popen", start)
    monkeypatch.setattr(
        automatic_upgrade, "_target_ready", lambda _port, _base_path: True
    )

    assert run_target_supervisor(settings, command=["target"]) == 0
    state = json.loads(
        (settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    assert state["stage"] == "completed"
    assert _source_value(settings.library_db_path) == "original"


@pytest.mark.asyncio
async def test_target_lifespan_waits_for_durable_parent_admission(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    token = "a" * 32
    monkeypatch.setenv(automatic_upgrade._ADMISSION_TOKEN_ENV, token)
    validated, admitted = automatic_upgrade._admission_paths(settings, token)

    task = asyncio.create_task(
        automatic_upgrade.await_target_startup_admission(settings)
    )
    for _ in range(20):
        if validated.is_file():
            break
        await asyncio.sleep(0.01)

    assert validated.is_file()
    assert not task.done()

    automatic_upgrade._write_state(admitted, {"token": token})
    await asyncio.wait_for(task, timeout=1)
    assert not validated.exists()
    assert not admitted.exists()


@pytest.mark.asyncio
async def test_target_startup_progress_heartbeat_advances_while_stage_is_active(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    token = "b" * 32
    monkeypatch.setenv(automatic_upgrade._ADMISSION_TOKEN_ENV, token)
    monkeypatch.setattr(
        automatic_upgrade, "_ADMISSION_HEARTBEAT_INTERVAL_SECONDS", 0.01
    )
    progress_path = automatic_upgrade._admission_progress_path(settings, token)

    async with automatic_upgrade.target_startup_progress(
        settings, "catalog_validation"
    ):
        await asyncio.sleep(0.035)
        progress = automatic_upgrade._target_progress(progress_path, token)

    assert progress is not None
    assert progress["stage"] == "catalog_validation"
    assert progress["sequence"] >= 3
    assert progress["elapsed_seconds"] > 0

    async with automatic_upgrade.target_startup_progress(settings, "admission"):
        next_progress = automatic_upgrade._target_progress(progress_path, token)
    assert next_progress is not None
    assert next_progress["stage"] == "admission"
    assert next_progress["sequence"] == 1


def test_target_startup_heartbeat_extends_idle_deadline_until_validation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)

    def migrate(working: Path) -> dict[str, object]:
        _mark_migrated(working / "cache" / "library.db")
        return {"passed": True}

    run_automatic_copy_upgrade(settings, runner=migrate, require_target_admission=True)

    class FakeProcess:
        returncode: int | None = None

        def __init__(self) -> None:
            self.thread: threading.Thread | None = None

        def poll(self) -> int | None:
            return self.returncode

        def wait(self, timeout: float | None = None) -> int:
            assert self.thread is not None
            self.thread.join(timeout)
            return self.returncode or 0

        def send_signal(self, _signum: int) -> None:
            self.returncode = 1

        def terminate(self) -> None:
            self.returncode = 1

        def kill(self) -> None:
            self.returncode = 1

    process = FakeProcess()

    def start(_command: list[str], *, env: dict[str, str]) -> FakeProcess:
        token = env[automatic_upgrade._ADMISSION_TOKEN_ENV]
        validated, admitted = automatic_upgrade._admission_paths(settings, token)
        progress = automatic_upgrade._admission_progress_path(settings, token)

        def child() -> None:
            for sequence in range(1, 7):
                automatic_upgrade._write_state(
                    progress,
                    {
                        "token": token,
                        "stage": "catalog_validation",
                        "sequence": sequence,
                        "elapsed_seconds": sequence * 0.02,
                    },
                )
                time.sleep(0.02)
            automatic_upgrade._write_state(validated, {"token": token})
            deadline = time.monotonic() + 1
            while time.monotonic() < deadline and not admitted.exists():
                time.sleep(0.005)
            process.returncode = 0

        process.thread = threading.Thread(target=child, daemon=True)
        process.thread.start()
        return process

    monkeypatch.setattr(automatic_upgrade.subprocess, "Popen", start)
    monkeypatch.setattr(
        automatic_upgrade, "_target_ready", lambda _port, _base_path: True
    )

    assert (
        run_target_supervisor(
            settings,
            command=["target"],
            admission_timeout_seconds=0.04,
        )
        == 0
    )
    state = json.loads(
        (settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    assert state["stage"] == "completed"


def test_target_startup_without_heartbeat_times_out_with_sanitized_evidence(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)

    def migrate(working: Path) -> dict[str, object]:
        _mark_migrated(working / "cache" / "library.db")
        return {"passed": True}

    run_automatic_copy_upgrade(settings, runner=migrate, require_target_admission=True)

    class StalledProcess:
        returncode: int | None = None

        def poll(self) -> int | None:
            return self.returncode

        def wait(self, timeout: float | None = None) -> int:
            return self.returncode or 0

        def send_signal(self, _signum: int) -> None:
            self.returncode = 1

        def terminate(self) -> None:
            self.returncode = 1

        def kill(self) -> None:
            self.returncode = 1

    monkeypatch.setattr(
        automatic_upgrade.subprocess,
        "Popen",
        lambda *_args, **_kwargs: StalledProcess(),
    )

    assert (
        run_target_supervisor(
            settings,
            command=["target"],
            admission_timeout_seconds=0.02,
        )
        == 1
    )
    state = json.loads(
        (settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    assert state["error_type"] == "TargetStartupTimeout"
    assert state["failure_evidence"]["last_stage"] == "process_start"
    assert state["failure_evidence"]["elapsed_seconds"] >= 0.02
    assert state["failure_evidence"]["returncode"] == 1
    assert _source_value(settings.library_db_path) == "original"


def test_target_startup_hard_timeout_stops_advancing_heartbeat(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)

    def migrate(working: Path) -> dict[str, object]:
        _mark_migrated(working / "cache" / "library.db")
        return {"passed": True}

    run_automatic_copy_upgrade(settings, runner=migrate, require_target_admission=True)

    class StalledProcess:
        returncode: int | None = None

        def poll(self) -> int | None:
            return self.returncode

        def wait(self, timeout: float | None = None) -> int:
            return self.returncode or 0

        def send_signal(self, _signum: int) -> None:
            self.returncode = 1

        def terminate(self) -> None:
            self.returncode = 1

        def kill(self) -> None:
            self.returncode = 1

    sequence = 0

    def progress(_path: Path, _token: str) -> dict[str, object]:
        nonlocal sequence
        sequence += 1
        return {
            "stage": "catalog_validation",
            "sequence": sequence,
            "elapsed_seconds": sequence * 0.01,
        }

    monkeypatch.setattr(
        automatic_upgrade.subprocess,
        "Popen",
        lambda *_args, **_kwargs: StalledProcess(),
    )
    monkeypatch.setattr(automatic_upgrade, "_target_progress", progress)
    monkeypatch.setattr(automatic_upgrade, "_TARGET_STARTUP_HARD_TIMEOUT_SECONDS", 0.03)

    assert (
        run_target_supervisor(
            settings,
            command=["target"],
            admission_timeout_seconds=1,
        )
        == 1
    )
    state = json.loads(
        (settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    assert state["error_type"] == "TargetStartupHardTimeout"
    assert state["failure_evidence"]["last_stage"] == "catalog_validation"
    assert _source_value(settings.library_db_path) == "original"


def test_post_admission_readiness_timeout_records_failure_without_rollback(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)

    def migrate(working: Path) -> dict[str, object]:
        database = working / "cache" / "library.db"
        with sqlite3.connect(database) as connection:
            connection.execute("UPDATE source_value SET value = 'migrated'")
        _mark_migrated(database)
        return {"passed": True}

    run_automatic_copy_upgrade(settings, runner=migrate, require_target_admission=True)

    class StalledProcess:
        returncode: int | None = None

        def poll(self) -> int | None:
            return self.returncode

        def wait(self, timeout: float | None = None) -> int:
            return self.returncode or 0

        def send_signal(self, _signum: int) -> None:
            self.returncode = 1

        def terminate(self) -> None:
            self.returncode = 1

        def kill(self) -> None:
            self.returncode = 1

    def start(_command: list[str], *, env: dict[str, str]) -> StalledProcess:
        token = env[automatic_upgrade._ADMISSION_TOKEN_ENV]
        validated, _admitted = automatic_upgrade._admission_paths(settings, token)
        automatic_upgrade._write_state(validated, {"token": token})
        return StalledProcess()

    monkeypatch.setattr(automatic_upgrade.subprocess, "Popen", start)
    monkeypatch.setattr(
        automatic_upgrade, "_target_ready", lambda _port, _base_path: False
    )

    assert (
        run_target_supervisor(
            settings,
            command=["target"],
            admission_timeout_seconds=0.02,
        )
        == 1
    )
    state = json.loads(
        (settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    assert state["stage"] == "completed"
    failure = state["target_startup_failure"]
    assert failure["error_type"] == "TargetReadinessTimeout"
    assert failure["last_stage"] == "process_start"
    assert failure["elapsed_seconds"] >= 0.02
    assert failure["returncode"] == 1
    assert _source_value(settings.library_db_path) == "migrated"

    monkeypatch.setattr(
        automatic_upgrade.subprocess,
        "Popen",
        lambda *_args, **_kwargs: StalledProcess(),
    )
    monkeypatch.setattr(
        automatic_upgrade, "_target_ready", lambda _port, _base_path: True
    )
    assert run_target_supervisor(settings, command=["target"]) == 0
    recovered_state = json.loads(
        (settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    assert "target_startup_failure" not in recovered_state


def test_post_admission_clean_exit_records_failure_without_rollback(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)

    def migrate(working: Path) -> dict[str, object]:
        database = working / "cache" / "library.db"
        with sqlite3.connect(database) as connection:
            connection.execute("UPDATE source_value SET value = 'migrated'")
        _mark_migrated(database)
        return {"passed": True}

    run_automatic_copy_upgrade(settings, runner=migrate, require_target_admission=True)

    class ExitingProcess:
        returncode = 0

        def __init__(self, admitted: Path) -> None:
            self.admitted = admitted

        def poll(self) -> int | None:
            return 0 if self.admitted.exists() else None

        def wait(self, timeout: float | None = None) -> int:
            return 0

        def send_signal(self, _signum: int) -> None:
            return

        def terminate(self) -> None:
            return

        def kill(self) -> None:
            return

    def start(_command: list[str], *, env: dict[str, str]) -> ExitingProcess:
        token = env[automatic_upgrade._ADMISSION_TOKEN_ENV]
        validated, admitted = automatic_upgrade._admission_paths(settings, token)
        automatic_upgrade._write_state(validated, {"token": token})
        return ExitingProcess(admitted)

    monkeypatch.setattr(automatic_upgrade.subprocess, "Popen", start)
    monkeypatch.setattr(
        automatic_upgrade, "_target_ready", lambda _port, _base_path: False
    )

    assert run_target_supervisor(settings, command=["target"]) == 1
    state = json.loads(
        (settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    assert state["stage"] == "completed"
    assert state["target_startup_failure"]["error_type"] == (
        "TargetProcessExitedBeforeReadiness"
    )
    assert state["target_startup_failure"]["returncode"] == 0
    assert _source_value(settings.library_db_path) == "migrated"


def test_admission_write_failure_kills_unresponsive_target_and_records_failure(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)

    def migrate(working: Path) -> dict[str, object]:
        _mark_migrated(working / "cache" / "library.db")
        return {"passed": True}

    run_automatic_copy_upgrade(settings, runner=migrate, require_target_admission=True)

    class UnresponsiveProcess:
        returncode: int | None = None
        killed = False

        def poll(self) -> int | None:
            return self.returncode

        def wait(self, timeout: float | None = None) -> int:
            if not self.killed:
                raise subprocess.TimeoutExpired("target", timeout)
            return self.returncode or 0

        def send_signal(self, _signum: int) -> None:
            return

        def terminate(self) -> None:
            return

        def kill(self) -> None:
            self.killed = True
            self.returncode = 9

    process = UnresponsiveProcess()
    original_write_state = automatic_upgrade._write_state

    def start(_command: list[str], *, env: dict[str, str]) -> UnresponsiveProcess:
        token = env[automatic_upgrade._ADMISSION_TOKEN_ENV]
        validated, _admitted = automatic_upgrade._admission_paths(settings, token)
        original_write_state(validated, {"token": token})
        return process

    def fail_admitted_write(path: Path, payload: dict[str, object]) -> None:
        if path.name.endswith(".admitted.json"):
            raise OSError("simulated admission write failure")
        original_write_state(path, payload)

    monkeypatch.setattr(automatic_upgrade.subprocess, "Popen", start)
    monkeypatch.setattr(automatic_upgrade, "_write_state", fail_admitted_write)

    assert run_target_supervisor(settings, command=["target"]) == 1
    assert process.killed is True
    state = json.loads(
        (settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    assert state["stage"] == "completed"
    assert state["target_startup_failure"]["error_type"] == (
        "TargetAdmissionWriteError"
    )
    assert state["target_startup_failure"]["returncode"] == 9


def test_admission_commit_error_rechecks_durable_state_before_rollback(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)

    def migrate(working: Path) -> dict[str, object]:
        database = working / "cache" / "library.db"
        with sqlite3.connect(database) as connection:
            connection.execute("UPDATE source_value SET value = 'migrated'")
        _mark_migrated(database)
        return {"passed": True}

    run_automatic_copy_upgrade(settings, runner=migrate, require_target_admission=True)

    class StalledProcess:
        returncode: int | None = None

        def poll(self) -> int | None:
            return self.returncode

        def wait(self, timeout: float | None = None) -> int:
            return self.returncode or 0

        def send_signal(self, _signum: int) -> None:
            self.returncode = 1

        def terminate(self) -> None:
            self.returncode = 1

        def kill(self) -> None:
            self.returncode = 1

    def start(_command: list[str], *, env: dict[str, str]) -> StalledProcess:
        token = env[automatic_upgrade._ADMISSION_TOKEN_ENV]
        validated, _admitted = automatic_upgrade._admission_paths(settings, token)
        automatic_upgrade._write_state(validated, {"token": token})
        return StalledProcess()

    original_complete = automatic_upgrade._complete_target_admission

    def complete_then_fail(current_settings: Settings) -> None:
        original_complete(current_settings)
        raise OSError("simulated post-rename fsync failure")

    monkeypatch.setattr(automatic_upgrade.subprocess, "Popen", start)
    monkeypatch.setattr(
        automatic_upgrade, "_complete_target_admission", complete_then_fail
    )

    assert run_target_supervisor(settings, command=["target"]) == 1
    state = json.loads(
        (settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    assert state["stage"] == "completed"
    assert state["target_startup_failure"]["error_type"] == (
        "TargetAdmissionWriteError"
    )
    assert _source_value(settings.library_db_path) == "migrated"


def test_baked_source_revision_overrides_static_compose_tag(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    revision = tmp_path / "revision"
    revision.write_text("backend-build-one\n", encoding="utf-8")
    monkeypatch.setattr(automatic_upgrade, "_SOURCE_REVISION_PATH", revision)
    monkeypatch.setenv("DROPPEDNEEDLE_SOURCE_REVISION", "unknown")
    monkeypatch.setenv("COMMIT_TAG", "hosting-local")

    assert automatic_upgrade._image_version() == "backend-build-one"

    revision.write_text("backend-build-two\n", encoding="utf-8")
    assert automatic_upgrade._image_version() == "backend-build-two"


def test_main_uses_the_container_port_for_upgrade_health(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    observed: list[int] = []

    @contextmanager
    def fake_health(port: int, base_path: str):
        observed.append(port)
        yield

    monkeypatch.setenv("PORT", "9876")
    monkeypatch.setattr(sys, "argv", ["automatic_upgrade"])
    monkeypatch.setattr(automatic_upgrade, "get_settings", lambda: settings)
    monkeypatch.setattr(automatic_upgrade, "_upgrade_health_server", fake_health)
    monkeypatch.setattr(
        automatic_upgrade,
        "run_automatic_copy_upgrade",
        lambda _settings, **_kwargs: "upgraded",
    )

    assert automatic_upgrade.main() == 0
    assert observed == [9876]


def test_main_removes_default_config_when_fresh_upgrade_fails(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)

    def create_default_settings() -> Settings:
        settings.config_file_path.parent.mkdir(parents=True, exist_ok=True)
        settings.config_file_path.write_text('{"generated":true}', encoding="utf-8")
        return settings

    @contextmanager
    def fake_health(_port: int, _base_path: str):
        yield

    def fail(_settings: Settings, **_kwargs: object) -> str:
        raise AutomaticUpgradeError("simulated failure")

    monkeypatch.setenv("ROOT_APP_DIR", str(tmp_path))
    monkeypatch.setattr(sys, "argv", ["automatic_upgrade"])
    monkeypatch.setattr(automatic_upgrade, "get_settings", create_default_settings)
    monkeypatch.setattr(automatic_upgrade, "_upgrade_health_server", fake_health)
    monkeypatch.setattr(automatic_upgrade, "run_automatic_copy_upgrade", fail)

    assert automatic_upgrade.main() == 1
    assert not settings.config_file_path.exists()


def _failed_replace(_source: object, _destination: object) -> None:
    raise OSError("rename unsupported")


def test_replace_file_falls_back_to_copy_when_rename_unavailable(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source = tmp_path / "source.txt"
    destination = tmp_path / "nested" / "destination.txt"
    source.write_bytes(b"published-content")
    monkeypatch.setattr(automatic_upgrade.os, "replace", _failed_replace)

    automatic_upgrade._replace_file(source, destination)

    assert destination.read_bytes() == b"published-content"


def test_replace_database_falls_back_to_sqlite_copy_when_rename_unavailable(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source = tmp_path / "source.db"
    destination = tmp_path / "destination.db"
    _write_unmigrated_database(source)
    _mark_migrated(source)
    _write_unmigrated_database(destination, value="outdated")
    monkeypatch.setattr(automatic_upgrade.os, "replace", _failed_replace)

    automatic_upgrade._replace_database(source, destination)

    assert automatic_upgrade._database_has_marker(destination)
    assert _source_value(destination) == "original"
    assert not Path(f"{destination}-wal").exists()
    assert not Path(f"{destination}-shm").exists()


def test_replace_file_retries_transient_stale_read(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source = tmp_path / "source.txt"
    destination = tmp_path / "destination.txt"
    source.write_bytes(b"published-content")
    real_sha256 = automatic_upgrade._sha256
    destination_reads = 0

    def flaky_sha256(path: Path) -> str | None:
        nonlocal destination_reads
        if Path(path) == destination:
            destination_reads += 1
            if destination_reads == 1:
                return "stale"
        return real_sha256(path)

    monkeypatch.setattr(automatic_upgrade, "_sha256", flaky_sha256)
    monkeypatch.setattr(automatic_upgrade, "_PUBLISH_VERIFY_INTERVAL_SECONDS", 0)

    automatic_upgrade._replace_file(source, destination)

    assert destination.read_bytes() == b"published-content"
    assert destination_reads > 1


def test_replace_database_raises_when_content_never_verifies(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source = tmp_path / "source.db"
    destination = tmp_path / "destination.db"
    _write_unmigrated_database(source)
    _mark_migrated(source)
    _write_unmigrated_database(destination, value="outdated")
    real_sha256 = automatic_upgrade._sha256

    def wrong_destination_hash(path: Path) -> str | None:
        if Path(path) == destination:
            return "wrong"
        return real_sha256(path)

    monkeypatch.setattr(automatic_upgrade, "_sha256", wrong_destination_hash)
    monkeypatch.setattr(automatic_upgrade, "_PUBLISH_VERIFY_INTERVAL_SECONDS", 0)

    with pytest.raises(OSError, match="could not be verified"):
        automatic_upgrade._replace_database(source, destination)

    assert automatic_upgrade._database_has_marker(destination)
    assert _source_value(destination) == "original"


def test_replace_file_retries_transient_missing_destination_read(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source = tmp_path / "source.txt"
    destination = tmp_path / "destination.txt"
    source.write_bytes(b"published-content")
    real_sha256 = automatic_upgrade._sha256
    destination_reads = 0

    def flaky_sha256(path: Path) -> str | None:
        nonlocal destination_reads
        if Path(path) == destination:
            destination_reads += 1
            if destination_reads <= 2:
                raise FileNotFoundError(
                    errno.ENOENT, "bind-mount flap", str(path)
                )
        return real_sha256(path)

    monkeypatch.setattr(automatic_upgrade, "_sha256", flaky_sha256)
    monkeypatch.setattr(automatic_upgrade, "_PUBLISH_VERIFY_INTERVAL_SECONDS", 0)

    automatic_upgrade._replace_file(source, destination)

    assert destination.read_bytes() == b"published-content"
    assert destination_reads > 2


def test_replace_database_retries_transient_missing_destination_read(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source = tmp_path / "source.db"
    destination = tmp_path / "destination.db"
    _write_unmigrated_database(source)
    _mark_migrated(source)
    _write_unmigrated_database(destination, value="outdated")
    real_sha256 = automatic_upgrade._sha256
    destination_reads = 0

    def flaky_sha256(path: Path) -> str | None:
        nonlocal destination_reads
        if Path(path) == destination:
            destination_reads += 1
            if destination_reads <= 2:
                raise FileNotFoundError(
                    errno.ENOENT, "bind-mount flap", str(path)
                )
        return real_sha256(path)

    monkeypatch.setattr(automatic_upgrade, "_sha256", flaky_sha256)
    monkeypatch.setattr(automatic_upgrade, "_PUBLISH_VERIFY_INTERVAL_SECONDS", 0)

    automatic_upgrade._replace_database(source, destination)

    assert automatic_upgrade._database_has_marker(destination)
    assert _source_value(destination) == "original"
    assert destination_reads > 2


def test_replace_file_recovers_when_destination_open_flaps(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source = tmp_path / "source.txt"
    destination = tmp_path / "destination.txt"
    source.write_bytes(b"published-content")
    real_open = Path.open
    flap_count = 0

    def flappy_open(self: Path, mode: str = "r", *args: object, **kwargs: object):
        nonlocal flap_count
        if (
            Path(self) == destination
            and isinstance(mode, str)
            and mode.startswith("r")
            and flap_count < 3
        ):
            flap_count += 1
            raise FileNotFoundError(errno.ENOENT, "bind-mount flap", str(self))
        return real_open(self, mode, *args, **kwargs)

    monkeypatch.setattr(Path, "open", flappy_open)
    monkeypatch.setattr(automatic_upgrade, "_PUBLISH_VERIFY_INTERVAL_SECONDS", 0)

    automatic_upgrade._replace_file(source, destination)

    assert destination.read_bytes() == b"published-content"
    assert flap_count == 3


def test_replace_file_raises_verifiable_error_when_destination_never_readable(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source = tmp_path / "source.txt"
    destination = tmp_path / "destination.txt"
    source.write_bytes(b"published-content")
    real_sha256 = automatic_upgrade._sha256

    def always_missing(path: Path) -> str | None:
        if Path(path) == destination:
            raise FileNotFoundError(errno.ENOENT, "bind-mount flap", str(path))
        return real_sha256(path)

    monkeypatch.setattr(automatic_upgrade, "_sha256", always_missing)
    monkeypatch.setattr(automatic_upgrade, "_PUBLISH_VERIFY_INTERVAL_SECONDS", 0)

    with pytest.raises(OSError, match="could not be verified") as exc_info:
        automatic_upgrade._replace_file(source, destination)

    assert not isinstance(exc_info.value, FileNotFoundError)


def test_replace_database_raises_verifiable_error_when_destination_unavailable(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source = tmp_path / "source.db"
    destination = tmp_path / "destination.db"
    _write_unmigrated_database(source)
    _mark_migrated(source)
    _write_unmigrated_database(destination, value="outdated")
    real_open = Path.open

    def missing_destination_open(
        self: Path, mode: str = "r", *args: object, **kwargs: object
    ):
        if Path(self) == destination:
            raise FileNotFoundError(errno.ENOENT, "bind-mount flap", str(self))
        return real_open(self, mode, *args, **kwargs)

    monkeypatch.setattr(Path, "open", missing_destination_open)
    monkeypatch.setattr(automatic_upgrade, "_PUBLISH_VERIFY_INTERVAL_SECONDS", 0)

    with pytest.raises(OSError, match="could not be verified") as exc_info:
        automatic_upgrade._replace_database(source, destination)

    assert not isinstance(exc_info.value, FileNotFoundError)


def test_promote_succeeds_when_live_destination_reads_flap(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)
    settings.config_file_path.parent.mkdir(parents=True, exist_ok=True)
    settings.config_file_path.write_text('{"name":"before"}', encoding="utf-8")
    backup = automatic_upgrade.capture_upgrade_backup(settings)
    working = automatic_upgrade.prepare_working_copy(settings, backup)
    working_database = working / "cache" / "library.db"
    with sqlite3.connect(working_database) as connection:
        connection.execute("UPDATE source_value SET value = 'migrated'")
    _mark_migrated(working_database)
    (working / "config" / "config.json").write_text(
        '{"name":"migrated"}', encoding="utf-8"
    )
    live = {settings.library_db_path, settings.config_file_path}
    real_open = Path.open
    flap_count = 0

    def flappy_open(self: Path, mode: str = "r", *args: object, **kwargs: object):
        nonlocal flap_count
        if (
            Path(self) in live
            and isinstance(mode, str)
            and mode.startswith("r")
            and flap_count < 4
        ):
            flap_count += 1
            raise FileNotFoundError(errno.ENOENT, "bind-mount flap", str(self))
        return real_open(self, mode, *args, **kwargs)

    monkeypatch.setattr(Path, "open", flappy_open)
    monkeypatch.setattr(automatic_upgrade, "_PUBLISH_VERIFY_INTERVAL_SECONDS", 0)

    automatic_upgrade.promote_working_copy(settings, working)

    assert automatic_upgrade._database_has_marker(settings.library_db_path)
    assert _source_value(settings.library_db_path) == "migrated"
    assert settings.config_file_path.read_text(encoding="utf-8") == '{"name":"migrated"}'
    assert flap_count == 4


def test_failure_state_written_when_live_signatures_unavailable(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)
    settings.config_file_path.parent.mkdir(parents=True, exist_ok=True)
    settings.config_file_path.write_text('{"name":"before"}', encoding="utf-8")
    monkeypatch.setenv("COMMIT_TAG", "signature-flap-version")
    failure_phase = {"armed": False}
    real_current_signature = automatic_upgrade._current_signature

    def vanishing_signature(
        database: Path, config: Path
    ) -> dict[str, str | None]:
        if failure_phase["armed"]:
            raise FileNotFoundError(errno.ENOENT, "live file vanished mid-failure")
        return real_current_signature(database, config)

    def fail(_working: Path) -> dict[str, object]:
        failure_phase["armed"] = True
        raise RuntimeError("simulated failure")

    monkeypatch.setattr(automatic_upgrade, "_current_signature", vanishing_signature)

    with pytest.raises(AutomaticUpgradeError, match="previous database"):
        run_automatic_copy_upgrade(settings, runner=fail)
    state = json.loads(
        (settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    assert state["stage"] == "failed"
    assert state["error_type"] == "RuntimeError"
    assert state["error_message"] == "simulated failure"
    assert state["restored_signature"] == {
        "database_sha256": None,
        "config_sha256": None,
    }


def test_replace_file_recovers_when_temp_hash_flaps_once(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source = tmp_path / "source.txt"
    destination = tmp_path / "destination.txt"
    source.write_bytes(b"published-content")
    real_sha256 = automatic_upgrade._sha256
    flapped = False

    def flaky_sha256(path: Path) -> str | None:
        nonlocal flapped
        if ".upgrade-" in Path(path).name and not flapped:
            flapped = True
            return None
        return real_sha256(path)

    monkeypatch.setattr(automatic_upgrade, "_sha256", flaky_sha256)
    monkeypatch.setattr(automatic_upgrade, "_PUBLISH_VERIFY_INTERVAL_SECONDS", 0)

    automatic_upgrade._replace_file(source, destination)

    assert flapped
    assert destination.read_bytes() == b"published-content"


def test_replace_database_raises_when_hashes_never_recover(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source = tmp_path / "source.db"
    destination = tmp_path / "destination.db"
    _write_unmigrated_database(source)
    _mark_migrated(source)
    _write_unmigrated_database(destination, value="outdated")

    def always_unreadable(path: Path) -> str | None:
        return None

    monkeypatch.setattr(automatic_upgrade, "_sha256", always_unreadable)
    monkeypatch.setattr(automatic_upgrade, "_PUBLISH_VERIFY_INTERVAL_SECONDS", 0)
    # Force the copy fallback: with rename succeeding the bytes would already
    # be installed before verification fails, so only the fallback path can
    # prove the destination is left untouched.
    monkeypatch.setattr(automatic_upgrade.os, "replace", _failed_replace)
    with pytest.raises(OSError, match="could not be verified") as exc_info:
        automatic_upgrade._replace_database(source, destination)

    assert not isinstance(exc_info.value, FileNotFoundError)
    assert _source_value(destination) == "outdated"
    assert not automatic_upgrade._database_has_marker(destination)


def test_replace_file_retries_transient_staging_flap(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    source = tmp_path / "source.txt"
    destination = tmp_path / "destination.txt"
    source.write_bytes(b"published-content")
    real_open = Path.open
    flap_count = 0

    def flappy_open(self: Path, mode: str = "r", *args: object, **kwargs: object):
        nonlocal flap_count
        if (
            Path(self) == source
            and isinstance(mode, str)
            and mode.startswith("r")
            and flap_count < 1
        ):
            flap_count += 1
            raise FileNotFoundError(errno.ENOENT, "bind-mount flap", str(self))
        return real_open(self, mode, *args, **kwargs)

    monkeypatch.setattr(Path, "open", flappy_open)
    monkeypatch.setattr(automatic_upgrade, "_PUBLISH_VERIFY_INTERVAL_SECONDS", 0)

    automatic_upgrade._replace_file(source, destination)

    assert flap_count == 1
    assert destination.read_bytes() == b"published-content"


def _write_multi_page_database(path: Path, value: str = "original") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(path) as connection:
        connection.execute("CREATE TABLE source_value (value TEXT NOT NULL)")
        connection.execute("INSERT INTO source_value VALUES (?)", (value,))
        connection.execute("CREATE TABLE bulk (payload TEXT NOT NULL)")
        connection.executemany(
            "INSERT INTO bulk VALUES (?)",
            [(f"row-{index}-" + "x" * 120,) for index in range(2000)],
        )


def test_promote_rejects_working_copy_that_fails_quick_check(tmp_path: Path) -> None:
    """F2/H2: page-level rot in the working copy fails promotion closed
    before any live byte is swapped; the marker gate alone cannot see it."""
    settings = _settings(tmp_path)
    _write_multi_page_database(settings.library_db_path)
    backup = automatic_upgrade.capture_upgrade_backup(settings)
    working = automatic_upgrade.prepare_working_copy(settings, backup)
    working_database = working / "cache" / "library.db"
    with sqlite3.connect(working_database) as connection:
        connection.execute("UPDATE source_value SET value = 'migrated'")
    _mark_migrated(working_database)
    # Empirically validated technique: flipping a leaf-page header trips
    # PRAGMA quick_check while the completion-marker gate still passes.
    with open(working_database, "r+b") as handle:
        handle.seek(4096 * 3 + 8)
        handle.write(b"\xde\xad\xbe\xef" * 4)
    assert automatic_upgrade._database_has_marker(working_database)

    live_before = automatic_upgrade._sha256(settings.library_db_path)
    with pytest.raises(AutomaticUpgradeError, match="quick_check"):
        automatic_upgrade.promote_working_copy(settings, working)

    assert automatic_upgrade._sha256(settings.library_db_path) == live_before


def test_promote_healthy_working_copy_installs_itself(tmp_path: Path) -> None:
    """Positive companion for the quick_check gate: an intact working copy
    promotes unchanged through the same code path."""
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)
    backup = automatic_upgrade.capture_upgrade_backup(settings)
    working = automatic_upgrade.prepare_working_copy(settings, backup)
    working_database = working / "cache" / "library.db"
    with sqlite3.connect(working_database) as connection:
        connection.execute("UPDATE source_value SET value = 'migrated'")
    _mark_migrated(working_database)

    automatic_upgrade.promote_working_copy(settings, working)

    assert automatic_upgrade._database_has_marker(settings.library_db_path)
    assert _source_value(settings.library_db_path) == "migrated"


def test_wal_quarantine_survives_crash_between_unlink_and_replace(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """F7/H7: a crash after the destination WAL is quarantined but before the
    swap leaves the old WAL recoverable beside the database; the restart
    restore path installs the manifest-verified backup bytes and sweeps the
    quarantine siblings."""
    settings = _settings(tmp_path)
    settings.library_db_path.parent.mkdir(parents=True)
    crash = {"armed": True}
    real_replace = os.replace

    class SimulatedPromotionCrash(BaseException):
        pass

    def crashing_replace(source: object, destination: object, **kwargs: object):
        if crash["armed"] and Path(str(destination)) == settings.library_db_path:
            raise SimulatedPromotionCrash
        return real_replace(source, destination, **kwargs)

    live_connection = sqlite3.connect(settings.library_db_path)
    try:
        assert (
            live_connection.execute("PRAGMA journal_mode=WAL").fetchone()[0] == "wal"
        )
        live_connection.execute("CREATE TABLE source_value (value TEXT NOT NULL)")
        live_connection.execute("INSERT INTO source_value VALUES ('original')")
        live_connection.execute("CREATE TABLE wal_value (value TEXT NOT NULL)")
        live_connection.execute("INSERT INTO wal_value VALUES ('committed')")
        live_connection.commit()
        backup = automatic_upgrade.capture_upgrade_backup(settings)
        manifest = json.loads(
            (backup.directory / "manifest.json").read_text(encoding="utf-8")
        )

        working = automatic_upgrade.prepare_working_copy(settings, backup)
        working_database = working / "cache" / "library.db"
        with sqlite3.connect(working_database) as connection:
            connection.execute("UPDATE source_value SET value = 'migrated'")
        _mark_migrated(working_database)

        # Committed frames that exist ONLY in the -wal file at promote time.
        live_connection.execute("INSERT INTO wal_value VALUES ('late-wal-row')")
        live_connection.commit()
        assert Path(f"{settings.library_db_path}-wal").is_file()

        monkeypatch.setattr(
            automatic_upgrade.os, "replace", crashing_replace
        )
        with pytest.raises(SimulatedPromotionCrash):
            automatic_upgrade.promote_working_copy(settings, working)
    finally:
        live_connection.close()

    quarantined_wal = list(
        settings.library_db_path.parent.glob(
            f".{settings.library_db_path.name}-wal.upgrade-*.quarantine"
        )
    )
    assert len(quarantined_wal) == 1
    assert b"late-wal-row" in quarantined_wal[0].read_bytes()
    assert not Path(f"{settings.library_db_path}-wal").exists()

    crash["armed"] = False
    restored = automatic_upgrade._load_upgrade_backup(
        settings, str(backup.directory)
    )
    automatic_upgrade.restore_upgrade_backup(settings, restored)

    assert automatic_upgrade._sha256(settings.library_db_path) == manifest[
        "database_sha256"
    ]
    assert not list(settings.library_db_path.parent.glob("*.quarantine"))
    recovered = sqlite3.connect(settings.library_db_path)
    try:
        assert (
            recovered.execute("SELECT value FROM source_value").fetchone()[0]
            == "original"
        )
        assert (
            recovered.execute("SELECT value FROM wal_value").fetchone()[0]
            == "committed"
        )
    finally:
        recovered.close()


def test_upgrade_completes_when_atomic_rename_is_unavailable(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)
    settings.config_file_path.parent.mkdir(parents=True)
    settings.config_file_path.write_text('{"name":"before"}', encoding="utf-8")
    monkeypatch.setenv("COMMIT_TAG", "test-version")

    def migrate(working: Path) -> dict[str, object]:
        working_database = working / "cache" / "library.db"
        with sqlite3.connect(working_database) as connection:
            connection.execute("UPDATE source_value SET value = 'migrated'")
        (working / "config" / "config.json").write_text(
            '{"name":"after"}', encoding="utf-8"
        )
        _mark_migrated(working_database)
        return {"passed": True}

    monkeypatch.setattr(automatic_upgrade.os, "replace", _failed_replace)
    result = run_automatic_copy_upgrade(settings, runner=migrate)

    assert result == "upgraded"
    assert automatic_upgrade._database_has_marker(settings.library_db_path)
    assert _source_value(settings.library_db_path) == "migrated"
    state = json.loads(
        (settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    assert state["stage"] == "completed"
    assert settings.config_file_path.read_text(encoding="utf-8") == '{"name":"after"}'


def test_write_state_falls_back_when_rename_unavailable(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    path = tmp_path / "state.json"
    payload = {"stage": "completed", "attempt": 1}
    monkeypatch.setattr(automatic_upgrade.os, "replace", _failed_replace)

    automatic_upgrade._write_state(path, payload)

    assert automatic_upgrade._read_state(path) == payload
@pytest.mark.asyncio
async def test_perform_target_migration_carries_probe_timeout_evidence_and_always_closes_reconciler(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # Use a historical root that will be probed and will timeout
    historical_root = tmp_path / "Historical" / "Music"
    historical_root.mkdir(parents=True)
    (historical_root / "track.flac").write_bytes(b"a" * 100)
    database = tmp_path / "cache" / "library.db"
    database.parent.mkdir(parents=True)
    from tests.infrastructure.test_legacy_catalog_importer import _create_source

    _create_source(database, historical_root)
    # Mock Path.stat to block for Historical for longer than probe timeout (5s)
    blocked = threading.Event()
    orig_stat = Path.stat

    def blocking_stat(self, *args, **kwargs):  # type: ignore[no-untyped-def]
        if "Historical" in str(self):
            blocked.wait(timeout=10.0)
        return orig_stat(self, *args, **kwargs)

    monkeypatch.setattr(Path, "stat", blocking_stat)
    # Mock other expensive phases, but not the reconciler's reconcile (the behavior under assertion)
    monkeypatch.setattr(automatic_upgrade, "migrate_legacy_config", lambda: None)
    from core.config import Settings
    from infrastructure.persistence.native_library_store import NativeLibraryStore
    from api.v1.schemas.library_policies import LibraryRootSettings, TypedLibrarySettings
    from services.native.library_policy_resolver import LibraryPolicyResolver

    settings = Settings(
        root_app_dir=tmp_path,
        cache_dir=tmp_path / "cache",
        library_db_path=database,
        config_file_path=tmp_path / "config" / "config.json",
    )
    settings.config_file_path.parent.mkdir(parents=True, exist_ok=True)
    settings.config_file_path.write_text("{}", encoding="utf-8")
    monkeypatch.setattr(automatic_upgrade, "get_settings", lambda: settings)
    # Mock preferences to return a settings with Missing root (so Historical is outside)
    from unittest.mock import MagicMock

    mock_preferences = MagicMock()
    mock_preferences.get_typed_library_settings.return_value = TypedLibrarySettings(
        library_roots=[LibraryRootSettings(id="root", path=str(tmp_path / "Missing" / "Music"), label="Library", policy="automatic")],
        staging_path=str(tmp_path / "Staging"),
    )
    mock_preferences.retarget_library_roots_for_upgrade = MagicMock()
    monkeypatch.setattr("core.dependencies.cache_providers.get_preferences_service", lambda: mock_preferences)
    test_store = NativeLibraryStore(database, threading.Lock())
    monkeypatch.setattr("core.dependencies.cache_providers.get_native_library_store", lambda: test_store)
    # Track reconciler aclose
    from services.native.legacy_path_reconciler import LegacyPathReconciler

    original_aclose = LegacyPathReconciler.aclose
    aclose_called = []

    async def tracked_aclose(self):  # type: ignore[no-untyped-def]
        aclose_called.append(True)
        await original_aclose(self)

    monkeypatch.setattr(LegacyPathReconciler, "aclose", tracked_aclose)
    # Mock migrator to capture projector and skip flag, and return lenient success
    captured = {}

    class FakeMigrator:
        def __init__(self, store, resolver, emit_progress=None, path_projector=None, skip_unmappable_paths=False, **kwargs):  # type: ignore[no-untyped-def]
            captured["path_projector"] = path_projector
            captured["skip_unmappable"] = skip_unmappable_paths
            self.store = store
            self.resolver = resolver

        async def migrate(self, migration_id, now=None):  # type: ignore[no-untyped-def]
            from services.native.bounded_legacy_catalog_migrator import BoundedMigrationOutcome
            from models.library_migration import MigrationDryRunReport

            report = MigrationDryRunReport(
                migration_id=migration_id,
                source_revision="src",
                root_revision="root",
                state="applied",
                identified_albums=0,
                local_only_albums=0,
                identified_tracks=0,
                local_only_tracks=0,
                artists=0,
                reference_counts=[],
                network_calls=0,
                tag_reads=0,
                fingerprints=0,
                embedded_art_reads=0,
            )
            return BoundedMigrationOutcome(
                report=report,
                skipped_counts={"library_file": 2, "review_row": 4},
                blocker_count=0,
            )

    monkeypatch.setattr("services.native.bounded_legacy_catalog_migrator.BoundedLegacyCatalogMigrator", FakeMigrator)

    # Mock validator to avoid needing full DB
    from services.native.target_startup_validator import TargetStartupValidator

    monkeypatch.setattr(TargetStartupValidator, "validate", AsyncMock(return_value={"invariants": {}}))
    # Mock get_library_policy_resolver to avoid cache
    monkeypatch.setattr("core.dependencies.service_providers.get_library_policy_resolver", lambda: LibraryPolicyResolver(mock_preferences.get_typed_library_settings.return_value))
    # GH-300 gate fix: _perform_target_migration imports the resolver from
    # core.dependencies.service_providers at call time (NEW-MIG-02 refactor),
    # so patch that binding; the old maintenance.automatic_upgrade attribute
    # no longer exists.

    try:
        evidence = await automatic_upgrade._perform_target_migration()
        assert aclose_called, "reconciler.aclose should be awaited even on timeout"
        assert captured["path_projector"] is None
        assert captured["skip_unmappable"] is True
        assert "path_reconciliation" in evidence
        assert evidence["path_reconciliation"]["failure_reason"] == "legacy_path_probe_timeout"
        assert evidence["path_reconciliation"]["mode"] == "blocked"
        # Sanitized: no raw historical path in evidence
        import json

        assert str(historical_root) not in json.dumps(evidence["path_reconciliation"])
        assert evidence.get("skipped", {}).get("library_file", 0) >= 2
    finally:
        blocked.set()



def _forbidden_work_settings(tmp_path: Path) -> tuple[Settings, Path]:
    historical_root = tmp_path / "Historical" / "Music"
    historical_root.mkdir(parents=True)
    (historical_root / "track.flac").write_bytes(b"a" * 100)
    database = tmp_path / "cache" / "library.db"
    database.parent.mkdir(parents=True)
    from tests.infrastructure.test_legacy_catalog_importer import _create_source

    _create_source(database, historical_root)
    settings = Settings(
        root_app_dir=tmp_path,
        cache_dir=tmp_path / "cache",
        library_db_path=database,
        config_file_path=tmp_path / "config" / "config.json",
    )
    settings.config_file_path.parent.mkdir(parents=True, exist_ok=True)
    settings.config_file_path.write_text("{}", encoding="utf-8")
    return settings, database


def _stub_child_providers(
    monkeypatch: pytest.MonkeyPatch,
    settings: Settings,
    database: Path,
):
    """Shared stubbing for in-process _perform_target_migration drives."""
    import threading
    from unittest.mock import MagicMock

    from api.v1.schemas.library_policies import (
        LibraryRootSettings,
        TypedLibrarySettings,
    )
    from infrastructure.persistence.native_library_store import NativeLibraryStore
    from services.native.library_policy_resolver import LibraryPolicyResolver

    monkeypatch.setattr(automatic_upgrade, "get_settings", lambda: settings)
    monkeypatch.setattr(automatic_upgrade, "migrate_legacy_config", lambda: None)
    mock_preferences = MagicMock()
    mock_preferences.get_typed_library_settings.return_value = TypedLibrarySettings(
        library_roots=[
            LibraryRootSettings(
                id="root",
                path=str(settings.root_app_dir / "Missing" / "Music"),
                label="Library",
                policy="automatic",
            )
        ],
        staging_path=str(settings.root_app_dir / "Staging"),
    )
    mock_preferences.retarget_library_roots_for_upgrade = MagicMock()
    monkeypatch.setattr(
        "core.dependencies.cache_providers.get_preferences_service",
        lambda: mock_preferences,
    )
    test_store = NativeLibraryStore(database, threading.Lock())
    monkeypatch.setattr(
        "core.dependencies.cache_providers.get_native_library_store",
        lambda: test_store,
    )
    monkeypatch.setattr(
        "core.dependencies.service_providers.get_library_policy_resolver",
        lambda: LibraryPolicyResolver(
            mock_preferences.get_typed_library_settings.return_value
        ),
    )
    return test_store


@pytest.mark.asyncio
async def test_forbidden_work_abort_writes_failure_evidence(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """F3/H3a: the forbidden-work guard leaves the counters that name the
    offending guard behind instead of raising bare."""
    from models.library_migration import MigrationDryRunReport
    from services.native.bounded_legacy_catalog_migrator import (
        BoundedMigrationOutcome,
    )

    class FakeMigrator:
        def __init__(self, *args, **kwargs) -> None:  # type: ignore[no-untyped-def]
            pass

        async def migrate(self, migration_id: str, now: float | None = None):
            report = MigrationDryRunReport(
                migration_id=migration_id,
                source_revision="src",
                root_revision="root",
                state="applied",
                identified_albums=0,
                local_only_albums=0,
                identified_tracks=0,
                local_only_tracks=0,
                artists=0,
                reference_counts=[],
                network_calls=1,
                tag_reads=0,
                fingerprints=0,
                embedded_art_reads=0,
            )
            return BoundedMigrationOutcome(report=report, blocker_count=0)

    settings, database = _forbidden_work_settings(tmp_path)
    _stub_child_providers(monkeypatch, settings, database)
    monkeypatch.setattr(
        "services.native.bounded_legacy_catalog_migrator.BoundedLegacyCatalogMigrator",
        FakeMigrator,
    )

    with pytest.raises(AutomaticUpgradeError, match="not allowed during startup"):
        await automatic_upgrade._perform_target_migration()

    failure = json.loads(
        (settings.cache_dir / automatic_upgrade._FAILURE_EVIDENCE_FILE).read_text(
            encoding="utf-8"
        )
    )
    assert failure["reason"] == "forbidden_work"
    assert failure["network_calls"] == 1
    assert failure["tag_reads"] == 0
    assert "path_reconciliation" in failure


def test_unrestorable_promotion_failure_is_recorded_truthfully(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """F3/H3c: when the post-promotion restore itself fails, the state record
    says so explicitly instead of leaving a bare `promoting` stage."""
    settings = _settings(tmp_path)
    _write_unmigrated_database(settings.library_db_path)

    def runner(_working: Path) -> dict[str, object]:
        return {"passed": True}

    def broken_promote(_settings: Settings, _working: Path) -> None:
        raise RuntimeError("simulated promotion crash")

    def broken_restore(_settings: Settings, _backup: object) -> None:
        raise OSError("restore volume unavailable")

    monkeypatch.setattr(automatic_upgrade, "promote_working_copy", broken_promote)
    monkeypatch.setattr(automatic_upgrade, "restore_upgrade_backup", broken_restore)

    with pytest.raises(AutomaticUpgradeError) as error:
        run_automatic_copy_upgrade(settings, runner=runner)

    message = str(error.value)
    assert "backup could not be restored" in message
    assert "Do not start an older image" in message
    state = json.loads(
        (settings.cache_dir / f"automatic-upgrade-{UPGRADE_ID}.json").read_text(
            encoding="utf-8"
        )
    )
    assert state["stage"] == "promoting"
    assert state["restore_failed"] is True
    assert state["restore_error_type"] == "OSError"


@pytest.mark.asyncio
async def test_success_evidence_carries_breakdowns_timings_and_identities(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """F4/H4: success evidence carries the per-kind provenance breakdown,
    migrator phase timings, and source/config/image identities."""
    from unittest.mock import AsyncMock

    import services.native.legacy_path_reconciler as lpr_module
    from api.v1.schemas.library_policies import (
        LibraryRootSettings,
        TypedLibrarySettings,
    )
    from services.native.library_policy_resolver import LibraryPolicyResolver
    from services.native.target_startup_validator import TargetStartupValidator

    settings, database = _forbidden_work_settings(tmp_path)
    resolvable_roots = TypedLibrarySettings(
        library_roots=[
            LibraryRootSettings(
                id="root",
                path=str(tmp_path / "Historical" / "Music"),
                label="Library",
                policy="automatic",
            )
        ],
        staging_path=str(tmp_path / "Staging"),
    )
    # /tmp is a blocked probe prefix in production; relax it exactly like the
    # reconciler tests so the real reconciler can prove the seeded files.
    monkeypatch.setattr(lpr_module, "_BLOCKED_ROOTS", (Path("/"),))
    test_store = _stub_child_providers(monkeypatch, settings, database)
    monkeypatch.setattr(
        "core.dependencies.service_providers.get_library_policy_resolver",
        lambda: LibraryPolicyResolver(resolvable_roots),
    )
    monkeypatch.setattr(
        TargetStartupValidator,
        "validate",
        AsyncMock(return_value={"invariants": {}}),
    )
    monkeypatch.setenv("COMMIT_TAG", "evidence-test")

    evidence = await automatic_upgrade._perform_target_migration()

    store_counts = await test_store.get_migration_provenance_counts(
        automatic_upgrade.MIGRATION_ID
    )
    assert store_counts
    assert evidence["reference_counts"] == dict(sorted(store_counts.items()))
    timings = evidence["phase_timings_ms"]
    assert timings and all(value >= 0 for value in timings.values())
    assert "Migrating identified catalog tracks" in timings
    assert evidence["source_sha256"] == automatic_upgrade._sha256(database)
    assert evidence["config_sha256"] == automatic_upgrade._sha256(
        settings.config_file_path
    )
    assert evidence["image_version"] == "evidence-test"
    assert evidence["invariants"] == {}
