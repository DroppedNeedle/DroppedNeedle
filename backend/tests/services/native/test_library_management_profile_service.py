import json
from pathlib import Path

import msgspec
import pytest

from api.v1.schemas.library_management import (
    COMPLETE_LIBRARY_ORGANIZER_PROFILE_ID,
    LEGACY_DEFAULT_SIDECAR_PATTERNS,
    LEGACY_NAMING_PROFILE_ID,
    PICARD_ORGANIZER_MULTI_DISC_NAMING_SCRIPT_ID,
    PICARD_ORGANIZER_NAMING_SCRIPT_ID,
    PICARD_ORGANIZER_PROFILE_ID,
    LibraryManagementSettings,
    LibraryManagementRootAssignment,
    LibraryManagementRootOverrides,
    NamingScriptSettings,
    profile_revision,
)
from core.config import Settings
from core.exceptions import ConfigurationError, StaleRevisionError
from models.library_management_planning import (
    naming_policy_revision,
    pin_library_management_profile,
)
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
    management_settings = prefs.get_library_management_settings_raw()
    profile = next(
        value
        for value in management_settings.profiles
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
        activation_naming_policy_revision=naming_policy_revision(
            pin_library_management_profile(management_settings, profile)
        ),
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
    copied = service.copy_profile(
        PICARD_ORGANIZER_PROFILE_ID,
        name="Assigned custom profile",
        expected_settings_revision=current.settings_revision,
    )
    current = service.get_settings()
    proposed = prefs.get_library_management_settings_raw()
    proposed.root_assignments = [
        LibraryManagementRootAssignment(
            root_id=prefs.get_typed_library_settings_raw().library_roots[0].id,
            profile_id=copied.id,
        )
    ]
    saved = service.save_settings(
        proposed,
        expected_settings_revision=current.settings_revision,
    )

    with pytest.raises(ConfigurationError, match="assigned"):
        service.delete_profile(
            copied.id,
            expected_settings_revision=saved.settings_revision,
        )


def test_full_settings_save_cannot_remove_or_relabel_a_present_preset(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs)
    before = service.get_settings()
    without_preset = prefs.get_library_management_settings_raw()
    without_preset.profiles = [
        profile
        for profile in without_preset.profiles
        if profile.id != COMPLETE_LIBRARY_ORGANIZER_PROFILE_ID
    ]

    with pytest.raises(ConfigurationError, match="presets cannot be deleted"):
        service.save_settings(
            without_preset,
            expected_settings_revision=before.settings_revision,
        )

    relabeled = prefs.get_library_management_settings_raw()
    profile = next(
        value for value in relabeled.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    profile.preset_origin = None
    with pytest.raises(ConfigurationError, match="preset identity cannot be changed"):
        service.update_profile(
            profile,
            expected_settings_revision=before.settings_revision,
        )

    assert service.get_settings() == before


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


@pytest.mark.parametrize(
    ("assignment_field", "trigger"),
    [
        ("automatic_drop_imports", "drop_import"),
        ("automatic_scan_discovered", "scan_discovered"),
    ],
)
def test_enabling_another_trigger_reuses_current_write_authorization(
    tmp_path: Path,
    assignment_field: str,
    trigger: str,
) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs, validate=True)
    _activate(service, prefs)
    current = service.get_settings()
    proposed = prefs.get_library_management_settings_raw()
    assignment = proposed.root_assignments[0]
    setattr(assignment, assignment_field, True)

    impact = service.preview_impact(
        proposed,
        expected_settings_revision=current.settings_revision,
    )
    saved = service.save_settings(
        proposed,
        expected_settings_revision=current.settings_revision,
    )
    prepared = service.prepare_automatic_profile(
        root_id=assignment.root_id,
        trigger=trigger,
        expected_policy_revision=assignment.activation_policy_revision or "",
    )

    assert impact.classification == "harmless"
    assert impact.preview_required is False
    assert impact.affected_root_ids == [assignment.root_id]
    assert "authorized write profile is unchanged" in impact.reasons[0]
    assert getattr(saved.root_assignments[0], assignment_field) is True
    assert saved.root_assignments[0].activation_preview_token == "verified"
    assert prepared is not None


def test_custom_edition_automation_requires_effective_fresh_activation(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs, validate=True)
    _activate(service, prefs)
    current = service.get_settings()
    dormant = prefs.get_library_management_settings_raw()
    dormant.root_assignments[0].automatic_custom_editions = True

    dormant_impact = service.preview_impact(
        dormant, expected_settings_revision=current.settings_revision
    )
    saved = service.save_settings(
        dormant, expected_settings_revision=current.settings_revision
    )

    assert dormant_impact.preview_required is False
    assert saved.root_assignments[0].automatic_custom_editions is False

    proposed = prefs.get_library_management_settings_raw()
    proposed.root_assignments[0].automatic_scan_discovered = True
    proposed.root_assignments[0].automatic_custom_editions = True
    impact = service.preview_impact(
        proposed, expected_settings_revision=saved.settings_revision
    )

    assert impact.classification == "destructive"
    assert impact.preview_required is True
    assert "Custom edition" in impact.reasons[0]


def test_enabling_scan_requires_fresh_activation_when_custom_automation_is_dormant(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs, validate=True)
    _activate(service, prefs)
    current = prefs.get_library_management_settings_raw()
    current.root_assignments[0].automatic_custom_editions = True
    proposed = msgspec.convert(
        msgspec.to_builtins(current), type=LibraryManagementSettings
    )
    proposed.root_assignments[0].automatic_scan_discovered = True

    impact = service._classify(current, proposed)

    assert impact.classification == "destructive"
    assert impact.preview_required is True
    assert any("Custom edition" in reason for reason in impact.reasons)


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


def test_root_standard_mode_follows_the_effective_standard_script(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs)
    settings = prefs.get_library_management_settings_raw()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    first_id = "88888888-8888-4888-8888-888888888888"
    second_id = "99999999-9999-4999-8999-999999999999"
    settings.naming_scripts.extend(
        [
            NamingScriptSettings(
                id=first_id, name="First standard", source="First/{title}.{ext}"
            ),
            NamingScriptSettings(
                id=second_id, name="Second standard", source="Second/{title}.{ext}"
            ),
        ]
    )
    profile.organization.naming_script_id = first_id
    assignment = LibraryManagementRootAssignment(
        root_id="root-1",
        profile_id=profile.id,
        overrides=LibraryManagementRootOverrides(multi_disc_naming_mode="standard"),
    )

    first = service._effective_profile(settings, assignment)
    profile.organization.naming_script_id = second_id
    changed_profile = service._effective_profile(settings, assignment)
    assert assignment.overrides is not None
    assignment.overrides.naming_script_id = first_id
    changed_override = service._effective_profile(settings, assignment)

    assert first.organization.naming_script_id == first_id
    assert changed_profile.organization.naming_script_id == second_id
    assert changed_override.organization.naming_script_id == first_id
    assert first.organization.multi_disc_naming_script_id is None
    assert changed_profile.organization.multi_disc_naming_script_id is None
    assert changed_override.organization.multi_disc_naming_script_id is None


@pytest.mark.parametrize(
    "script_id",
    [
        PICARD_ORGANIZER_NAMING_SCRIPT_ID,
        PICARD_ORGANIZER_MULTI_DISC_NAMING_SCRIPT_ID,
    ],
)
def test_editing_either_activated_naming_script_stales_automatic_work(
    tmp_path: Path, script_id: str
) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs, validate=True)
    _activate(service, prefs)
    current = service.get_settings()
    changed = prefs.get_library_management_settings_raw()
    script = next(value for value in changed.naming_scripts if value.id == script_id)
    script.source = f"Changed-{script_id[:4]}/{{title}}.{{ext}}"
    prefs.save_library_management_settings_if_current(
        changed, expected_settings_revision=current.settings_revision
    )
    assignment = prefs.get_library_management_settings_raw().root_assignments[0]

    with pytest.raises(StaleRevisionError, match="activation is stale"):
        service.prepare_automatic_profile(
            root_id=assignment.root_id,
            trigger="acquisition",
            expected_policy_revision=assignment.activation_policy_revision or "",
        )


def test_standard_only_activation_without_naming_evidence_remains_compatible(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs, validate=True)
    current = service.get_settings()
    settings = prefs.get_library_management_settings_raw()
    profile = next(
        value for value in settings.profiles if value.id == LEGACY_NAMING_PROFILE_ID
    )
    root_id = prefs.get_typed_library_settings_raw().library_roots[0].id
    policy_revision = LibraryPolicyResolver(
        prefs.get_typed_library_settings_raw()
    ).policy_revision
    settings.root_assignments = [
        LibraryManagementRootAssignment(
            root_id=root_id,
            profile_id=profile.id,
            enabled=True,
            automatic_acquisitions=True,
            activation_profile_revision=profile.revision,
            activation_naming_policy_revision=None,
            activation_policy_revision=policy_revision,
            activation_settings_revision=current.settings_revision,
            activation_preview_token="verified",
            activation_preview_hash="preview-hash",
            activation_confirmed_at=1.0,
        )
    ]
    service.save_settings(
        settings, expected_settings_revision=current.settings_revision
    )

    prepared = service.prepare_automatic_profile(
        root_id=root_id,
        trigger="acquisition",
        expected_policy_revision=policy_revision,
    )

    assert prepared is not None


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


def test_preset_diff_is_generalized_and_names_version_upgrade_groups(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs)

    complete = service.preset_diff(COMPLETE_LIBRARY_ORGANIZER_PROFILE_ID)
    assert complete.differs is False
    assert complete.preset_origin == "complete_library_organizer"
    assert complete.version_upgrade_groups == []
    assert complete.preset_profile is not None
    assert complete.preset_profile.enrichment.replaygain.mode == "replace"

    current = service.get_settings()
    proposed = prefs.get_library_management_settings_raw()
    picard = next(
        profile
        for profile in proposed.profiles
        if profile.id == PICARD_ORGANIZER_PROFILE_ID
    )
    picard.preset_version = 3
    picard.organization.move_enabled = False
    service.update_profile(picard, expected_settings_revision=current.settings_revision)

    older = service.preset_diff(PICARD_ORGANIZER_PROFILE_ID)
    assert older.changed_groups == ["organization"]
    assert older.version_upgrade_groups == ["organization"]


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


def test_activation_health_empty_for_fresh_activation(tmp_path: Path) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs, validate=True)
    _activate(service, prefs)

    assert service.activation_health().stale_root_ids == []


def test_activation_health_flags_profile_changed_after_activation(
    tmp_path: Path,
) -> None:
    """A migration-style rewrite (saved outside the gate, like #401) surfaces.

    The profile change must flag the root in health AND hold the automatic
    gate - the two share one predicate and must never drift.
    """

    prefs = _preferences(tmp_path)
    service = _service(prefs, validate=True)
    _activate(service, prefs)
    root_id = prefs.get_typed_library_settings_raw().library_roots[0].id

    current = service.get_settings()
    rewritten = prefs.get_library_management_settings_raw()
    organizer = next(
        profile
        for profile in rewritten.profiles
        if profile.id == PICARD_ORGANIZER_PROFILE_ID
    )
    organizer.organization.sidecar_patterns = [
        *organizer.organization.sidecar_patterns,
        "artwork*.jpg",
    ]
    prefs.save_library_management_settings_if_current(
        rewritten,
        expected_settings_revision=current.settings_revision,
    )
    policy_revision = LibraryPolicyResolver(
        prefs.get_typed_library_settings_raw()
    ).policy_revision

    assert service.activation_health().stale_root_ids == [root_id]
    with pytest.raises(StaleRevisionError, match="activation is stale"):
        service.prepare_automatic_profile(
            root_id=root_id,
            trigger="acquisition",
            expected_policy_revision=policy_revision,
        )


def test_activation_health_ignores_inactive_assignments(tmp_path: Path) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs, validate=True)
    root_id = prefs.get_typed_library_settings_raw().library_roots[0].id
    current = service.get_settings()
    proposed = prefs.get_library_management_settings_raw()
    proposed.root_assignments = [
        LibraryManagementRootAssignment(root_id=root_id, enabled=True)
    ]
    service.save_settings(
        proposed,
        expected_settings_revision=current.settings_revision,
    )

    assert service.activation_health().stale_root_ids == []


def test_activation_health_flags_never_activated_assignment(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs, validate=True)
    root_id = prefs.get_typed_library_settings_raw().library_roots[0].id
    current = service.get_settings()
    proposed = prefs.get_library_management_settings_raw()
    proposed.root_assignments = [
        LibraryManagementRootAssignment(
            root_id=root_id,
            enabled=True,
            automatic_acquisitions=True,
        )
    ]
    prefs.save_library_management_settings_if_current(
        proposed,
        expected_settings_revision=current.settings_revision,
    )

    assert service.activation_health().stale_root_ids == [root_id]


def test_activation_health_blocks_every_active_root_when_policy_unresolvable(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path, available=False)
    service = _service(prefs, validate=True)
    root_id = prefs.get_typed_library_settings_raw().library_roots[0].id
    current = service.get_settings()
    proposed = prefs.get_library_management_settings_raw()
    proposed.root_assignments = [
        LibraryManagementRootAssignment(
            root_id=root_id,
            enabled=True,
            automatic_acquisitions=True,
        )
    ]
    prefs.save_library_management_settings_if_current(
        proposed,
        expected_settings_revision=current.settings_revision,
    )
    policy_revision = LibraryPolicyResolver(
        prefs.get_typed_library_settings_raw()
    ).policy_revision

    health = service.activation_health()
    assert health.stale_root_ids == []
    assert health.blocked_root_ids == [root_id]
    assert health.blocked_reason is not None
    assert "not currently available" in health.blocked_reason
    with pytest.raises(ConfigurationError, match="not currently available"):
        service.prepare_automatic_profile(
            root_id=root_id,
            trigger="acquisition",
            expected_policy_revision=policy_revision,
        )


def _activate_over_legacy_sidecars(
    service: LibraryManagementProfileService, prefs: PreferencesService
) -> str:
    """Forge the exact post-#401 state: new sidecars stored, legacy pin kept.

    A pre-#401 activation pinned the legacy-sidecar hash, and the #401
    load migration then rewrote the stored profiles without touching the
    pin. Post-#401 code can no longer create that state through normal
    reads (every load migrates first), so the pin is rewound directly to
    what the same effective profile hashed to with legacy sidecars - a
    construction proven against live data (it reproduces the real held
    user's activation hash exactly).
    """

    _activate(service, prefs)
    root_id = prefs.get_typed_library_settings_raw().library_roots[0].id
    current = service.get_settings()
    raw = prefs.get_library_management_settings_raw()
    assignment = next(
        value for value in raw.root_assignments if value.root_id == root_id
    )
    effective = LibraryManagementProfileService._effective_profile(raw, assignment)
    legacy_organization = msgspec.structs.replace(
        effective.organization,
        sidecar_patterns=list(LEGACY_DEFAULT_SIDECAR_PATTERNS),
    )
    legacy_effective = msgspec.structs.replace(
        effective, organization=legacy_organization
    )
    assignment.activation_profile_revision = profile_revision(legacy_effective)
    assert assignment.activation_profile_revision != profile_revision(effective)
    prefs.save_library_management_settings_if_current(
        raw, expected_settings_revision=current.settings_revision
    )
    return root_id


def _policy_revision_for(prefs: PreferencesService) -> str:
    return LibraryPolicyResolver(
        prefs.get_typed_library_settings_raw()
    ).policy_revision


def test_automatic_gate_carries_activation_across_default_migration(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs, validate=True)
    root_id = _activate_over_legacy_sidecars(service, prefs)

    result = service.prepare_automatic_profile(
        root_id=root_id,
        trigger="acquisition",
        expected_policy_revision=_policy_revision_for(prefs),
    )

    assert result is not None
    # The carry is read-only: pins converge on the next user-confirmed dry
    # run instead of being rewritten mid-import (which would churn the
    # settings revision the in-flight bundle sealed).
    carried = prefs.get_library_management_settings_raw()
    assignment = next(
        value for value in carried.root_assignments if value.root_id == root_id
    )
    effective = LibraryManagementProfileService._effective_profile(
        carried, assignment
    )
    assert assignment.activation_profile_revision != profile_revision(effective)


def test_activation_health_current_across_default_migration(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs, validate=True)
    _activate_over_legacy_sidecars(service, prefs)

    health = service.activation_health()

    assert health.stale_root_ids == []
    assert health.blocked_root_ids == []


def test_automatic_gate_still_holds_migration_plus_real_change(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs, validate=True)
    root_id = _activate_over_legacy_sidecars(service, prefs)
    current = service.get_settings()
    edited = prefs.get_library_management_settings_raw()
    organizer = next(
        profile
        for profile in edited.profiles
        if profile.id == PICARD_ORGANIZER_PROFILE_ID
    )
    organizer.organization.rename_enabled = (
        not organizer.organization.rename_enabled
    )
    prefs.save_library_management_settings_if_current(
        edited, expected_settings_revision=current.settings_revision
    )

    _assert_migration_plus_change_holds(service, prefs, root_id)


def _assert_migration_plus_change_holds(
    service: LibraryManagementProfileService,
    prefs: PreferencesService,
    root_id: str,
) -> None:
    with pytest.raises(StaleRevisionError, match="activation is stale"):
        service.prepare_automatic_profile(
            root_id=root_id,
            trigger="acquisition",
            expected_policy_revision=_policy_revision_for(prefs),
        )
    assert service.activation_health().stale_root_ids == [root_id]


def test_automatic_gate_still_holds_migration_plus_naming_change(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs, validate=True)
    root_id = _activate_over_legacy_sidecars(service, prefs)
    current = service.get_settings()
    edited = prefs.get_library_management_settings_raw()
    organizer = next(
        profile
        for profile in edited.profiles
        if profile.id == PICARD_ORGANIZER_PROFILE_ID
    )
    organizer.organization.naming_script_id = (
        PICARD_ORGANIZER_MULTI_DISC_NAMING_SCRIPT_ID
    )
    prefs.save_library_management_settings_if_current(
        edited, expected_settings_revision=current.settings_revision
    )

    _assert_migration_plus_change_holds(service, prefs, root_id)


def test_automatic_gate_still_holds_migration_plus_cleared_proof(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs, validate=True)
    root_id = _activate_over_legacy_sidecars(service, prefs)
    current = service.get_settings()
    edited = prefs.get_library_management_settings_raw()
    assignment = next(
        value for value in edited.root_assignments if value.root_id == root_id
    )
    assignment.activation_preview_token = None
    prefs.save_library_management_settings_if_current(
        edited, expected_settings_revision=current.settings_revision
    )

    _assert_migration_plus_change_holds(service, prefs, root_id)


def test_automatic_gate_still_holds_migration_plus_overrides(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs, validate=True)
    root_id = _activate_over_legacy_sidecars(service, prefs)
    current = service.get_settings()
    edited = prefs.get_library_management_settings_raw()
    organizer = next(
        profile
        for profile in edited.profiles
        if profile.id == PICARD_ORGANIZER_PROFILE_ID
    )
    assignment = next(
        value for value in edited.root_assignments if value.root_id == root_id
    )
    assignment.overrides = LibraryManagementRootOverrides(
        move_enabled=not organizer.organization.move_enabled
    )
    prefs.save_library_management_settings_if_current(
        edited, expected_settings_revision=current.settings_revision
    )

    _assert_migration_plus_change_holds(service, prefs, root_id)


def test_automatic_gate_still_holds_migration_plus_policy_drift(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs, validate=True)
    root_id = _activate_over_legacy_sidecars(service, prefs)
    current = service.get_settings()
    edited = prefs.get_library_management_settings_raw()
    assignment = next(
        value for value in edited.root_assignments if value.root_id == root_id
    )
    assignment.activation_policy_revision = "bogus-policy-revision"
    prefs.save_library_management_settings_if_current(
        edited, expected_settings_revision=current.settings_revision
    )

    _assert_migration_plus_change_holds(service, prefs, root_id)


def test_activation_health_omits_reason_when_nothing_is_blocked(
    tmp_path: Path,
) -> None:
    prefs = _preferences(tmp_path)
    service = _service(prefs, validate=True)
    library_root = prefs.get_typed_library_settings_raw().library_roots[0].path
    current = service.get_settings()
    proposed = prefs.get_library_management_settings_raw()
    proposed.recycle_bin_path = library_root
    prefs.save_library_management_settings_if_current(
        proposed,
        expected_settings_revision=current.settings_revision,
    )

    health = service.activation_health()

    assert health.stale_root_ids == []
    assert health.blocked_root_ids == []
    assert health.blocked_reason is None


def test_activation_health_survives_one_broken_assignment(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    first = tmp_path / "Music"
    second = tmp_path / "Vinyl"
    first.mkdir()
    second.mkdir()
    settings = Settings()
    settings.config_file_path = tmp_path / "config.json"
    settings.config_file_path.write_text(
        json.dumps({"library_settings": {"library_paths": [str(first), str(second)]}}),
        encoding="utf-8",
    )
    prefs = PreferencesService(settings)
    service = LibraryManagementProfileService(prefs)
    roots = prefs.get_typed_library_settings_raw().library_roots
    current = service.get_settings()
    healthy = _activation_assignment(
        prefs,
        settings_revision=current.settings_revision,
    )
    other = _activation_assignment(
        prefs,
        settings_revision=current.settings_revision,
    )
    other.root_id = roots[1].id
    proposed = prefs.get_library_management_settings_raw()
    proposed.root_assignments = [healthy, other]
    prefs.save_library_management_settings_if_current(
        proposed,
        expected_settings_revision=current.settings_revision,
    )

    # A profile that vanishes between save and read (the bare StopIteration
    # `_effective_profile` raises) must stain only its own root - as blocked,
    # since a dry run would re-validate the same settings and fail too.
    original = LibraryManagementProfileService._effective_profile

    def flaky_effective(settings_value, assignment):  # noqa: ANN001, ANN202
        if assignment.root_id == roots[1].id:
            raise StopIteration("missing-profile")
        return original(settings_value, assignment)

    monkeypatch.setattr(
        LibraryManagementProfileService,
        "_effective_profile",
        staticmethod(flaky_effective),
    )

    health = service.activation_health()
    assert health.stale_root_ids == []
    assert health.blocked_root_ids == [roots[1].id]
    assert health.blocked_reason is None
    policy_revision = LibraryPolicyResolver(
        prefs.get_typed_library_settings_raw()
    ).policy_revision
    # Gate parity: the automatic path hits the same unmapped StopIteration
    # for this root (unreachable via stored settings, which normalize
    # forbids) while health degrades to blocked instead of failing.
    with pytest.raises(StopIteration):
        service.prepare_automatic_profile(
            root_id=roots[1].id,
            trigger="acquisition",
            expected_policy_revision=policy_revision,
        )
