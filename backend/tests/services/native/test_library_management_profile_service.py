import json
from pathlib import Path

import pytest

from api.v1.schemas.library_management import (
    LEGACY_NAMING_PROFILE_ID,
    PICARD_ORGANIZER_PROFILE_ID,
    LibraryManagementRootAssignment,
    profile_revision,
)
from core.config import Settings
from core.exceptions import ConfigurationError
from services.native.library_management_profile_service import (
    LibraryManagementProfileService,
)
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.preferences_service import PreferencesService


def _preferences(tmp_path: Path, *, available: bool = True) -> PreferencesService:
    tmp_path.mkdir(parents=True, exist_ok=True)
    root = tmp_path / "Music"
    if available:
        root.mkdir()
    settings = Settings()
    settings.config_file_path = tmp_path / "config.json"
    settings.config_file_path.write_text(
        json.dumps({"library_settings": {"library_paths": [str(root)]}}),
        encoding="utf-8",
    )
    return PreferencesService(settings)


def _service(
    prefs: PreferencesService,
    *,
    validate: bool = False,
) -> LibraryManagementProfileService:
    return LibraryManagementProfileService(
        prefs,
        activation_validator=(
            (lambda assignment: assignment.activation_preview_token == "verified")
            if validate
            else None
        ),
    )


def _activation_assignment(
    prefs: PreferencesService,
    *,
    settings_revision: str,
    profile_revision_value: str | None = None,
) -> LibraryManagementRootAssignment:
    root_id = prefs.get_typed_library_settings_raw().library_roots[0].id
    profile = next(
        value
        for value in prefs.get_library_management_settings_raw().profiles
        if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    policy_revision = LibraryPolicyResolver(
        prefs.get_typed_library_settings_raw()
    ).policy_revision
    return LibraryManagementRootAssignment(
        root_id=root_id,
        enabled=True,
        automatic_acquisitions=True,
        activation_profile_revision=(profile_revision_value or profile.revision),
        activation_policy_revision=policy_revision,
        activation_settings_revision=settings_revision,
        activation_preview_token="verified",
        activation_preview_hash="preview-hash",
        activation_confirmed_at=1.0,
    )


def _activate(
    service: LibraryManagementProfileService,
    prefs: PreferencesService,
) -> None:
    current = service.get_settings()
    proposed = prefs.get_library_management_settings_raw()
    proposed.root_assignments = [
        _activation_assignment(
            prefs,
            settings_revision=current.settings_revision,
        )
    ]
    service.save_settings(
        proposed,
        expected_settings_revision=current.settings_revision,
    )


def test_create_and_copy_profiles_are_independent(tmp_path: Path) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs)
    current = service.get_settings()

    copied = service.copy_profile(
        PICARD_ORGANIZER_PROFILE_ID,
        name="My organizer",
        expected_settings_revision=current.settings_revision,
    )
    saved = service.get_settings()
    copied.organization.move_enabled = False
    updated = service.update_profile(
        copied,
        expected_settings_revision=saved.settings_revision,
    )
    organizer = next(
        profile
        for profile in service.get_settings().profiles
        if profile.id == PICARD_ORGANIZER_PROFILE_ID
    )

    assert updated.id != organizer.id
    assert updated.preset_origin is None
    assert updated.organization.move_enabled is False
    assert organizer.organization.move_enabled is True


def test_assigned_profile_cannot_be_deleted(tmp_path: Path) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs)
    current = service.get_settings()
    proposed = prefs.get_library_management_settings_raw()
    proposed.root_assignments = [
        LibraryManagementRootAssignment(
            root_id=prefs.get_typed_library_settings_raw().library_roots[0].id,
            profile_id=LEGACY_NAMING_PROFILE_ID,
        )
    ]
    saved = service.save_settings(
        proposed,
        expected_settings_revision=current.settings_revision,
    )

    with pytest.raises(ConfigurationError, match="assigned"):
        service.delete_profile(
            LEGACY_NAMING_PROFILE_ID,
            expected_settings_revision=saved.settings_revision,
        )


def test_impact_distinguishes_harmless_restrictive_and_destructive(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs, validate=True)
    _activate(service, prefs)

    harmless = prefs.get_library_management_settings_raw()
    legacy = next(
        profile
        for profile in harmless.profiles
        if profile.id == LEGACY_NAMING_PROFILE_ID
    )
    legacy.description = "Cosmetic text"
    harmless_impact = service.preview_impact(harmless)

    restrictive = prefs.get_library_management_settings_raw()
    organizer = next(
        profile
        for profile in restrictive.profiles
        if profile.id == PICARD_ORGANIZER_PROFILE_ID
    )
    organizer.organization.move_enabled = False
    restrictive_impact = service.preview_impact(restrictive)

    destructive = prefs.get_library_management_settings_raw()
    organizer = next(
        profile
        for profile in destructive.profiles
        if profile.id == PICARD_ORGANIZER_PROFILE_ID
    )
    organizer.metadata.scrub_unmanaged_tags = True
    destructive_impact = service.preview_impact(destructive)

    assert harmless_impact.classification == "harmless"
    assert harmless_impact.preview_required is False
    assert restrictive_impact.classification == "restrictive"
    assert restrictive_impact.preview_required is False
    assert destructive_impact.classification == "destructive"
    assert destructive_impact.preview_required is True


def test_automatic_enablement_requires_bound_verified_activation(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs, validate=True)
    current = service.get_settings()
    proposed = prefs.get_library_management_settings_raw()
    proposed.root_assignments = [
        _activation_assignment(
            prefs,
            settings_revision=current.settings_revision,
            profile_revision_value="stale-profile",
        )
    ]

    with pytest.raises(ConfigurationError, match="dry run"):
        service.save_settings(
            proposed,
            expected_settings_revision=current.settings_revision,
        )

    profile = next(
        value for value in proposed.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    proposed.root_assignments[0].activation_profile_revision = profile_revision(profile)
    saved = service.save_settings(
        proposed,
        expected_settings_revision=current.settings_revision,
    )

    assert saved.root_assignments[0].automatic_acquisitions is True
    assert saved.root_assignments[0].automatic_drop_imports is False
    assert saved.root_assignments[0].automatic_scan_discovered is False


def test_no_validator_keeps_automatic_activation_inert(tmp_path: Path) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs)
    current = service.get_settings()
    proposed = prefs.get_library_management_settings_raw()
    proposed.root_assignments = [
        _activation_assignment(
            prefs,
            settings_revision=current.settings_revision,
        )
    ]

    with pytest.raises(ConfigurationError, match="dry run"):
        service.save_settings(
            proposed,
            expected_settings_revision=current.settings_revision,
        )
    assert service.get_settings().root_assignments == []


def test_broadened_active_profile_requires_a_fresh_revision_binding(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs, validate=True)
    _activate(service, prefs)
    current = service.get_settings()
    proposed = prefs.get_library_management_settings_raw()
    profile = next(
        value for value in proposed.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    profile.metadata.scrub_unmanaged_tags = True

    with pytest.raises(ConfigurationError, match="dry run"):
        service.save_settings(
            proposed,
            expected_settings_revision=current.settings_revision,
        )

    profile.revision = profile_revision(profile)
    assignment = proposed.root_assignments[0]
    assignment.activation_profile_revision = profile.revision
    assignment.activation_settings_revision = current.settings_revision
    assignment.activation_preview_token = "verified"
    assignment.activation_preview_hash = "fresh-hash"
    assignment.activation_confirmed_at = 2.0
    saved = service.save_settings(
        proposed,
        expected_settings_revision=current.settings_revision,
    )
    assert (
        next(
            value for value in saved.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
        ).metadata.scrub_unmanaged_tags
        is True
    )


def test_assignment_requires_a_known_available_root(tmp_path: Path) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs)
    current = service.get_settings()
    proposed = prefs.get_library_management_settings_raw()
    proposed.root_assignments = [
        LibraryManagementRootAssignment(root_id="missing-root")
    ]
    with pytest.raises(ConfigurationError, match="unknown root"):
        service.save_settings(
            proposed,
            expected_settings_revision=current.settings_revision,
        )

    unavailable_prefs = _preferences(tmp_path / "unavailable", available=False)
    unavailable_service = _service(unavailable_prefs, validate=True)
    unavailable_current = unavailable_service.get_settings()
    unavailable_proposed = unavailable_prefs.get_library_management_settings_raw()
    unavailable_proposed.root_assignments = [
        _activation_assignment(
            unavailable_prefs,
            settings_revision=unavailable_current.settings_revision,
        )
    ]
    with pytest.raises(ConfigurationError, match="not currently available"):
        unavailable_service.save_settings(
            unavailable_proposed,
            expected_settings_revision=unavailable_current.settings_revision,
        )


def test_picard_preset_diff_names_changed_groups(tmp_path: Path) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs)
    original = service.preset_diff(PICARD_ORGANIZER_PROFILE_ID)
    assert original.differs is False
    assert original.preset_profile is not None
    assert original.preset_profile.id == PICARD_ORGANIZER_PROFILE_ID
    current = service.get_settings()
    proposed = prefs.get_library_management_settings_raw()
    profile = next(
        value for value in proposed.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    profile.genres.maximum_count = 9
    service.update_profile(
        profile,
        expected_settings_revision=current.settings_revision,
    )

    diff = service.preset_diff(PICARD_ORGANIZER_PROFILE_ID)
    assert diff.differs is True
    assert diff.changed_groups == ["genres"]
    assert diff.preset_profile is not None
    assert diff.preset_profile.genres.maximum_count != 9


@pytest.mark.parametrize("location", ["inside", "parent", "same"])
def test_recycle_bin_cannot_overlap_library_root(tmp_path: Path, location: str) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs)
    current = service.get_settings()
    proposed = prefs.get_library_management_settings_raw()
    library_root = Path(prefs.get_typed_library_settings_raw().library_roots[0].path)
    proposed.recycle_bin_path = {
        "inside": library_root / ".recycle",
        "parent": library_root.parent,
        "same": library_root,
    }[location].as_posix()

    with pytest.raises(ConfigurationError, match="cannot overlap"):
        service.save_settings(
            proposed,
            expected_settings_revision=current.settings_revision,
        )
