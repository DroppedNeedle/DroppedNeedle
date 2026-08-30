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
