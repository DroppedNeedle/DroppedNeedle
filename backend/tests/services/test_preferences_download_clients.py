"""PreferencesService: the shared download_policy (migration-on-read from the legacy
slskd download_client section) + the SABnzbd connection (mask/preserve/encrypt)."""

import json
from pathlib import Path

import pytest

from api.v1.schemas.settings import (
    SABNZBD_API_KEY_MASK,
    QBITTORRENT_API_KEY_MASK,
    DownloadClientConnectionSettings,
    DownloadPolicySettings,
    NewznabIndexerSettings,
    ProwlarrConnectionSettings,
    QbittorrentConnectionSettings,
    SabnzbdConnectionSettings,
)
from core.config import Settings
from services.preferences_service import PreferencesService


@pytest.fixture
def prefs(tmp_path: Path) -> PreferencesService:
    settings = Settings()
    settings.config_file_path = tmp_path / "config.json"
    return PreferencesService(settings)


# --- download-source readiness (single source of truth for "can the user acquire?") ---


def test_no_source_ready_when_nothing_configured(prefs):
    """No user-configured client. Free Music (D24) still makes the user able to
    acquire, which is the point of it - so switch it off to test the old gate."""
    from api.v1.schemas.settings import FreeMusicSettings

    assert prefs.is_soulseek_ready() is False
    assert prefs.is_usenet_ready() is False
    assert prefs.is_builtin_download_ready() is False
    assert prefs.is_download_source_ready() is True  # Free Music, on by default

    prefs.save_free_music_settings(FreeMusicSettings(enabled=False))
    assert prefs.is_download_source_ready() is False


def test_soulseek_ready_makes_source_ready(prefs):
    prefs.save_download_client_settings(
        DownloadClientConnectionSettings(enabled=True, url="http://slskd:5030", api_key="k")
    )
    assert prefs.is_soulseek_ready() is True
    assert prefs.is_download_source_ready() is True


def test_usenet_ready_requires_sabnzbd_and_an_enabled_indexer(prefs):
    # The reported bug: slskd disabled, SABnzbd enabled - Home must NOT show "connect a
    # download client". But SABnzbd alone (no indexer) can't find anything, so it only
    # counts once an enabled indexer exists.
    prefs.save_sabnzbd_connection(
        SabnzbdConnectionSettings(enabled=True, url="http://sab:8080", api_key="k")
    )
    assert prefs.is_usenet_ready() is False  # no indexer yet
    assert prefs.is_builtin_download_ready() is False

    prefs.save_indexer(
        NewznabIndexerSettings(name="DS", url="https://idx.test/api", api_key="k", enabled=True)
    )
    assert prefs.is_usenet_ready() is True
    assert prefs.is_download_source_ready() is True


def test_usenet_only_is_ready_even_with_slskd_disabled(prefs):
    prefs.save_sabnzbd_connection(
        SabnzbdConnectionSettings(enabled=True, url="http://sab:8080", api_key="k")
    )
    prefs.save_indexer(
        NewznabIndexerSettings(name="DS", url="https://idx.test/api", api_key="k", enabled=True)
    )
    # slskd present but disabled - the Usenet path keeps the source "ready".
    prefs.save_download_client_settings(
        DownloadClientConnectionSettings(enabled=False, url="http://slskd:5030", api_key="k")
    )
    assert prefs.is_soulseek_ready() is False
    assert prefs.is_download_source_ready() is True


def test_policy_defaults_when_unset(prefs):
    policy = prefs.get_download_policy()
    assert policy.quality_min == "mp3_320"
    assert policy.preflight_score_auto_accept == 0.70
    assert policy.usenet_min_release_age_minutes == 30


def test_policy_upgrade_fields_default_off_for_preexisting_config(prefs):
    # A config saved before the upgrade/cutoff fields existed must load with
    # upgrades OFF and the cutoff at the band ceiling (CollectionManagement A1).
    prefs._save_config({"download_policy": {"quality_min": "mp3_320", "quality_max": "lossless"}})
    policy = prefs.get_download_policy()
    assert policy.upgrade_allowed is False
    assert policy.quality_cutoff == "lossless"
    assert policy.recycle_bin_path == ""
    assert policy.recycle_retention_days == 30


def test_policy_migrates_from_legacy_download_client(prefs):
    # An upgraded install has policy fields on the old slskd section and NO
    # download_policy section -> get_download_policy derives them (copy-not-delete).
    config = prefs._load_config().copy()
    config["download_client"] = {
        "enabled": True, "client_type": "slskd", "url": "http://slskd:5030", "api_key": "x",
        "quality_min": "lossless", "quality_max": "lossless", "flac_mp3_only": False,
        "preflight_score_auto_accept": 0.85, "max_failover_attempts": 5,
        "auto_retry_base_interval_minutes": 20,
    }
    prefs._save_config(config)

    policy = prefs.get_download_policy()
    assert policy.quality_min == "lossless"
    assert policy.flac_mp3_only is False
    assert policy.preflight_score_auto_accept == 0.85
    assert policy.max_failover_attempts == 5
    # The old key is untouched (rollback-safe).
    assert "download_client" in prefs._load_config()
    assert "download_policy" not in prefs._load_config()  # derived, not written


def test_explicit_policy_takes_precedence_over_legacy(prefs):
    prefs._save_config({
        "download_client": {"quality_min": "low"},
        "download_policy": {"quality_min": "lossless"},
    })
    assert prefs.get_download_policy().quality_min == "lossless"


def test_save_and_read_policy(prefs):
    prefs.save_download_policy(DownloadPolicySettings(usenet_min_release_age_minutes=45))
    assert prefs.get_download_policy().usenet_min_release_age_minutes == 45


def test_source_priority_defaults_soulseek_first(prefs):
    assert prefs.get_source_priority() == ["soulseek", "usenet", "torrent"]


def test_source_priority_save_and_normalise(prefs):
    prefs.save_source_priority(["usenet"])  # only one given -> the others are appended
    assert prefs.get_source_priority() == ["usenet", "soulseek", "torrent"]
    prefs.save_source_priority(["usenet", "bogus", "torrent", "soulseek"])  # unknowns dropped
    assert prefs.get_source_priority() == ["usenet", "torrent", "soulseek"]


def test_sabnzbd_defaults_disabled(prefs):
    sab = prefs.get_sabnzbd_connection()
    assert sab.enabled is False
    assert sab.category == "*"
    assert sab.downloads_mount == "/sabnzbd-downloads"


def test_sabnzbd_key_masked_on_read_decrypted_raw(prefs):
    prefs.save_sabnzbd_connection(
        SabnzbdConnectionSettings(enabled=True, url="http://sab:8080", api_key="full-key")
    )
    assert prefs.get_sabnzbd_connection().api_key == SABNZBD_API_KEY_MASK
    assert prefs.get_sabnzbd_connection_raw().api_key == "full-key"
    stored = json.loads(prefs._config_path.read_text())["download_clients"]["sabnzbd"]["api_key"]
    assert stored not in ("", "full-key")  # encrypted at rest


def test_sabnzbd_masked_save_preserves_key(prefs):
    prefs.save_sabnzbd_connection(
        SabnzbdConnectionSettings(url="http://sab:8080", api_key="full-key")
    )
    prefs.save_sabnzbd_connection(
        SabnzbdConnectionSettings(url="http://new:8080", api_key=SABNZBD_API_KEY_MASK)
    )
    raw = prefs.get_sabnzbd_connection_raw()
    assert raw.api_key == "full-key"  # preserved
    assert raw.url == "http://new:8080"  # updated


def test_qbittorrent_key_masked_encrypted_and_preserved(prefs):
    prefs.save_qbittorrent_connection(
        QbittorrentConnectionSettings(
            enabled=True, url="http://qbt:8080", api_key="full-key"
        )
    )
    assert prefs.get_qbittorrent_connection().api_key == QBITTORRENT_API_KEY_MASK
    assert prefs.get_qbittorrent_connection_raw().api_key == "full-key"
    stored = json.loads(prefs._config_path.read_text())["download_clients"]["qbittorrent"]["api_key"]
    assert stored not in ("", "full-key")

    prefs.save_qbittorrent_connection(
        QbittorrentConnectionSettings(
            enabled=True, url="http://new-qbt:8080", api_key=QBITTORRENT_API_KEY_MASK
        )
    )
    assert prefs.get_qbittorrent_connection_raw().api_key == "full-key"
    assert prefs.get_qbittorrent_connection_raw().url == "http://new-qbt:8080"


def test_torrent_ready_requires_key_selected_client_and_prowlarr(prefs):
    prefs.save_prowlarr_connection(
        ProwlarrConnectionSettings(
            enabled=True, url="http://prowlarr:9696", api_key="p"
        )
    )
    prefs.save_qbittorrent_connection(
        QbittorrentConnectionSettings(
            enabled=True, url="http://qbt:8080", api_key="q"
        )
    )
    assert prefs.is_torrent_ready() is True
