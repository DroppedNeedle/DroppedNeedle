import asyncio
import threading
from pathlib import Path

import pytest

from api.v1.schemas.settings import LibrarySyncSettings, UserPreferences
from core.config import Settings
from services.preferences_service import PreferencesService


@pytest.mark.asyncio
async def test_concurrent_section_saves_merge_from_one_config_snapshot(
    tmp_path: Path, monkeypatch
) -> None:
    config_path = tmp_path / "config.json"
    settings = Settings()
    settings.config_file_path = config_path
    service = PreferencesService(settings)

    first_save_started = threading.Event()
    release_first_save = threading.Event()
    save_calls = 0
    save_calls_lock = threading.Lock()
    original_save_config = service._save_config

    def blocking_first_save(config: dict) -> None:
        nonlocal save_calls
        with save_calls_lock:
            save_calls += 1
            is_first_save = save_calls == 1
        if is_first_save:
            first_save_started.set()
            assert release_first_save.wait(timeout=5)
        original_save_config(config)

    monkeypatch.setattr(service, "_save_config", blocking_first_save)

    first = asyncio.create_task(
        asyncio.to_thread(
            service.save_preferences, UserPreferences(primary_types=["album"])
        )
    )
    assert await asyncio.to_thread(first_save_started.wait, 5)

    second = asyncio.create_task(
        asyncio.to_thread(
            service.save_library_sync_settings,
            LibrarySyncSettings(sync_frequency="1hr"),
        )
    )
    await asyncio.sleep(0)
    release_first_save.set()

    await asyncio.gather(first, second)
    config = service._load_config()
    assert config["user_preferences"]["primary_types"] == ["album"]
    assert config["library_sync_settings"]["sync_frequency"] == "1hr"


def _service(tmp_path: Path) -> tuple[PreferencesService, Settings]:
    config_path = tmp_path / "config.json"
    settings = Settings()
    settings.config_file_path = config_path
    return PreferencesService(settings), settings


@pytest.mark.asyncio
async def test_release_policy_revision_normalizes_case_order_and_duplicates(
    tmp_path: Path,
) -> None:
    service, _settings = _service(tmp_path)
    first = UserPreferences(
        primary_types=[" Album ", "album", "EP"],
        secondary_types=["Studio", "studio"],
    )
    service.save_preferences(first)
    _, revision = service.get_preferences_with_revision()
    assert revision == 1

    service.save_preferences(
        UserPreferences(
            primary_types=["ep", "ALBUM", "album"],
            secondary_types=["studio", " STUDIO "],
        )
    )
    _, unchanged_revision = service.get_preferences_with_revision()
    assert unchanged_revision == revision


@pytest.mark.asyncio
async def test_release_policy_revision_increments_on_a_to_b_to_a(
    tmp_path: Path,
) -> None:
    service, _settings = _service(tmp_path)
    policy_a = UserPreferences(primary_types=["album"], secondary_types=["studio"])
    policy_b = UserPreferences(primary_types=["single"], secondary_types=["demo"])

    service.save_preferences(policy_a)
    _, revision_a = service.get_preferences_with_revision()
    service.save_preferences(policy_b)
    _, revision_b = service.get_preferences_with_revision()
    service.save_preferences(policy_a)
    _, revision_again = service.get_preferences_with_revision()

    assert (revision_a, revision_b, revision_again) == (1, 2, 3)


@pytest.mark.asyncio
async def test_release_policy_revision_survives_service_restart(tmp_path: Path) -> None:
    service, settings = _service(tmp_path)
    service.save_preferences(
        UserPreferences(primary_types=["album"], secondary_types=["soundtrack"])
    )
    _, expected_revision = service.get_preferences_with_revision()

    restarted = PreferencesService(settings)

    preferences, revision = restarted.get_preferences_with_revision()
    assert preferences.secondary_types == ["soundtrack"]
    assert revision == expected_revision
