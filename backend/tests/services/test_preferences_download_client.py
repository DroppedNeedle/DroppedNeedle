"""PreferencesService download-client settings: defaults, slskd key mask/preserve/encrypt, thresholds."""

import json
import logging
from pathlib import Path

import msgspec
import pytest

from api.v1.schemas.settings import DOWNLOAD_CLIENT_API_KEY_MASK, DownloadClientConnectionSettings
from core.config import Settings
from services.preferences_service import PreferencesService


@pytest.fixture
def prefs(tmp_path: Path) -> PreferencesService:
    settings = Settings()
    settings.config_file_path = tmp_path / "config.json"
    return PreferencesService(settings)


def test_defaults_when_unset(prefs):
    s = prefs.get_download_client_settings()
    assert s.url == ""
    assert s.api_key == ""
    assert s.verify_downloads is True
    assert s.min_bitrate_kbps == 128
    assert s.preflight_score_auto_accept == 0.70
    assert s.preflight_score_manual_min == 0.50


def test_key_masked_on_read_decrypted_raw(prefs):
    prefs.save_download_client_settings(
        DownloadClientConnectionSettings(url="http://slskd:5030", api_key="secret-key")
    )
    assert prefs.get_download_client_settings().api_key == DOWNLOAD_CLIENT_API_KEY_MASK
    assert prefs.get_download_client_settings_raw().api_key == "secret-key"


def test_key_stored_encrypted(prefs):
    prefs.save_download_client_settings(DownloadClientConnectionSettings(api_key="secret-key"))
    stored = json.loads(prefs._config_path.read_text())["download_client"]["api_key"]
    assert stored != "secret-key"  # ciphertext
    assert stored != ""


def test_mask_on_save_preserves_existing_key(prefs):
    prefs.save_download_client_settings(
        DownloadClientConnectionSettings(url="http://a:5030", api_key="secret-key")
    )
    # Re-save with the masked sentinel + a changed url - key must be preserved.
    prefs.save_download_client_settings(
        DownloadClientConnectionSettings(url="http://b:5030", api_key=DOWNLOAD_CLIENT_API_KEY_MASK)
    )
    raw = prefs.get_download_client_settings_raw()
    assert raw.api_key == "secret-key"  # preserved
    assert raw.url == "http://b:5030"  # updated


def test_api_key_never_logged(prefs, caplog):
    # task-040: the slskd api_key must never appear in logs, even at DEBUG.
    with caplog.at_level(logging.DEBUG):
        prefs.save_download_client_settings(
            DownloadClientConnectionSettings(url="http://slskd:5030", api_key="super-secret-key")
        )
        prefs.get_download_client_settings()
        prefs.get_download_client_settings_raw()
    assert "super-secret-key" not in caplog.text


def test_url_scheme_normalised_for_bare_host():
    # A bare host gets https:// prepended (+ trailing slash stripped) so the saved
    # and Test-connection URLs are always full URLs - httpx rejects a schemeless one.
    assert DownloadClientConnectionSettings(url="slskd.harveybragg.com/").url == (
        "https://slskd.harveybragg.com"
    )


def test_url_scheme_preserved_when_already_present():
    assert DownloadClientConnectionSettings(url="http://slskd:5030").url == "http://slskd:5030"
    assert DownloadClientConnectionSettings(url="").url == ""


def test_downloads_subpath_sanitised_and_round_trips(prefs):
    # leading slashes / "." / ".." components are stripped so the subpath can never
    # escape the mount, and the safe remainder persists.
    assert DownloadClientConnectionSettings(downloads_subpath="/downloads/slskd/").downloads_subpath == (
        "downloads/slskd"
    )
    assert DownloadClientConnectionSettings(downloads_subpath="../../etc").downloads_subpath == "etc"
    prefs.save_download_client_settings(
        DownloadClientConnectionSettings(url="http://a:5030", downloads_subpath="downloads/slskd/complete")
    )
    assert prefs.get_download_client_settings().downloads_subpath == "downloads/slskd/complete"


def test_auto_retry_fields_default_and_validate():
    d = DownloadClientConnectionSettings()
    assert d.auto_retry_enabled is True
    assert d.auto_retry_max_attempts == 6
    assert d.auto_retry_base_interval_minutes == 15

    with pytest.raises(msgspec.ValidationError, match="auto_retry_max_attempts"):
        DownloadClientConnectionSettings(auto_retry_max_attempts=21)
    with pytest.raises(msgspec.ValidationError, match="auto_retry_base_interval_minutes"):
        DownloadClientConnectionSettings(auto_retry_base_interval_minutes=0)


def test_quality_and_threshold_fields_persist(prefs):
    prefs.save_download_client_settings(
        DownloadClientConnectionSettings(
            min_bitrate_kbps=320,
            verify_downloads=False,
            preflight_score_auto_accept=0.80,
            preflight_score_manual_min=0.40,
        )
    )
    raw = prefs.get_download_client_settings_raw()
    assert raw.min_bitrate_kbps == 320
    assert raw.verify_downloads is False
    assert raw.preflight_score_auto_accept == 0.80
    assert raw.preflight_score_manual_min == 0.40
