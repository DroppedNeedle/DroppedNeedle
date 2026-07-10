"""PreferencesService library settings: defaults, seeding, AcoustID mask/preserve."""

import json
from pathlib import Path

import pytest

from api.v1.schemas.settings import ACOUSTID_KEY_MASK, LibrarySettings, LibrarySyncSettings
from core.config import Settings
from services.preferences_service import PreferencesService


@pytest.fixture
def prefs(tmp_path: Path) -> PreferencesService:
    settings = Settings()
    settings.config_file_path = tmp_path / "config.json"
    return PreferencesService(settings)


def test_defaults_when_unset(prefs):
    settings = prefs.get_library_settings()
    assert settings.library_paths == ["/music"]
    assert settings.acoustid_api_key == ""  # no key configured yet


def test_scan_schedule_defaults_when_unset(prefs):
    assert prefs.get_library_scan_schedule().scan_frequency == "24hr"


def test_scan_schedule_migrates_from_legacy_sync_settings(prefs):
    prefs.save_library_sync_settings(LibrarySyncSettings(sync_frequency="6hr"))
    sched = prefs.get_library_scan_schedule()
    assert sched.scan_frequency == "6hr"  # carried over from the old Lidarr-era setting
    stored = json.loads(prefs._config_path.read_text())
    assert stored["library_scan_schedule"]["scan_frequency"] == "6hr"  # persisted under the new key


def test_acoustid_key_masked_on_read_decrypted_raw(prefs):
    prefs.save_library_settings(
        LibrarySettings(library_paths=["/m"], acoustid_api_key="secret-key")
    )
    assert prefs.get_library_settings().acoustid_api_key == ACOUSTID_KEY_MASK
    assert prefs.get_library_settings_raw().acoustid_api_key == "secret-key"


def test_acoustid_key_stored_encrypted(prefs):
    prefs.save_library_settings(
        LibrarySettings(library_paths=["/m"], acoustid_api_key="secret-key")
    )
    stored = json.loads(prefs._config_path.read_text())["library_settings"]["acoustid_api_key"]
    assert stored != "secret-key"  # ciphertext, not plaintext
    assert stored != ""


def test_mask_on_save_preserves_existing_key(prefs):
    prefs.save_library_settings(
        LibrarySettings(library_paths=["/m"], acoustid_api_key="secret-key")
    )
    # Re-save with the mask sentinel (as the UI would) + changed paths.
    prefs.save_library_settings(
        LibrarySettings(library_paths=["/m2"], acoustid_api_key=ACOUSTID_KEY_MASK)
    )
    raw = prefs.get_library_settings_raw()
    assert raw.acoustid_api_key == "secret-key"  # secret preserved
    assert raw.library_paths == ["/m2"]  # non-secret fields updated


def test_lyrics_fetch_enabled_defaults_off_and_round_trips(prefs):
    assert prefs.get_library_settings().lyrics_fetch_enabled is False  # off by default
    prefs.save_library_settings(
        LibrarySettings(library_paths=["/m"], lyrics_fetch_enabled=True)
    )
    assert prefs.get_library_settings().lyrics_fetch_enabled is True
    assert prefs.get_library_settings_raw().lyrics_fetch_enabled is True


def test_seeds_library_paths_from_legacy_root(tmp_path: Path):
    settings = Settings()
    settings.config_file_path = tmp_path / "config.json"
    settings.config_file_path.parent.mkdir(parents=True, exist_ok=True)
    settings.config_file_path.write_text(
        json.dumps({"_legacy_lidarr": {"root_folder_path": "/legacy/music"}})
    )
    prefs = PreferencesService(settings)
    assert prefs.get_library_settings().library_paths == ["/legacy/music"]
