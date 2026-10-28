from __future__ import annotations

import subprocess
from pathlib import Path
from xml.etree import ElementTree

import pytest


REPOSITORY_ROOT = Path(__file__).resolve().parents[3]


def test_entrypoint_defaults_to_secure_configurable_umask() -> None:
    entrypoint = REPOSITORY_ROOT / "entrypoint.sh"
    result = subprocess.run(
        ["sh", "-n", str(entrypoint)],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    source = entrypoint.read_text()
    assert "REQUESTED_UMASK=${UMASK:-027}" in source
    assert 'umask "$REQUESTED_UMASK"' in source


@pytest.mark.parametrize("value", ["22x", "22", "888", "1027", "7777"])
def test_entrypoint_rejects_invalid_umask_before_startup(value: str) -> None:
    result = subprocess.run(
        ["sh", str(REPOSITORY_ROOT / "entrypoint.sh"), "true"],
        check=False,
        capture_output=True,
        text=True,
        env={"UMASK": value},
    )

    assert result.returncode == 1
    assert "must be three or four octal digits" in result.stdout


def test_entrypoint_fails_fast_when_app_is_shadowed(tmp_path: Path) -> None:
    """A bind mount over /app (code files missing from an existing /app-like
    directory) must fail with the actionable message before any other init
    step, while a missing directory (outside the container) must not."""
    entrypoint = REPOSITORY_ROOT / "entrypoint.sh"
    script = entrypoint.read_text()

    shadowed_root = tmp_path / "app"
    shadowed_root.mkdir()
    relocated = script.replace("/app", str(shadowed_root))
    script_file = tmp_path / "entrypoint.sh"
    script_file.write_text(relocated)

    result = subprocess.run(
        ["sh", str(script_file), "true"],
        check=False,
        capture_output=True,
        text=True,
        env={"UMASK": "888"},
    )

    assert result.returncode == 1
    assert "does not contain the DroppedNeedle application code" in result.stdout
    assert "Mount data subdirectories only" in result.stdout


def test_entrypoint_skips_shadow_check_when_app_missing(tmp_path: Path) -> None:
    """Without an /app directory the shadow check is skipped (host/test
    environment) and normal init validation runs instead."""
    entrypoint = REPOSITORY_ROOT / "entrypoint.sh"
    script = entrypoint.read_text()
    missing_root = tmp_path / "no-such-app"
    assert not missing_root.exists()
    relocated = script.replace("/app", str(missing_root))
    script_file = tmp_path / "entrypoint.sh"
    script_file.write_text(relocated)

    result = subprocess.run(
        ["sh", str(script_file), "true"],
        check=False,
        capture_output=True,
        text=True,
        env={"UMASK": "888"},
    )

    assert result.returncode == 1
    assert "must be three or four octal digits" in result.stdout
    assert "does not contain the DroppedNeedle application code" not in result.stdout


def test_puid_remap_does_not_chown_the_application_tree() -> None:
    """`usermod -u` chowns everything under the user's home. With /app as the
    home that copied the whole application into the container layer and
    stalled startup for minutes on every recreate with a non-default PUID."""
    dockerfile = (REPOSITORY_ROOT / "Dockerfile").read_text()
    entrypoint = (REPOSITORY_ROOT / "entrypoint.sh").read_text()

    assert "-d /nonexistent" in dockerfile
    assert "-d /app" not in dockerfile
    assert 'usermod -o -u "$PUID" -d /nonexistent droppedneedle' in entrypoint
    for data_dir in ("/app/config", "/app/cache", "/app/plugins", "/app/imports"):
        assert data_dir in dockerfile


def test_unraid_template_uses_the_secure_default() -> None:
    root = ElementTree.parse(REPOSITORY_ROOT / "templates/droppedneedle.xml").getroot()
    setting = next(
        item for item in root.findall("Config") if item.get("Target") == "UMASK"
    )

    assert setting.get("Default") == "027"
    assert setting.text == "027"
