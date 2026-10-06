"""PreferencesService: the NZBGet connection (mask/preserve/encrypt) and the
one-Usenet-client-at-a-time invariant it shares with SABnzbd."""

from pathlib import Path

import pytest

from api.v1.schemas.settings import (
    NZBGET_PASSWORD_MASK,
    NzbgetConnectionSettings,
    SabnzbdConnectionSettings,
)
from core.config import Settings
from services.preferences_service import PreferencesService


@pytest.fixture
def prefs(tmp_path: Path) -> PreferencesService:
    settings = Settings()
    settings.config_file_path = tmp_path / "config.json"
    return PreferencesService(settings)


def _nzbget(**overrides) -> NzbgetConnectionSettings:
    return NzbgetConnectionSettings(
        **{
            "enabled": True,
            "url": "http://nzbget:6789",
            "username": "nzbget",
            "password": "secret",
            "downloads_mount": "/nzbget-downloads",
            **overrides,
        }
    )


def _sabnzbd(**overrides) -> SabnzbdConnectionSettings:
    return SabnzbdConnectionSettings(
        **{"enabled": True, "url": "http://sab:8080", "api_key": "key", **overrides}
    )


def test_password_is_masked_on_read_and_decrypted_for_the_client(prefs):
    prefs.save_nzbget_connection(_nzbget())
    assert prefs.get_nzbget_connection().password == NZBGET_PASSWORD_MASK
    assert prefs.get_nzbget_connection_raw().password == "secret"


def test_password_is_not_stored_in_plaintext(prefs, tmp_path):
    prefs.save_nzbget_connection(_nzbget())
    assert "secret" not in (tmp_path / "config.json").read_text(encoding="utf-8")


def test_saving_the_mask_preserves_the_stored_password(prefs):
    prefs.save_nzbget_connection(_nzbget())
    # The UI round-trips the masked sentinel, which must not overwrite the real one.
    prefs.save_nzbget_connection(_nzbget(password=NZBGET_PASSWORD_MASK, category="music"))
    assert prefs.get_nzbget_connection_raw().password == "secret"
    assert prefs.get_nzbget_connection_raw().category == "music"


def test_enabling_nzbget_stands_sabnzbd_down(prefs):
    prefs.save_sabnzbd_connection(_sabnzbd())
    prefs.save_nzbget_connection(_nzbget())
    # Exactly one client owns the usenet source; a saved pair the engine won't honour
    # is worse than making the switch explicit.
    assert prefs.get_nzbget_connection_raw().enabled is True
    assert prefs.get_sabnzbd_connection_raw().enabled is False
    # Credentials survive, so flipping back is one click.
    assert prefs.get_sabnzbd_connection_raw().api_key == "key"


def test_enabling_sabnzbd_stands_nzbget_down(prefs):
    prefs.save_nzbget_connection(_nzbget())
    prefs.save_sabnzbd_connection(_sabnzbd())
    assert prefs.get_sabnzbd_connection_raw().enabled is True
    assert prefs.get_nzbget_connection_raw().enabled is False
    assert prefs.get_nzbget_connection_raw().password == "secret"


def test_saving_a_disabled_client_leaves_the_other_alone(prefs):
    prefs.save_sabnzbd_connection(_sabnzbd())
    prefs.save_nzbget_connection(_nzbget(enabled=False))
    # Only ENABLING is exclusive: editing a parked client's settings must not disable
    # the one actually running.
    assert prefs.get_sabnzbd_connection_raw().enabled is True


def test_usenet_is_ready_with_only_nzbget_enabled(prefs, monkeypatch):
    prefs.save_nzbget_connection(_nzbget())
    monkeypatch.setattr(prefs, "get_usenet_search_backend", lambda: "prowlarr")
    monkeypatch.setattr(prefs, "is_prowlarr_configured", lambda: True)
    # Before this, readiness required SABnzbd specifically, so an NZBGet-only install
    # silently skipped the Usenet source entirely.
    assert prefs.is_usenet_ready() is True
