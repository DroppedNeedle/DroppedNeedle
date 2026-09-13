import json
from pathlib import Path

import msgspec
import pytest

from api.v1.schemas.library_management import (
    COMPLETE_LIBRARY_ORGANIZER_PROFILE_ID,
    DEFAULT_SIDECAR_PATTERNS,
    LEGACY_DEFAULT_SIDECAR_PATTERNS,
    LEGACY_NAMING_PROFILE_ID,
    LEGACY_NAMING_SCRIPT_ID,
    LibraryManagementSettings,
    PICARD_ORGANIZER_MULTI_DISC_NAMING_SCRIPT_ID,
    PICARD_ORGANIZER_NAMING_SCRIPT_ID,
    PICARD_ORGANIZER_PROFILE_ID,
    PICARD_ORGANIZER_V2_NAMING_SCRIPT_ID,
    PICARD_ORGANIZER_V3_MULTI_DISC_NAMING_SCRIPT_ID,
    PICARD_ORGANIZER_V3_MULTI_DISC_NAMING_SOURCE,
    PICARD_ORGANIZER_V3_NAMING_SCRIPT_ID,
    PICARD_ORGANIZER_V3_STANDARD_NAMING_SOURCE,
    NamingScriptSettings,
    OrganizationManagementSettings,
    build_initial_library_management_settings,
)
from api.v1.schemas.settings import ACOUSTID_KEY_MASK, DEFAULT_NAMING_TEMPLATE
from core.config import Settings
from core.exceptions import ConfigurationError, StaleRevisionError
from infrastructure.crypto import encrypt
import services.preferences_service as preferences_module
from services.preferences_service import PreferencesService


def _preferences(tmp_path: Path, payload: dict) -> PreferencesService:
    settings = Settings()
    settings.config_file_path = tmp_path / "config.json"
    settings.config_file_path.write_text(json.dumps(payload), encoding="utf-8")
    return PreferencesService(settings)


def test_management_migration_is_inert_idempotent_and_keeps_legacy_template(
    tmp_path: Path,
) -> None:
    template = "{albumartist}/{album}/{disc:02d}{track:02d} - {title}.{ext}"
    prefs = _preferences(
        tmp_path,
        {
            "library_settings": {
                "library_paths": [str(tmp_path / "Music")],
                "naming_template": template,
            }
        },
    )

    first = prefs.get_library_management_settings()
    first_text = prefs._config_path.read_text(encoding="utf-8")
    second = prefs.get_library_management_settings()

    legacy_script = next(
        value for value in first.naming_scripts if value.id == LEGACY_NAMING_SCRIPT_ID
    )
    legacy_profile = next(
        value for value in first.profiles if value.id == LEGACY_NAMING_PROFILE_ID
    )
    assert legacy_script.source == template
    assert legacy_profile.organization.naming_script_id == legacy_script.id
    assert first.root_assignments == []
    assert second.settings_revision == first.settings_revision
    assert prefs._config_path.read_text(encoding="utf-8") == first_text
    stored = json.loads(first_text)["library_management"]
    assert stored["root_assignments"] == []


def test_management_save_uses_revision_compare_and_swap(tmp_path: Path) -> None:
    prefs = _preferences(tmp_path, {})
    current = prefs.get_library_management_settings()
    update = prefs.get_library_management_settings_raw()
    update.undo_retention_days = 120
    update.recycle_bin_path = str(tmp_path / "Recycle")

    saved = prefs.save_library_management_settings_if_current(
        update,
        expected_settings_revision=current.settings_revision,
    )

    assert saved.undo_retention_days == 120
    assert saved.recycle_bin_path == str(tmp_path / "Recycle")
    assert saved.settings_revision != current.settings_revision
    with pytest.raises(StaleRevisionError):
        prefs.save_library_management_settings_if_current(
            update,
            expected_settings_revision=current.settings_revision,
        )


def test_management_save_round_trips_every_nested_group_without_activation(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path, {})
    current = prefs.get_library_management_settings()
    update = prefs.get_library_management_settings_raw()
    profile = update.profiles[0]
    profile.metadata.artist_credits.translate_names = True
    profile.metadata.artist_credits.preferred_locales = ["en-GB", "ja"]
    profile.genres.maximum_count = 12
    profile.artwork.external_format = "png"
    profile.organization.compatibility.maximum_path_length = 1024
    profile.file_behavior.preserve_permissions = False
    profile.enrichment.lyrics.write_synced = False
    profile.notification.refresh_external_servers = True
    update.external_refresh.plex_enabled = True

    prefs.save_library_management_settings_if_current(
        update,
        expected_settings_revision=current.settings_revision,
    )
    saved = prefs.get_library_management_settings_raw()
    saved_profile = next(value for value in saved.profiles if value.id == profile.id)

    assert saved_profile.metadata.artist_credits.preferred_locales == ["en-GB", "ja"]
    assert saved_profile.genres.maximum_count == 12
    assert saved_profile.artwork.external_format == "png"
    assert saved_profile.organization.compatibility.maximum_path_length == 1024
    assert saved_profile.file_behavior.preserve_permissions is False
    assert saved_profile.enrichment.lyrics.write_synced is False
    assert saved_profile.notification.refresh_external_servers is True
    assert saved.external_refresh.plex_enabled is True
    assert saved.root_assignments == []


def test_management_save_does_not_replace_masked_library_secret(tmp_path: Path) -> None:
    encrypted_key = encrypt("acoustid-secret")
    prefs = _preferences(
        tmp_path,
        {
            "library_settings": {
                "library_paths": [str(tmp_path / "Music")],
                "acoustid_api_key": encrypted_key,
            }
        },
    )
    assert prefs.get_typed_library_settings().acoustid_api_key == ACOUSTID_KEY_MASK
    current = prefs.get_library_management_settings()

    prefs.save_library_management_settings_if_current(
        prefs.get_library_management_settings_raw(),
        expected_settings_revision=current.settings_revision,
    )

    assert prefs.get_typed_library_settings_raw().acoustid_api_key == "acoustid-secret"


def test_invalid_stored_management_settings_fail_closed(tmp_path: Path) -> None:
    prefs = _preferences(
        tmp_path,
        {
            "library_management": {
                "schema_version": 999,
                "profiles": [],
                "default_profile_id": "",
            }
        },
    )

    with pytest.raises(ConfigurationError, match="invalid"):
        prefs.get_library_management_settings()


def test_invalid_stored_management_script_fails_closed(tmp_path: Path) -> None:
    prefs = _preferences(tmp_path, {})
    prefs.get_library_management_settings()
    payload = json.loads(prefs._config_path.read_text(encoding="utf-8"))
    payload["library_management"]["naming_scripts"][0]["source"] = (
        "{environment('HOME')}"
    )

    fresh = _preferences(tmp_path, payload)

    with pytest.raises(ConfigurationError, match="invalid"):
        fresh.get_library_management_settings()


def test_invalid_management_script_save_is_a_configuration_error(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path, {})
    current = prefs.get_library_management_settings()
    update = prefs.get_library_management_settings_raw()
    update.naming_scripts[0].source = "{__import__('os')}"

    with pytest.raises(ConfigurationError, match="Unknown safe function"):
        prefs.save_library_management_settings_if_current(
            update,
            expected_settings_revision=current.settings_revision,
        )


def test_recycle_bin_path_must_be_absolute(tmp_path: Path) -> None:
    prefs = _preferences(tmp_path, {})
    current = prefs.get_library_management_settings()
    update = prefs.get_library_management_settings_raw()
    update.recycle_bin_path = "relative/recycle"

    with pytest.raises(ConfigurationError, match="absolute path"):
        prefs.save_library_management_settings_if_current(
            update,
            expected_settings_revision=current.settings_revision,
        )


def test_raw_management_settings_are_a_detached_typed_copy(tmp_path: Path) -> None:
    prefs = _preferences(tmp_path, {})
    raw = prefs.get_library_management_settings_raw()

    assert isinstance(raw, LibraryManagementSettings)
    assert isinstance(
        msgspec.to_builtins(raw)["profiles"],
        list,
    )
    raw.profiles.clear()
    assert prefs.get_library_management_settings().profiles


def _legacy_picard_management_payload(preset_version: int) -> dict:
    settings = build_initial_library_management_settings()
    settings.preset_catalog_version = 0
    settings.profiles = [
        value
        for value in settings.profiles
        if value.id != COMPLETE_LIBRARY_ORGANIZER_PROFILE_ID
    ]
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    profile.preset_version = preset_version
    profile.organization = (
        OrganizationManagementSettings(
            naming_script_id=PICARD_ORGANIZER_V3_NAMING_SCRIPT_ID,
            multi_disc_naming_script_id=PICARD_ORGANIZER_V3_MULTI_DISC_NAMING_SCRIPT_ID,
        )
        if preset_version == 3
        else OrganizationManagementSettings(
            naming_script_id=PICARD_ORGANIZER_V2_NAMING_SCRIPT_ID
        )
    )
    if preset_version == 1:
        profile.enrichment.lyrics.write_synced = False
    settings.naming_scripts = [
        value
        for value in settings.naming_scripts
        if value.id
        not in {
            PICARD_ORGANIZER_NAMING_SCRIPT_ID,
            PICARD_ORGANIZER_MULTI_DISC_NAMING_SCRIPT_ID,
        }
    ]
    if preset_version == 3:
        settings.naming_scripts.extend(
            [
                NamingScriptSettings(
                    id=PICARD_ORGANIZER_V3_NAMING_SCRIPT_ID,
                    name="Picard-style single-disc folders",
                    source=PICARD_ORGANIZER_V3_STANDARD_NAMING_SOURCE,
                    preset_origin="picard_style_organizer",
                    preset_version=3,
                ),
                NamingScriptSettings(
                    id=PICARD_ORGANIZER_V3_MULTI_DISC_NAMING_SCRIPT_ID,
                    name="Picard-style multi-disc folders",
                    source=PICARD_ORGANIZER_V3_MULTI_DISC_NAMING_SOURCE,
                    preset_origin="picard_style_organizer",
                    preset_version=3,
                ),
            ]
        )
    else:
        settings.naming_scripts.append(
            NamingScriptSettings(
                id=PICARD_ORGANIZER_V2_NAMING_SCRIPT_ID,
                name="Picard-style folders",
                source=DEFAULT_NAMING_TEMPLATE,
                preset_origin="picard_style_organizer",
                preset_version=preset_version,
            )
        )
    return msgspec.to_builtins(settings)


@pytest.mark.parametrize("preset_version", [1, 2, 3])
def test_exact_legacy_preset_migration_is_locked_and_idempotent(
    tmp_path: Path,
    preset_version: int,
) -> None:
    payload = _legacy_picard_management_payload(preset_version)
    prefs = _preferences(tmp_path, {"library_management": payload})

    first = prefs.get_library_management_settings_raw()
    first_text = prefs._config_path.read_text(encoding="utf-8")
    second = prefs.get_library_management_settings_raw()
    profile = next(
        value for value in first.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )

    assert profile.preset_version == 4
    assert profile.organization.naming_script_id == PICARD_ORGANIZER_NAMING_SCRIPT_ID
    assert (
        profile.organization.multi_disc_naming_script_id
        == PICARD_ORGANIZER_MULTI_DISC_NAMING_SCRIPT_ID
    )
    if preset_version == 1:
        assert profile.enrichment.lyrics.write_synced is False
    assert prefs._config_path.read_text(encoding="utf-8") == first_text
    assert msgspec.to_builtins(second) == msgspec.to_builtins(first)


def test_preset_ratchet_runs_while_the_preferences_cache_lock_is_held(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    prefs = _preferences(
        tmp_path, {"library_management": _legacy_picard_management_payload(3)}
    )
    original = preferences_module.migrate_library_management_presets
    observed: list[bool] = []

    def tracking_migration(settings: LibraryManagementSettings):
        observed.append(prefs._cache_lock._is_owned())
        return original(settings)

    monkeypatch.setattr(
        preferences_module,
        "migrate_library_management_presets",
        tracking_migration,
    )

    prefs.get_library_management_settings_raw()

    assert observed == [True]


@pytest.mark.parametrize("preset_version", [1, 2, 3])
def test_legacy_preset_migration_preserves_customized_organization(
    tmp_path: Path,
    preset_version: int,
) -> None:
    payload = _legacy_picard_management_payload(preset_version)
    profile = next(
        value
        for value in payload["profiles"]
        if value["id"] == PICARD_ORGANIZER_PROFILE_ID
    )
    profile["organization"]["move_enabled"] = False
    prefs = _preferences(tmp_path, {"library_management": payload})

    migrated = prefs.get_library_management_settings_raw()
    preserved = next(
        value for value in migrated.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )

    assert preserved.preset_version == preset_version
    assert preserved.organization.move_enabled is False
    assert preserved.organization.naming_script_id == (
        PICARD_ORGANIZER_V3_NAMING_SCRIPT_ID
        if preset_version == 3
        else PICARD_ORGANIZER_V2_NAMING_SCRIPT_ID
    )
    assert {
        PICARD_ORGANIZER_NAMING_SCRIPT_ID,
        PICARD_ORGANIZER_MULTI_DISC_NAMING_SCRIPT_ID,
    } <= {value.id for value in migrated.naming_scripts}


@pytest.mark.parametrize("preset_version", [1, 2, 3])
def test_legacy_preset_migration_preserves_edited_script_and_custom_profile(
    tmp_path: Path,
    preset_version: int,
) -> None:
    payload = _legacy_picard_management_payload(preset_version)
    edited_script_id = (
        PICARD_ORGANIZER_V3_NAMING_SCRIPT_ID
        if preset_version == 3
        else PICARD_ORGANIZER_V2_NAMING_SCRIPT_ID
    )
    legacy_script = next(
        value for value in payload["naming_scripts"] if value["id"] == edited_script_id
    )
    legacy_script["source"] = "Custom/{track:02d} - {title}.{ext}"
    built_in = next(
        value
        for value in payload["profiles"]
        if value["id"] == PICARD_ORGANIZER_PROFILE_ID
    )
    copied = msgspec.json.decode(msgspec.json.encode(built_in))
    copied["id"] = "66666666-6666-4666-8666-666666666666"
    copied["name"] = "My copied organizer"
    copied["preset_origin"] = None
    copied["preset_version"] = None
    payload["profiles"].append(copied)
    prefs = _preferences(tmp_path, {"library_management": payload})

    migrated = prefs.get_library_management_settings_raw()
    profile = next(
        value for value in migrated.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    custom = next(value for value in migrated.profiles if value.id == copied["id"])
    preserved_script = next(
        value for value in migrated.naming_scripts if value.id == edited_script_id
    )

    assert profile.preset_version == preset_version
    assert profile.organization.naming_script_id == edited_script_id
    assert custom.organization.naming_script_id == edited_script_id
    assert preserved_script.source == "Custom/{track:02d} - {title}.{ext}"


def test_legacy_preset_migration_preserves_a_custom_profile_name(
    tmp_path: Path,
) -> None:
    payload = _legacy_picard_management_payload(3)
    profile = next(
        value
        for value in payload["profiles"]
        if value["id"] == PICARD_ORGANIZER_PROFILE_ID
    )
    profile["name"] = "My organizer"
    prefs = _preferences(tmp_path, {"library_management": payload})

    migrated = prefs.get_library_management_settings_raw()
    preserved = next(
        value for value in migrated.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )

    assert preserved.name == "My organizer"
    assert preserved.preset_version == 3
    assert (
        preserved.organization.naming_script_id == PICARD_ORGANIZER_V3_NAMING_SCRIPT_ID
    )


def test_legacy_preset_migration_preserves_a_renamed_script(tmp_path: Path) -> None:
    payload = _legacy_picard_management_payload(3)
    script = next(
        value
        for value in payload["naming_scripts"]
        if value["id"] == PICARD_ORGANIZER_V3_NAMING_SCRIPT_ID
    )
    script["name"] = "My single-disc script"
    prefs = _preferences(tmp_path, {"library_management": payload})

    migrated = prefs.get_library_management_settings_raw()
    profile = next(
        value for value in migrated.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )

    assert profile.preset_version == 3
    assert profile.organization.naming_script_id == PICARD_ORGANIZER_V3_NAMING_SCRIPT_ID
    assert (
        next(
            value
            for value in migrated.naming_scripts
            if value.id == PICARD_ORGANIZER_V3_NAMING_SCRIPT_ID
        ).name
        == "My single-disc script"
    )


def test_preset_name_collision_keeps_the_custom_script(tmp_path: Path) -> None:
    payload = _legacy_picard_management_payload(2)
    payload["naming_scripts"].append(
        {
            "id": "77777777-7777-4777-8777-777777777777",
            "name": "Picard-style: single disc",
            "source": "Mine/{title}.{ext}",
            "revision": "",
            "preset_origin": None,
            "preset_version": None,
        }
    )
    prefs = _preferences(tmp_path, {"library_management": payload})

    migrated = prefs.get_library_management_settings_raw()
    custom = next(
        value
        for value in migrated.naming_scripts
        if value.id == "77777777-7777-4777-8777-777777777777"
    )
    built_in = next(
        value
        for value in migrated.naming_scripts
        if value.id == PICARD_ORGANIZER_NAMING_SCRIPT_ID
    )

    assert custom.name == "Picard-style: single disc"
    assert custom.source == "Mine/{title}.{ext}"
    assert built_in.name == "Picard-style: single disc (built-in)"


def test_installed_v4_preset_script_can_be_edited_without_retriggering_migration(
    tmp_path: Path,
) -> None:
    prefs = _preferences(
        tmp_path, {"library_management": _legacy_picard_management_payload(2)}
    )
    migrated = prefs.get_library_management_settings_raw()
    current = prefs.get_library_management_settings()
    standard = next(
        value
        for value in migrated.naming_scripts
        if value.id == PICARD_ORGANIZER_NAMING_SCRIPT_ID
    )
    standard.source = "Edited/{track:02d} - {title}.{ext}"
    prefs.save_library_management_settings_if_current(
        migrated, expected_settings_revision=current.settings_revision
    )

    reloaded = prefs.get_library_management_settings_raw()
    edited = next(
        value
        for value in reloaded.naming_scripts
        if value.id == PICARD_ORGANIZER_NAMING_SCRIPT_ID
    )

    assert edited.source == "Edited/{track:02d} - {title}.{ext}"


def test_preset_stable_id_collision_fails_closed(tmp_path: Path) -> None:
    payload = _legacy_picard_management_payload(2)
    payload["naming_scripts"].append(
        {
            "id": PICARD_ORGANIZER_NAMING_SCRIPT_ID,
            "name": "Occupied",
            "source": "{title}.{ext}",
            "revision": "",
            "preset_origin": None,
            "preset_version": None,
        }
    )
    prefs = _preferences(tmp_path, {"library_management": payload})
    before = prefs._config_path.read_text(encoding="utf-8")

    with pytest.raises(ConfigurationError, match="invalid"):
        prefs.get_library_management_settings_raw()

    assert prefs._config_path.read_text(encoding="utf-8") == before


def test_picard_profile_stable_id_collision_fails_without_writing(
    tmp_path: Path,
) -> None:
    payload = _legacy_picard_management_payload(3)
    occupied = next(
        profile
        for profile in payload["profiles"]
        if profile["id"] == PICARD_ORGANIZER_PROFILE_ID
    )
    occupied["name"] = "Occupied Picard preset"
    occupied["preset_origin"] = None
    occupied["preset_version"] = None
    prefs = _preferences(tmp_path, {"library_management": payload})
    before = prefs._config_path.read_text(encoding="utf-8")

    with pytest.raises(ConfigurationError, match="invalid"):
        prefs.get_library_management_settings_raw()

    assert prefs._config_path.read_text(encoding="utf-8") == before


def test_complete_preset_is_installed_once_and_stays_deleted(tmp_path: Path) -> None:
    prefs = _preferences(
        tmp_path, {"library_management": _legacy_picard_management_payload(3)}
    )

    installed = prefs.get_library_management_settings_raw()
    assert installed.preset_catalog_version == 1
    assert (
        sum(
            profile.id == COMPLETE_LIBRARY_ORGANIZER_PROFILE_ID
            for profile in installed.profiles
        )
        == 1
    )

    current = prefs.get_library_management_settings()
    installed.profiles = [
        profile
        for profile in installed.profiles
        if profile.id != COMPLETE_LIBRARY_ORGANIZER_PROFILE_ID
    ]
    prefs.save_library_management_settings_if_current(
        installed, expected_settings_revision=current.settings_revision
    )

    reloaded = prefs.get_library_management_settings_raw()
    assert reloaded.preset_catalog_version == 1
    assert all(
        profile.id != COMPLETE_LIBRARY_ORGANIZER_PROFILE_ID
        for profile in reloaded.profiles
    )


def test_complete_preset_name_collision_is_deterministically_suffixed(
    tmp_path: Path,
) -> None:
    payload = _legacy_picard_management_payload(3)
    legacy = next(
        profile
        for profile in payload["profiles"]
        if profile["id"] == LEGACY_NAMING_PROFILE_ID
    )
    legacy["name"] = "Complete Library Organizer"
    prefs = _preferences(tmp_path, {"library_management": payload})

    migrated = prefs.get_library_management_settings_raw()
    complete = next(
        profile
        for profile in migrated.profiles
        if profile.id == COMPLETE_LIBRARY_ORGANIZER_PROFILE_ID
    )

    assert legacy["name"] == "Complete Library Organizer"
    assert complete.name == "Complete Library Organizer (built-in)"


def test_complete_preset_stable_id_collision_fails_without_writing(
    tmp_path: Path,
) -> None:
    payload = _legacy_picard_management_payload(3)
    occupied = dict(payload["profiles"][0])
    occupied["id"] = COMPLETE_LIBRARY_ORGANIZER_PROFILE_ID
    occupied["name"] = "Occupied complete preset"
    occupied["preset_origin"] = None
    occupied["preset_version"] = None
    payload["profiles"].append(occupied)
    prefs = _preferences(tmp_path, {"library_management": payload})
    before = prefs._config_path.read_text(encoding="utf-8")

    with pytest.raises(ConfigurationError, match="invalid"):
        prefs.get_library_management_settings_raw()

    assert prefs._config_path.read_text(encoding="utf-8") == before


def test_legacy_editor_options_normalize_to_current_live_settings(
    tmp_path: Path,
) -> None:
    payload = _legacy_picard_management_payload(3)
    profile = next(
        value
        for value in payload["profiles"]
        if value["id"] == PICARD_ORGANIZER_PROFILE_ID
    )
    profile["metadata"]["fields"][0]["mode"] = "preserve"
    profile["metadata"]["fields"][0]["clear_when_canonical_missing"] = True
    profile["metadata"]["artist_credits"]["standardization"] = "variations"
    profile["metadata"]["format_compatibility"]["constrained_genres_primary_only"] = (
        True
    )
    profile["genres"]["write_primary_only_for_constrained_formats"] = False
    profile["artwork"]["providers"].append("audiodb")
    profile["notification"]["refresh_droppedneedle"] = False
    prefs = _preferences(tmp_path, {"library_management": payload})

    normalized = prefs.get_library_management_settings_raw()
    current = next(
        value
        for value in normalized.profiles
        if value.id == PICARD_ORGANIZER_PROFILE_ID
    )

    assert current.metadata.fields[0].mode == "disabled"
    assert current.metadata.fields[0].clear_when_canonical_missing is False
    assert current.metadata.artist_credits.standardization == "credited"
    assert (
        current.metadata.format_compatibility.constrained_genres_primary_only is False
    )
    assert current.genres.write_primary_only_for_constrained_formats is True
    assert "audiodb" not in current.artwork.providers
    assert current.notification.refresh_droppedneedle is True


def _stored_profile(payload: dict, profile_id: str) -> dict:
    return next(value for value in payload["profiles"] if value["id"] == profile_id)


def test_stored_legacy_sidecar_defaults_migrate_to_back_booklet_medium(
    tmp_path: Path,
) -> None:
    payload = msgspec.to_builtins(build_initial_library_management_settings())
    for profile_id in (
        PICARD_ORGANIZER_PROFILE_ID,
        COMPLETE_LIBRARY_ORGANIZER_PROFILE_ID,
    ):
        _stored_profile(payload, profile_id)["organization"]["sidecar_patterns"] = list(
            LEGACY_DEFAULT_SIDECAR_PATTERNS
        )
    prefs = _preferences(tmp_path, {"library_management": payload})

    migrated = prefs.get_library_management_settings_raw()

    for profile_id in (
        PICARD_ORGANIZER_PROFILE_ID,
        COMPLETE_LIBRARY_ORGANIZER_PROFILE_ID,
    ):
        profile = next(value for value in migrated.profiles if value.id == profile_id)
        assert profile.organization.sidecar_patterns == list(DEFAULT_SIDECAR_PATTERNS)
    stored = json.loads(prefs._config_path.read_text(encoding="utf-8"))[
        "library_management"
    ]
    for profile_id in (
        PICARD_ORGANIZER_PROFILE_ID,
        COMPLETE_LIBRARY_ORGANIZER_PROFILE_ID,
    ):
        assert _stored_profile(stored, profile_id)["organization"][
            "sidecar_patterns"
        ] == list(DEFAULT_SIDECAR_PATTERNS)


def test_customized_sidecar_list_and_empty_legacy_list_are_untouched(
    tmp_path: Path,
) -> None:
    payload = msgspec.to_builtins(build_initial_library_management_settings())
    customized = [*LEGACY_DEFAULT_SIDECAR_PATTERNS, "*.txt"]
    _stored_profile(payload, PICARD_ORGANIZER_PROFILE_ID)["organization"][
        "sidecar_patterns"
    ] = list(customized)
    assert (
        _stored_profile(payload, LEGACY_NAMING_PROFILE_ID)["organization"][
            "sidecar_patterns"
        ]
        == []
    )
    prefs = _preferences(tmp_path, {"library_management": payload})

    migrated = prefs.get_library_management_settings_raw()

    picard = next(
        value for value in migrated.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    legacy = next(
        value for value in migrated.profiles if value.id == LEGACY_NAMING_PROFILE_ID
    )
    assert picard.organization.sidecar_patterns == customized
    assert legacy.organization.sidecar_patterns == []


def test_v3_payload_with_explicit_legacy_sidecars_still_migrates_to_v4(
    tmp_path: Path,
) -> None:
    payload = _legacy_picard_management_payload(3)
    _stored_profile(payload, PICARD_ORGANIZER_PROFILE_ID)["organization"][
        "sidecar_patterns"
    ] = list(LEGACY_DEFAULT_SIDECAR_PATTERNS)
    prefs = _preferences(tmp_path, {"library_management": payload})

    migrated = prefs.get_library_management_settings_raw()
    profile = next(
        value for value in migrated.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )

    assert profile.preset_version == 4
    assert profile.organization.sidecar_patterns == list(DEFAULT_SIDECAR_PATTERNS)
