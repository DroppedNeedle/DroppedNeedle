"""PreferencesService download-client settings: defaults, slskd key mask/preserve/encrypt, thresholds."""

import json
import logging
from pathlib import Path

import msgspec
import pytest

from api.v1.schemas.settings import (
    DOWNLOAD_CLIENT_API_KEY_MASK,
    DownloadClientConnectionSettings,
    DownloadPolicySettings,
)
from core.config import Settings
from services.native.acquisition.quality import build_snapshot
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
    prefs.save_download_client_settings(
        DownloadClientConnectionSettings(api_key="secret-key")
    )
    stored = json.loads(prefs._config_path.read_text())["download_client"]["api_key"]
    assert stored != "secret-key"  # ciphertext
    assert stored != ""


def test_mask_on_save_preserves_existing_key(prefs):
    prefs.save_download_client_settings(
        DownloadClientConnectionSettings(url="http://a:5030", api_key="secret-key")
    )
    # Re-save with the masked sentinel + a changed url - key must be preserved.
    prefs.save_download_client_settings(
        DownloadClientConnectionSettings(
            url="http://b:5030", api_key=DOWNLOAD_CLIENT_API_KEY_MASK
        )
    )
    raw = prefs.get_download_client_settings_raw()
    assert raw.api_key == "secret-key"  # preserved
    assert raw.url == "http://b:5030"  # updated


def test_api_key_never_logged(prefs, caplog):
    # task-040: the slskd api_key must never appear in logs, even at DEBUG.
    with caplog.at_level(logging.DEBUG):
        prefs.save_download_client_settings(
            DownloadClientConnectionSettings(
                url="http://slskd:5030", api_key="super-secret-key"
            )
        )
        prefs.get_download_client_settings()
        prefs.get_download_client_settings_raw()
    assert "super-secret-key" not in caplog.text


def test_url_scheme_normalised_for_bare_host():
    # A bare host gets https:// prepended (+ trailing slash stripped) so the saved
    # and Test-connection URLs are always full URLs - httpx rejects a schemeless one.
    assert DownloadClientConnectionSettings(url="slskd.example.com/").url == (
        "https://slskd.example.com"
    )


def test_url_scheme_preserved_when_already_present():
    assert (
        DownloadClientConnectionSettings(url="http://slskd:5030").url
        == "http://slskd:5030"
    )
    assert DownloadClientConnectionSettings(url="").url == ""


def test_downloads_subpath_sanitised_and_round_trips(prefs):
    # leading slashes / "." / ".." components are stripped so the subpath can never
    # escape the mount, and the safe remainder persists.
    assert DownloadClientConnectionSettings(
        downloads_subpath="/downloads/slskd/"
    ).downloads_subpath == ("downloads/slskd")
    assert (
        DownloadClientConnectionSettings(
            downloads_subpath="../../etc"
        ).downloads_subpath
        == "etc"
    )
    prefs.save_download_client_settings(
        DownloadClientConnectionSettings(
            url="http://a:5030", downloads_subpath="downloads/slskd/complete"
        )
    )
    assert (
        prefs.get_download_client_settings().downloads_subpath
        == "downloads/slskd/complete"
    )


def test_incomplete_mount_sanitised_and_round_trips(prefs, tmp_path):
    # Absolute container paths are kept (traversal components dropped); a
    # relative value or bare "/" has no safe meaning here, so it becomes "".
    assert DownloadClientConnectionSettings(
        slskd_incomplete_mount="/data/slskd/incomplete/"
    ).slskd_incomplete_mount == "/data/slskd/incomplete"
    assert (
        DownloadClientConnectionSettings(
            slskd_incomplete_mount="/data/../data/slskd/./incomplete"
        ).slskd_incomplete_mount
        == "/data/data/slskd/incomplete"
    )
    assert (
        DownloadClientConnectionSettings(
            slskd_incomplete_mount="relative/path"
        ).slskd_incomplete_mount
        == ""
    )
    assert (
        DownloadClientConnectionSettings(slskd_incomplete_mount="/").slskd_incomplete_mount
        == ""
    )
    assert DownloadClientConnectionSettings().slskd_incomplete_mount == ""
    target = tmp_path / "incomplete"
    target.mkdir()
    prefs.save_download_client_settings(
        DownloadClientConnectionSettings(
            url="http://a:5030", slskd_incomplete_mount=str(target)
        )
    )
    assert (
        prefs.get_download_client_settings().slskd_incomplete_mount == str(target)
    )
    assert prefs.get_slskd_incomplete_mount() == target.resolve()


def test_incomplete_mount_getter_fails_closed(prefs, tmp_path):
    # Empty, missing, and non-dir values all yield None (fallback skipped).
    assert prefs.get_slskd_incomplete_mount() is None
    prefs.save_download_client_settings(
        DownloadClientConnectionSettings(
            url="http://a:5030",
            slskd_incomplete_mount=str(tmp_path / "no-such-dir"),
        )
    )
    assert prefs.get_slskd_incomplete_mount() is None
    staged = tmp_path / "not-a-dir"
    staged.write_bytes(b"x")
    prefs.save_download_client_settings(
        DownloadClientConnectionSettings(
            url="http://a:5030", slskd_incomplete_mount=str(staged)
        )
    )
    assert prefs.get_slskd_incomplete_mount() is None


def test_save_preserves_flac_mp3_only_alongside_incomplete_mount(prefs, tmp_path):
    # Regression: adding slskd_incomplete_mount to the save allowlist must not
    # drop flac_mp3_only (a save with it False reloaded as default True).
    target = tmp_path / "incomplete"
    target.mkdir()
    prefs.save_download_client_settings(
        DownloadClientConnectionSettings(
            url="http://a:5030",
            flac_mp3_only=False,
            slskd_incomplete_mount=str(target),
        )
    )
    reloaded = prefs.get_download_client_settings()
    assert reloaded.flac_mp3_only is False
    assert reloaded.slskd_incomplete_mount == str(target)


def test_auto_retry_fields_default_and_validate():
    d = DownloadClientConnectionSettings()
    assert d.auto_retry_enabled is True
    assert d.auto_retry_max_attempts == 6
    assert d.auto_retry_base_interval_minutes == 15

    with pytest.raises(msgspec.ValidationError, match="auto_retry_max_attempts"):
        DownloadClientConnectionSettings(auto_retry_max_attempts=21)
    with pytest.raises(
        msgspec.ValidationError, match="auto_retry_base_interval_minutes"
    ):
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


def test_quality_recipe_save_round_trips_with_legacy_mirrors(prefs):
    from models.acquisition_quality import QualityRecipeEntry

    recipe = [
        QualityRecipeEntry(format="flac", quality="cd"),
        QualityRecipeEntry(format="mp3", quality="320_plus"),
    ]
    prefs.save_download_policy(
        DownloadPolicySettings(quality_recipe=recipe, quality_preference_order=[])
    )

    stored = json.loads(prefs._config_path.read_text())
    section = stored["download_policy"]
    assert section["quality_recipe"] == [
        {
            "format": "flac",
            "quality": "cd",
            "min_bitrate_kbps": None,
            "target_bitrate_kbps": None,
            "max_bitrate_kbps": None,
            "bit_depth": None,
            "sample_rate_hz": None,
        },
        {
            "format": "mp3",
            "quality": "320_plus",
            "min_bitrate_kbps": 320,
            "target_bitrate_kbps": 320,
            "max_bitrate_kbps": None,
            "bit_depth": None,
            "sample_rate_hz": None,
        },
    ]
    assert section["quality_min"] == "mp3_320"
    assert section["quality_max"] == "lossless"
    assert section["quality_preference_order"] == ["lossless", "mp3_320"]
    assert prefs.get_free_music_settings().preferred_format == "flac"

    loaded = prefs.get_download_policy()
    assert loaded.quality_recipe_status == "v2"
    assert loaded.quality_recipe_error is None
    assert build_snapshot(loaded).schema_version == 2


def test_non_convertible_recipe_status_preserves_source_and_uses_v1_snapshot(prefs):
    from models.acquisition_quality import QualityRecipeEntry

    recipe = [QualityRecipeEntry(format="mp3", quality="320_plus")]
    prefs._save_config(
        {
            "download_policy": {
                "flac_mp3_only": False,
                "quality_recipe": [
                    {
                        "format": entry.format,
                        "quality": entry.quality,
                        "min_bitrate_kbps": entry.min_bitrate_kbps,
                        "target_bitrate_kbps": entry.target_bitrate_kbps,
                        "max_bitrate_kbps": entry.max_bitrate_kbps,
                        "bit_depth": entry.bit_depth,
                        "sample_rate_hz": entry.sample_rate_hz,
                    }
                    for entry in recipe
                ],
            }
        }
    )
    before = prefs._config_path.read_text()
    loaded = prefs.get_download_policy()
    assert loaded.quality_recipe_status == "non_convertible"
    assert loaded.quality_recipe_error
    assert loaded.quality_recipe
    assert build_snapshot(loaded).schema_version == 1
    assert prefs._config_path.read_text() == before


def test_invalid_stored_recipe_is_reported_without_healing_config(prefs):
    prefs._save_config(
        {
            "download_policy": {
                "quality_min": "mp3_192",
                "quality_max": "lossless",
                "quality_recipe": [{"format": "mp3", "quality": "not-a-quality"}],
            }
        }
    )
    before = prefs._config_path.read_text()
    loaded = prefs.get_download_policy()
    assert loaded.quality_recipe_status == "invalid"
    assert loaded.quality_recipe == []
    assert loaded.quality_recipe_error
    assert prefs._config_path.read_text() == before


def test_save_strips_whitespace_key_round_trip(prefs):
    # Issue #193: a pasted key saves stripped and reads back stripped. Assign
    # post-construction to isolate the save-layer strip from the schema strip.
    settings = DownloadClientConnectionSettings(
        url="http://slskd:5030", api_key="secret-key"
    )
    settings.api_key = "secret-key  \n"
    prefs.save_download_client_settings(settings)
    assert prefs.get_download_client_settings_raw().api_key == "secret-key"


def test_whitespace_only_key_clears_without_encrypting(prefs):
    # Stripped-empty must store "" (cleared), never encrypt whitespace.
    prefs.save_download_client_settings(
        DownloadClientConnectionSettings(url="http://slskd:5030", api_key="secret-key")
    )
    clearing = DownloadClientConnectionSettings(url="http://slskd:5030", api_key="")
    clearing.api_key = "   "
    prefs.save_download_client_settings(clearing)
    stored = json.loads(prefs._config_path.read_text())["download_client"]["api_key"]
    assert stored == ""
    assert prefs.get_download_client_settings_raw().api_key == ""


def test_mask_with_whitespace_still_preserves_existing_key(prefs):
    # Strip-then-compare at save: a padded sentinel preserves the stored key.
    prefs.save_download_client_settings(
        DownloadClientConnectionSettings(url="http://a:5030", api_key="secret-key")
    )
    resave = DownloadClientConnectionSettings(
        url="http://b:5030", api_key=DOWNLOAD_CLIENT_API_KEY_MASK
    )
    resave.api_key = f"{DOWNLOAD_CLIENT_API_KEY_MASK} "
    prefs.save_download_client_settings(resave)
    assert prefs.get_download_client_settings_raw().api_key == "secret-key"


def test_legacy_whitespace_key_stripped_on_raw_read(prefs):
    # A key saved before the fix (whitespace in ciphertext) authenticates.
    from infrastructure.crypto import encrypt

    prefs.save_download_client_settings(
        DownloadClientConnectionSettings(url="http://slskd:5030", api_key="secret-key")
    )
    config = json.loads(prefs._config_path.read_text())
    config["download_client"]["api_key"] = encrypt("  spaced-key  ")
    prefs._save_config(config)
    assert prefs.get_download_client_settings_raw().api_key == "spaced-key"


def test_schema_strips_api_key_but_keeps_mask_identity():
    assert (
        DownloadClientConnectionSettings(api_key="  k  ").api_key == "k"
    )
    assert (
        DownloadClientConnectionSettings(
            api_key=f"  {DOWNLOAD_CLIENT_API_KEY_MASK}  "
        ).api_key
        == DOWNLOAD_CLIENT_API_KEY_MASK
    )
