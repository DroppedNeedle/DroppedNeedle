import logging
import os
from pathlib import Path
import subprocess
import sys
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

import droppedneedle_cli
from core.exceptions import AuthenticationError


def test_cli_help_does_not_load_runtime_config(tmp_path):
    config_dir = tmp_path / "config"
    config_dir.mkdir()
    (config_dir / "config.json").write_text('{"plugins": {}}')
    environment = os.environ.copy()
    environment["ROOT_APP_DIR"] = str(tmp_path)

    result = subprocess.run(
        [sys.executable, "-m", "droppedneedle_cli", "--help"],
        cwd=Path(__file__).parents[2],
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0
    assert "Unknown config key" not in result.stderr


@pytest.mark.asyncio
async def test_cli_prints_recovery_code_without_password_data(monkeypatch, capsys):
    service = SimpleNamespace(
        create_password_recovery_code_for_username=AsyncMock(
            return_value=("AAAA-BBBB-CCCC-DDDD-EEEE", "2026-07-17T17:00:00Z")
        )
    )
    monkeypatch.setattr(droppedneedle_cli, "get_auth_service", lambda: service)

    assert await droppedneedle_cli._create_recovery_code("admin") == 0
    output = capsys.readouterr().out
    assert "AAAA-BBBB-CCCC-DDDD-EEEE" in output
    assert "/recover-password" in output
    service.create_password_recovery_code_for_username.assert_awaited_once_with("admin")


@pytest.mark.asyncio
async def test_cli_reports_accounts_without_local_recovery(monkeypatch, capsys):
    service = SimpleNamespace(
        create_password_recovery_code_for_username=AsyncMock(
            side_effect=AuthenticationError(
                "Local password recovery is not available for this account"
            )
        )
    )
    monkeypatch.setattr(droppedneedle_cli, "get_auth_service", lambda: service)

    assert await droppedneedle_cli._create_recovery_code("missing") == 1
    assert "not available" in capsys.readouterr().err


def test_cli_hides_unknown_runtime_config_keys(monkeypatch, caplog):
    service = SimpleNamespace()

    def get_service():
        logger = logging.getLogger("core.config")
        logger.warning("Unknown config key '%s', ignoring", "plugins")
        logger.warning("A useful configuration warning")
        return service

    monkeypatch.setattr(droppedneedle_cli, "get_auth_service", get_service)

    with caplog.at_level(logging.WARNING, logger="core.config"):
        assert droppedneedle_cli._get_auth_service_for_cli() is service

    messages = [record.getMessage() for record in caplog.records]
    assert "Unknown config key 'plugins', ignoring" not in messages
    assert "A useful configuration warning" in messages
