"""Inert profile CRUD, assignment validation, and activation-impact policy."""

from __future__ import annotations

import copy
import os
import uuid
from collections.abc import Callable
from pathlib import Path

import msgspec

from api.v1.schemas.library_management import (
    DEFAULT_SIDECAR_PATTERNS,
    LEGACY_DEFAULT_SIDECAR_PATTERNS,
    LibraryManagementActivationHealthResponse,
    LibraryManagementChangeImpact,
    ManagedFieldSettings,
    LibraryManagementPresetDiff,
    LibraryManagementProfile,
    LibraryManagementRootAssignment,
    LibraryManagementRootOverrides,
    LibraryManagementSettings,
    LibraryManagementSettingsResponse,
    normalize_library_management_settings,
    preset_profile_for_origin,
    profile_revision,
    settings_revision,
)
from api.v1.schemas.library_management_sharing import (
    LibraryManagementProfileExportResponse,
    LibraryManagementProfileImportPreviewResponse,
    LibraryManagementProfileImportResponse,
)
from core.exceptions import (
    ConfigurationError,
    ScriptValidationError,
    StaleRevisionError,
)
from models.library_management_planning import (
    PinnedLibraryManagementProfile,
    naming_policy_revision,
    pin_library_management_profile,
)
from services.native.library_management_naming_policy import (
    activation_naming_policy_matches,
)
from services.native.library_management_profile_sharing import (
    PROFILE_BUNDLE_MIME_TYPE,
    MaterializedProfileBundle,
    export_profile_bundle,
    materialize_profile_bundle,
    parse_profile_bundle,
    preview_materialized_profile,
    profile_aspects,
    profile_bundle_filename,
    profile_import_warnings,
    unique_import_name,
)
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.preferences_service import PreferencesService

ActivationValidator = Callable[[LibraryManagementRootAssignment], bool]

_FIELD_MODE_RANK = {
    "disabled": 0,
    "preserve": 0,
    "fill_missing": 1,
    "merge": 2,
    "replace": 3,
}
_GENRE_MODE_RANK = {"fill_missing": 0, "merge": 1, "replace": 2}


def _active_automatic(assignment: LibraryManagementRootAssignment | None) -> bool:
    return bool(
        assignment is not None
        and assignment.enabled
        and (
            assignment.automatic_acquisitions
            or assignment.automatic_drop_imports
            or assignment.automatic_scan_discovered
        )
    )


def _activation_is_current(
    assignment: LibraryManagementRootAssignment,
    effective: LibraryManagementProfile,
    pinned: PinnedLibraryManagementProfile,
    policy: LibraryPolicyResolver,
) -> bool:
    """The exact gate `prepare_automatic_profile` enforces, as a predicate.

    Shared with `activation_health` so the automation tab reports precisely
    the roots the automatic path would hold - the two must never drift.
    """

    return bool(
        assignment.activation_profile_revision == profile_revision(effective)
        and activation_naming_policy_matches(assignment, pinned)
        and assignment.activation_policy_revision == policy.policy_revision
        and assignment.activation_preview_token
        and assignment.activation_preview_hash
        and assignment.activation_confirmed_at is not None
    )


# Historical project-default sidecar patterns, oldest first. Load-time preset
# migrations rewrite untouched profiles to the current defaults; an activation
# that reviewed exactly "this effective profile with historical defaults" is
# still valid, so the automatic gate carries it forward instead of demanding a
# dry run for a change the user never made (owner decision, 2026-09-13: a
# default-only migration must never pause the organizer). Entries must be
# kept: activations pinned by old releases verify against them, and pins only
# converge to current hashes on the next user-confirmed dry run. Future
# default migrations append a copy of their superseded list here alongside
# the migration (copies, so a later mutation of the live constant cannot
# corrupt historical pins).
_SIDECAR_DEFAULT_HISTORY: tuple[list[str], ...] = (
    list(LEGACY_DEFAULT_SIDECAR_PATTERNS),
)


def migration_carry_applies(
    assignment: LibraryManagementRootAssignment,
    effective: LibraryManagementProfile,
    pinned: PinnedLibraryManagementProfile,
    policy: LibraryPolicyResolver,
) -> bool:
    """Whether a default-only migration is the sole activation drift.

    Returns True only when the stored profile carries the current project
    defaults and the activation is current for that same effective profile
    with a historical default list substituted back in - i.e. the reviewed
    state plus a project-blessed default evolution, the same class of
    change the organizer engine itself undergoes without invalidating dry
    runs. Any real user delta, naming/policy drift, or missing proof still
    needs a fresh dry run. Read-only: pins converge on the next confirmed
    dry run rather than being rewritten here.
    """

    if effective.organization.sidecar_patterns != DEFAULT_SIDECAR_PATTERNS:
        return False
    for historical in _SIDECAR_DEFAULT_HISTORY:
        pre_migration = msgspec.structs.replace(
            effective,
            organization=msgspec.structs.replace(
                effective.organization, sidecar_patterns=list(historical)
            ),
        )
        if _activation_is_current(assignment, pre_migration, pinned, policy):
            return True
    return False


def _profile_scope_payload(profile: LibraryManagementProfile) -> dict:
    payload = msgspec.to_builtins(profile)
    for field in (
        "id",
        "name",
        "description",
        "preset_origin",
        "preset_version",
        "revision",
        "notification",
    ):
        payload.pop(field, None)
    return payload


def _ordered_subset(candidate: list, original: list) -> bool:
    candidate_set = set(candidate)
    return candidate == [value for value in original if value in candidate_set]


def _reset_safe_boolean(
    candidate: dict,
    old: dict,
    new: dict,
    path: tuple[str, ...],
    *,
    safe_from: bool,
    safe_to: bool,
) -> None:
    old_node = old
    new_node = new
    candidate_node = candidate
    for key in path[:-1]:
        old_node = old_node[key]
        new_node = new_node[key]
        candidate_node = candidate_node[key]
    key = path[-1]
    if old_node[key] is safe_from and new_node[key] is safe_to:
        candidate_node[key] = old_node[key]


def _is_restrictive_profile_change(
    old_profile: LibraryManagementProfile,
    new_profile: LibraryManagementProfile,
) -> bool:
    old = _profile_scope_payload(old_profile)
    new = _profile_scope_payload(new_profile)
    if old == new:
        return False
    candidate = copy.deepcopy(new)

    for path in (
        ("metadata", "enabled"),
        ("metadata", "relationships", "enabled"),
        ("genres", "enabled"),
        ("artwork", "embedded_enabled"),
        ("artwork", "external_enabled"),
        ("organization", "rename_enabled"),
        ("organization", "move_enabled"),
        ("organization", "move_sidecars"),
        ("organization", "remove_empty_directories"),
        ("enrichment", "lyrics", "enabled"),
        ("enrichment", "replaygain", "enabled"),
        (
            # D-EDITION-AUTO S-3: disabling automatic acceptance only
            # narrows automatic scope; enabling it stays destructive.
            "identity",
            "automatic_edition_acceptance_enabled",
        ),
    ):
        _reset_safe_boolean(candidate, old, new, path, safe_from=True, safe_to=False)
    for path in (
        ("metadata", "preserve_embedded_art_during_scrub"),
        ("genres", "listenbrainz_curated_only"),
        ("genres", "lastfm_whitelist_only"),
        ("genres", "write_primary_only_for_constrained_formats"),
        ("artwork", "approved_only"),
        ("artwork", "embedded_front_only"),
        ("artwork", "external_front_only"),
        ("artwork", "never_replace_with_smaller"),
        ("file_behavior", "preserve_timestamps"),
        ("file_behavior", "preserve_permissions"),
        ("file_behavior", "strict_capability_gate"),
        ("file_behavior", "validate_written_metadata"),
        ("file_behavior", "validate_technical_audio"),
    ):
        _reset_safe_boolean(candidate, old, new, path, safe_from=False, safe_to=True)
    _reset_safe_boolean(
        candidate,
        old,
        new,
        ("metadata", "scrub_unmanaged_tags"),
        safe_from=True,
        safe_to=False,
    )
    _reset_safe_boolean(
        candidate,
        old,
        new,
        ("artwork", "overwrite_external_files"),
        safe_from=True,
        safe_to=False,
    )

    old_fields = {value["field"]: value for value in old["metadata"]["fields"]}
    new_fields = {value["field"]: value for value in new["metadata"]["fields"]}
    if set(new_fields).issubset(old_fields) and all(
        _FIELD_MODE_RANK[value["mode"]] <= _FIELD_MODE_RANK[old_fields[field]["mode"]]
        and not (
            value["clear_when_canonical_missing"]
            and not old_fields[field]["clear_when_canonical_missing"]
        )
        for field, value in new_fields.items()
    ):
        candidate["metadata"]["fields"] = old["metadata"]["fields"]

    old_preserved = old["metadata"]["preserve_fields"]
    new_preserved = new["metadata"]["preserve_fields"]
    if set(new_preserved).issuperset(old_preserved):
        candidate["metadata"]["preserve_fields"] = old_preserved

    old_relationships = old["metadata"]["relationships"]["types"]
    new_relationships = new["metadata"]["relationships"]["types"]
    if _ordered_subset(new_relationships, old_relationships):
        candidate["metadata"]["relationships"]["types"] = old_relationships

    old_genres = old["genres"]
    new_genres = new["genres"]
    candidate_genres = candidate["genres"]
    if _GENRE_MODE_RANK[new_genres["mode"]] <= _GENRE_MODE_RANK[old_genres["mode"]]:
        candidate_genres["mode"] = old_genres["mode"]
    if _ordered_subset(new_genres["sources"], old_genres["sources"]):
        candidate_genres["sources"] = old_genres["sources"]
    if new_genres["maximum_count"] <= old_genres["maximum_count"]:
        candidate_genres["maximum_count"] = old_genres["maximum_count"]
    for threshold in (
        "musicbrainz_minimum_count",
        "listenbrainz_minimum_count",
        "lastfm_minimum_weight",
    ):
        if new_genres[threshold] >= old_genres[threshold]:
            candidate_genres[threshold] = old_genres[threshold]

    old_artwork = old["artwork"]
    new_artwork = new["artwork"]
    candidate_artwork = candidate["artwork"]
    for field in ("providers", "image_types"):
        if _ordered_subset(new_artwork[field], old_artwork[field]):
            candidate_artwork[field] = old_artwork[field]
    if set(new_artwork["preserve_existing_types"]).issuperset(
        old_artwork["preserve_existing_types"]
    ):
        candidate_artwork["preserve_existing_types"] = old_artwork[
            "preserve_existing_types"
        ]
    for field in ("minimum_width", "minimum_height"):
        if new_artwork[field] >= old_artwork[field]:
            candidate_artwork[field] = old_artwork[field]

    old_organization = old["organization"]
    new_organization = new["organization"]
    if _ordered_subset(
        new_organization["sidecar_patterns"], old_organization["sidecar_patterns"]
    ):
        candidate["organization"]["sidecar_patterns"] = old_organization[
            "sidecar_patterns"
        ]
    if (
        old_organization["source_cleanup"] == "remove_after_confirmed_move"
        and new_organization["source_cleanup"] == "keep"
    ):
        candidate["organization"]["source_cleanup"] = old_organization["source_cleanup"]

    return candidate == old


class LibraryManagementProfileService:
    def __init__(
        self,
        preferences: PreferencesService,
        *,
        activation_validator: ActivationValidator | None = None,
    ) -> None:
        self._preferences = preferences
        self._activation_validator = activation_validator

    def get_settings(self) -> LibraryManagementSettingsResponse:
        return self._preferences.get_library_management_settings()

    def automatic_edition_acceptance_enabled(self, root_id: str) -> bool:
        """D-EDITION-AUTO S-3: effective profile-level opt-in for one root.

        Roots inherit their assigned (or the default) profile's flag;
        a per-root override wins when set. Purely a settings read - it
        neither schedules work nor mutates anything.
        """
        settings = self._preferences.get_library_management_settings_raw()
        assignment = next(
            (
                value
                for value in settings.root_assignments
                if value.root_id == root_id
            ),
            None,
        )
        effective = self._effective_profile(
            settings,
            assignment or LibraryManagementRootAssignment(root_id=root_id),
        )
        return bool(effective.identity.automatic_edition_acceptance_enabled)

    def get_profile(self, profile_id: str) -> LibraryManagementProfile:
        return self._find_profile(self.get_settings(), profile_id)

    def create_profile(
        self,
        *,
        name: str,
        description: str = "",
        expected_settings_revision: str,
    ) -> LibraryManagementProfile:
        settings = self._preferences.get_library_management_settings_raw()
        source = next(
            profile
            for profile in settings.profiles
            if profile.id == settings.default_profile_id
        )
        return self._copy_profile(
            settings,
            source,
            name=name,
            description=description,
            expected_settings_revision=expected_settings_revision,
        )

    def copy_profile(
        self,
        profile_id: str,
        *,
        name: str,
        expected_settings_revision: str,
    ) -> LibraryManagementProfile:
        settings = self._preferences.get_library_management_settings_raw()
        source = self._find_profile(settings, profile_id)
        return self._copy_profile(
            settings,
            source,
            name=name,
            description=source.description,
            expected_settings_revision=expected_settings_revision,
        )

    def _copy_profile(
        self,
        settings: LibraryManagementSettings,
        source: LibraryManagementProfile,
        *,
        name: str,
        description: str,
        expected_settings_revision: str,
    ) -> LibraryManagementProfile:
        copied = msgspec.convert(
            msgspec.to_builtins(source), type=LibraryManagementProfile
        )
        copied.id = str(uuid.uuid4())
        copied.name = name
        copied.description = description
        copied.preset_origin = None
        copied.preset_version = None
        copied.revision = ""
        settings.profiles.append(copied)
        saved = self.save_settings(
            settings, expected_settings_revision=expected_settings_revision
        )
        return self._find_profile(saved, copied.id)

    def update_profile(
        self,
        profile: LibraryManagementProfile,
        *,
        expected_settings_revision: str,
    ) -> LibraryManagementProfile:
        settings = self._preferences.get_library_management_settings_raw()
        for index, current in enumerate(settings.profiles):
            if current.id == profile.id:
                settings.profiles[index] = msgspec.convert(
                    msgspec.to_builtins(profile), type=LibraryManagementProfile
                )
                saved = self.save_settings(
                    settings,
                    expected_settings_revision=expected_settings_revision,
                )
                return self._find_profile(saved, profile.id)
        raise ConfigurationError("The Library Management profile does not exist.")

    def delete_profile(
        self,
        profile_id: str,
        *,
        expected_settings_revision: str,
    ) -> LibraryManagementSettingsResponse:
        settings = self._preferences.get_library_management_settings_raw()
        deleted = self._find_profile(settings, profile_id)
        if deleted.preset_origin is not None:
            raise ConfigurationError(
                "Built-in Library Management presets cannot be deleted."
            )
        if settings.default_profile_id == profile_id:
            raise ConfigurationError("The default profile cannot be deleted.")
        if any(
            assignment.profile_id == profile_id
            for assignment in settings.root_assignments
        ):
            raise ConfigurationError(
                "A profile assigned to a library root cannot be deleted."
            )
        settings.profiles = [
            profile for profile in settings.profiles if profile.id != profile_id
        ]
        candidate_naming_ids = {
            deleted.organization.naming_script_id,
            deleted.organization.multi_disc_naming_script_id,
            deleted.artwork.external_naming_script_id,
        } - {None}
        used_naming_ids = {
            script_id
            for profile in settings.profiles
            for script_id in (
                profile.organization.naming_script_id,
                profile.organization.multi_disc_naming_script_id,
                profile.artwork.external_naming_script_id,
            )
            if script_id is not None
        }
        used_naming_ids.update(
            script_id
            for assignment in settings.root_assignments
            if assignment.overrides is not None
            for script_id in (
                assignment.overrides.naming_script_id,
                assignment.overrides.multi_disc_naming_script_id,
            )
            if script_id is not None
        )
        candidate_tagging_ids = set(deleted.metadata.tagging_script_ids)
        used_tagging_ids = {
            script_id
            for profile in settings.profiles
            for script_id in profile.metadata.tagging_script_ids
        }
        settings.naming_scripts = [
            script
            for script in settings.naming_scripts
            if script.id not in candidate_naming_ids
            or script.id in used_naming_ids
            or script.preset_origin is not None
        ]
        settings.tagging_scripts = [
            script
            for script in settings.tagging_scripts
            if script.id not in candidate_tagging_ids
            or script.id in used_tagging_ids
            or script.preset_origin is not None
        ]
        return self.save_settings(
            settings, expected_settings_revision=expected_settings_revision
        )

    def export_profile(
        self,
        profile_id: str,
        *,
        expected_settings_revision: str,
    ) -> LibraryManagementProfileExportResponse:
        settings = self._preferences.get_library_management_settings_raw()
        current_revision = settings_revision(settings)
        if current_revision != expected_settings_revision:
            raise StaleRevisionError(
                "Library Management settings changed. Refresh this page and try again."
            )
        profile = self._find_profile(settings, profile_id)
        bundle = export_profile_bundle(
            profile,
            settings.naming_scripts,
            settings.tagging_scripts,
        )
        return LibraryManagementProfileExportResponse(
            filename=profile_bundle_filename(profile.name),
            mime_type=PROFILE_BUNDLE_MIME_TYPE,
            document=bundle.document,
            share_code=bundle.share_code,
            bundle_hash=bundle.bundle_hash,
            settings_revision=current_revision,
        )

    def preview_profile_import(
        self,
        content: str,
        *,
        expected_settings_revision: str,
    ) -> LibraryManagementProfileImportPreviewResponse:
        settings = self._preferences.get_library_management_settings_raw()
        current_revision = settings_revision(settings)
        if current_revision != expected_settings_revision:
            raise StaleRevisionError(
                "Library Management settings changed. Refresh this page and try again."
            )
        parsed = parse_profile_bundle(content)
        materialized = self._resolve_import_names(
            preview_materialized_profile(parsed), settings
        )
        return LibraryManagementProfileImportPreviewResponse(
            profile=materialized.profile,
            naming_scripts=materialized.naming_scripts,
            tagging_scripts=materialized.tagging_scripts,
            aspects=profile_aspects(materialized.profile),
            warnings=profile_import_warnings(materialized.profile),
            bundle_hash=parsed.bundle_hash,
            settings_revision=current_revision,
        )

    def import_profile(
        self,
        content: str,
        *,
        reviewed_bundle_hash: str,
        name: str,
        expected_settings_revision: str,
    ) -> LibraryManagementProfileImportResponse:
        settings = self._preferences.get_library_management_settings_raw()
        current_revision = settings_revision(settings)
        if current_revision != expected_settings_revision:
            raise StaleRevisionError(
                "Library Management settings changed. Refresh this page and try again."
            )
        parsed = parse_profile_bundle(content)
        if parsed.bundle_hash != reviewed_bundle_hash:
            raise StaleRevisionError(
                "The shared profile changed after it was reviewed. Review it again."
            )
        materialized = materialize_profile_bundle(
            parsed,
            profile_id=str(uuid.uuid4()),
            naming_id_factory=lambda _script: str(uuid.uuid4()),
            tagging_id_factory=lambda _script: str(uuid.uuid4()),
        )
        materialized = self._resolve_import_names(materialized, settings)
        materialized.profile.name = name
        profile_id = materialized.profile.id
        naming_ids = {script.id for script in materialized.naming_scripts}
        tagging_ids = {script.id for script in materialized.tagging_scripts}
        settings.profiles.append(materialized.profile)
        settings.naming_scripts.extend(materialized.naming_scripts)
        settings.tagging_scripts.extend(materialized.tagging_scripts)
        saved = self.save_settings(
            settings,
            expected_settings_revision=expected_settings_revision,
        )
        return LibraryManagementProfileImportResponse(
            profile=self._find_profile(saved, profile_id),
            naming_scripts=[
                script for script in saved.naming_scripts if script.id in naming_ids
            ],
            tagging_scripts=[
                script for script in saved.tagging_scripts if script.id in tagging_ids
            ],
            settings_revision=saved.settings_revision,
        )

    @classmethod
    def _resolve_import_names(
        cls,
        materialized: MaterializedProfileBundle,
        settings: LibraryManagementSettings,
    ) -> MaterializedProfileBundle:
        materialized.profile.name = unique_import_name(
            materialized.profile.name,
            {profile.name.casefold() for profile in settings.profiles},
        )
        naming_names = {script.name.casefold() for script in settings.naming_scripts}
        for script in materialized.naming_scripts:
            script.name = unique_import_name(script.name, naming_names)
            naming_names.add(script.name.casefold())
        tagging_names = {script.name.casefold() for script in settings.tagging_scripts}
        for script in materialized.tagging_scripts:
            script.name = unique_import_name(script.name, tagging_names)
            tagging_names.add(script.name.casefold())
        detached = LibraryManagementSettings(
            profiles=[materialized.profile],
            default_profile_id=materialized.profile.id,
            naming_scripts=materialized.naming_scripts,
            tagging_scripts=materialized.tagging_scripts,
        )
        normalized = cls._detached_normalized(detached)
        return MaterializedProfileBundle(
            profile=normalized.profiles[0],
            naming_scripts=normalized.naming_scripts,
            tagging_scripts=normalized.tagging_scripts,
        )

    def preset_diff(self, profile_id: str) -> LibraryManagementPresetDiff:
        settings = self._preferences.get_library_management_settings_raw()
        profile = self._find_profile(settings, profile_id)
        preset = (
            preset_profile_for_origin(profile.preset_origin)
            if profile.preset_origin is not None
            else None
        )
        if preset is None:
            return LibraryManagementPresetDiff(
                profile_id=profile.id,
                preset_origin=profile.preset_origin,
                preset_version=profile.preset_version,
            )
        changed = [
            group
            for group in (
                "metadata",
                "genres",
                "artwork",
                "organization",
                "file_behavior",
                "enrichment",
                "notification",
            )
            if msgspec.to_builtins(getattr(profile, group))
            != msgspec.to_builtins(getattr(preset, group))
        ]
        version_upgrade_groups = (
            ["organization"]
            if profile.preset_origin == "picard_style_organizer"
            and (profile.preset_version or 0) < (preset.preset_version or 0)
            else []
        )
        return LibraryManagementPresetDiff(
            profile_id=profile.id,
            preset_origin=profile.preset_origin,
            preset_version=profile.preset_version,
            differs=bool(changed),
            changed_groups=changed,
            version_upgrade_groups=version_upgrade_groups,
            preset_profile=preset,
        )

    def preview_impact(
        self,
        proposed: LibraryManagementSettings,
        *,
        expected_settings_revision: str | None = None,
    ) -> LibraryManagementChangeImpact:
        current = self._preferences.get_library_management_settings_raw()
        current_revision = settings_revision(current)
        normalized = self._detached_normalized(proposed)
        self._validate_preset_provenance(current, normalized)
        self._validate_root_assignments(normalized)
        impact = self._classify(current, normalized)
        impact.stale = (
            expected_settings_revision is not None
            and expected_settings_revision != current_revision
        )
        return impact

    def save_settings(
        self,
        proposed: LibraryManagementSettings,
        *,
        expected_settings_revision: str,
        validated_activation_root_ids: frozenset[str] = frozenset(),
    ) -> LibraryManagementSettingsResponse:
        current = self._preferences.get_library_management_settings_raw()
        current_revision = settings_revision(current)
        if current_revision != expected_settings_revision:
            raise StaleRevisionError(
                "Library Management settings changed. Refresh this page and try again."
            )
        normalized = self._detached_normalized(proposed)
        self._validate_preset_provenance(current, normalized)
        policy = self._validate_root_assignments(normalized)
        impact = self._classify(current, normalized)
        if impact.preview_required:
            assignments = {
                assignment.root_id: assignment
                for assignment in normalized.root_assignments
            }
            for root_id in impact.affected_root_ids:
                assignment = assignments.get(root_id)
                if not _active_automatic(assignment):
                    continue
                assert assignment is not None
                effective_profile_revision = profile_revision(
                    self._effective_profile(normalized, assignment)
                )
                pinned = self._pin_effective_profile(normalized, assignment)
                expected_activation_naming_revision = naming_policy_revision(pinned)
                activation_naming_matches = (
                    assignment.activation_naming_policy_revision
                    == expected_activation_naming_revision
                    or (
                        assignment.activation_naming_policy_revision is None
                        and pinned.multi_disc_naming_script is None
                    )
                )
                if (
                    assignment.activation_profile_revision != effective_profile_revision
                    or not activation_naming_matches
                    or assignment.activation_policy_revision != policy.policy_revision
                    or assignment.activation_settings_revision != current_revision
                    or not assignment.activation_preview_token
                    or not assignment.activation_preview_hash
                    or not assignment.activation_confirmed_at
                    or (
                        root_id not in validated_activation_root_ids
                        and (
                            self._activation_validator is None
                            or not self._activation_validator(assignment)
                        )
                    )
                ):
                    raise ConfigurationError(
                        "A current Library Management dry run must be confirmed before "
                        "this automatic change can be enabled."
                    )
        return self._preferences.save_library_management_settings_if_current(
            normalized,
            expected_settings_revision=expected_settings_revision,
        )

    def prepare_activation(
        self,
        proposed: LibraryManagementSettings,
        *,
        root_id: str,
        expected_settings_revision: str,
    ) -> tuple[
        LibraryManagementSettings,
        LibraryManagementRootAssignment,
        LibraryManagementProfile,
        LibraryPolicyResolver,
    ]:
        current_revision = settings_revision(
            self._preferences.get_library_management_settings_raw()
        )
        if current_revision != expected_settings_revision:
            raise StaleRevisionError(
                "Library Management settings changed. Refresh this page and try again."
            )
        normalized = self._detached_normalized(proposed)
        policy = self._validate_root_assignments(normalized)
        assignment = next(
            (
                value
                for value in normalized.root_assignments
                if value.root_id == root_id
            ),
            None,
        )
        if assignment is None or not _active_automatic(assignment):
            raise ConfigurationError(
                "Activation requires an enabled root assignment and automatic trigger."
            )
        return (
            normalized,
            assignment,
            self._effective_profile(normalized, assignment),
            policy,
        )

    def prepare_manual_profile(
        self,
        profile_id: str,
        overrides: LibraryManagementRootOverrides | None,
    ) -> tuple[LibraryManagementSettings, LibraryManagementProfile]:
        settings = self._preferences.get_library_management_settings_raw()
        self._find_profile(settings, profile_id)
        effective = self._effective_profile(
            settings,
            LibraryManagementRootAssignment(
                root_id="__manual_preview__",
                profile_id=profile_id,
                overrides=overrides,
            ),
        )
        naming_ids = {script.id for script in settings.naming_scripts}
        if effective.organization.naming_script_id not in naming_ids:
            raise ConfigurationError(
                "The manual preview references an unknown naming script."
            )
        if (
            effective.organization.multi_disc_naming_script_id is not None
            and effective.organization.multi_disc_naming_script_id not in naming_ids
        ):
            raise ConfigurationError(
                "The manual preview references an unknown multi-disc naming script."
            )
        return settings, effective

    def prepare_automatic_profile(
        self,
        *,
        root_id: str,
        trigger: str,
        expected_policy_revision: str,
    ) -> (
        tuple[
            LibraryManagementSettings,
            LibraryManagementRootAssignment,
            LibraryManagementProfile,
            LibraryPolicyResolver,
        ]
        | None
    ):
        """Resolve one current, dry-run-authorized automatic root assignment."""

        trigger_field = {
            "acquisition": "automatic_acquisitions",
            "drop_import": "automatic_drop_imports",
            "scan_discovered": "automatic_scan_discovered",
        }.get(trigger)
        if trigger_field is None:
            raise ConfigurationError("Unknown Library Management automatic trigger.")
        settings = self._preferences.get_library_management_settings_raw()
        policy = self._validate_root_assignments(settings)
        if policy.policy_revision != expected_policy_revision:
            raise StaleRevisionError(
                "Library policy changed before automatic management."
            )
        assignment = next(
            (value for value in settings.root_assignments if value.root_id == root_id),
            None,
        )
        if (
            assignment is None
            or not assignment.enabled
            or not getattr(assignment, trigger_field)
        ):
            return None
        effective = self._effective_profile(settings, assignment)
        pinned = self._pin_effective_profile(settings, assignment)
        if not _activation_is_current(
            assignment, effective, pinned, policy
        ) and not migration_carry_applies(assignment, effective, pinned, policy):
            raise StaleRevisionError(
                "Library Management activation is stale; run and confirm a new dry run."
            )
        return settings, assignment, effective, policy

    def activation_health(self) -> LibraryManagementActivationHealthResponse:
        """Report dry-run activation health for active automatic roots.

        Advisory read for the automation tab. One broken assignment must
        not hide the others. Roots whose effective profile cannot even
        resolve land in blocked: a dry run re-validates the same
        settings and would fail identically, so no dry run is offered.
        Roots that resolve but no longer match their stored activation
        land in stale. A policy that cannot resolve at all (unknown
        root, unavailable path, recycle-bin overlap) blocks every active
        root with the policy error as the reason.
        """

        settings = self._preferences.get_library_management_settings_raw()
        try:
            policy = self._validate_root_assignments(settings)
        except ConfigurationError as error:
            blocked = [
                assignment.root_id
                for assignment in settings.root_assignments
                if _active_automatic(assignment)
            ]
            return LibraryManagementActivationHealthResponse(
                blocked_root_ids=blocked,
                blocked_reason=str(error) if blocked else None,
            )
        stale: list[str] = []
        blocked: list[str] = []
        for assignment in settings.root_assignments:
            if not _active_automatic(assignment):
                continue
            try:
                effective = self._effective_profile(settings, assignment)
                pinned = self._pin_effective_profile(settings, assignment)
            except Exception:  # noqa: BLE001 - health must survive one bad root
                blocked.append(assignment.root_id)
                continue
            if _activation_is_current(
                assignment, effective, pinned, policy
            ) or migration_carry_applies(assignment, effective, pinned, policy):
                continue
            stale.append(assignment.root_id)
        return LibraryManagementActivationHealthResponse(
            stale_root_ids=stale, blocked_root_ids=blocked
        )

    def prepare_conversion_profile(
        self,
        *,
        root_id: str,
        expected_policy_revision: str,
    ) -> tuple[
        LibraryManagementSettings,
        LibraryManagementRootAssignment,
        LibraryManagementProfile,
        LibraryPolicyResolver,
    ]:
        """Resolve the current root profile for an administrator-confirmed conversion."""

        settings = self._preferences.get_library_management_settings_raw()
        policy = self._validate_root_assignments(settings)
        if policy.policy_revision != expected_policy_revision:
            raise StaleRevisionError(
                "Library policy changed before edition conversion."
            )
        assignment = next(
            (value for value in settings.root_assignments if value.root_id == root_id),
            LibraryManagementRootAssignment(
                root_id=root_id, profile_id=settings.default_profile_id
            ),
        )
        return (
            settings,
            assignment,
            self._effective_profile(settings, assignment),
            policy,
        )

    def prepare_tag_editor_profile(
        self,
        *,
        root_id: str,
        field_names: tuple[str, ...],
        reset_canonical: bool,
    ) -> tuple[LibraryManagementSettings, LibraryManagementProfile]:
        """Build a detached, tag-only profile without changing stored settings."""

        settings = self._preferences.get_library_management_settings_raw()
        assignment = next(
            (value for value in settings.root_assignments if value.root_id == root_id),
            LibraryManagementRootAssignment(
                root_id=root_id,
                profile_id=settings.default_profile_id,
            ),
        )
        effective = self._effective_profile(settings, assignment)
        effective.metadata.enabled = True
        effective.metadata.fields = [
            ManagedFieldSettings(field=name, mode="replace") for name in field_names
        ]
        effective.metadata.tagging_script_ids = []
        effective.metadata.preserve_fields = []
        effective.metadata.scrub_unmanaged_tags = False
        effective.artwork.embedded_enabled = False
        effective.artwork.external_enabled = False
        effective.organization.rename_enabled = False
        effective.organization.move_enabled = False
        effective.organization.move_sidecars = False
        effective.organization.source_cleanup = "keep"
        effective.organization.remove_empty_directories = False
        effective.enrichment.lyrics.enabled = False
        effective.enrichment.replaygain.enabled = False
        effective.genres.enabled = "genre" in field_names
        if effective.genres.enabled and not reset_canonical:
            effective.genres.sources = ["existing_local"]
            effective.genres.mode = "replace"
            effective.genres.canonicalize = False
        return settings, effective

    def _validate_root_assignments(
        self, settings: LibraryManagementSettings
    ) -> LibraryPolicyResolver:
        policy = LibraryPolicyResolver(
            self._preferences.get_typed_library_settings_raw()
        )
        roots = {root.id: root for root in policy.settings.library_roots}
        if settings.recycle_bin_path:
            recycle = Path(settings.recycle_bin_path).resolve(strict=False)
            for root in roots.values():
                library_root = Path(root.path).resolve(strict=False)
                if (
                    recycle == library_root
                    or recycle in library_root.parents
                    or library_root in recycle.parents
                ):
                    raise ConfigurationError(
                        "The Library Management recycle bin cannot overlap a library root."
                    )
        for assignment in settings.root_assignments:
            root = roots.get(assignment.root_id)
            if root is None:
                raise ConfigurationError(
                    "A Library Management assignment references an unknown root."
                )
            if not _active_automatic(assignment):
                continue
            path = Path(root.path)
            if not path.exists() or not path.is_dir():
                raise ConfigurationError(
                    f"Library root {root.label} is not currently available."
                )
            if not os.access(path, os.W_OK):
                raise ConfigurationError(
                    f"Library root {root.label} is not currently writable."
                )
        return policy

    @staticmethod
    def _detached_normalized(
        settings: LibraryManagementSettings,
    ) -> LibraryManagementSettings:
        detached = msgspec.convert(
            msgspec.to_builtins(settings), type=LibraryManagementSettings
        )
        try:
            return normalize_library_management_settings(detached)
        except (ScriptValidationError, ValueError) as exc:
            raise ConfigurationError(str(exc)) from exc

    @staticmethod
    def _validate_preset_provenance(
        current: LibraryManagementSettings,
        proposed: LibraryManagementSettings,
    ) -> None:
        current_by_id = {profile.id: profile for profile in current.profiles}
        proposed_by_id = {profile.id: profile for profile in proposed.profiles}
        for profile in current.profiles:
            candidate = proposed_by_id.get(profile.id)
            if profile.preset_origin is not None and candidate is None:
                raise ConfigurationError(
                    "Built-in Library Management presets cannot be deleted."
                )
            if (
                candidate is not None
                and candidate.preset_origin != profile.preset_origin
            ):
                raise ConfigurationError(
                    "Library Management preset identity cannot be changed."
                )
        if any(
            profile.id not in current_by_id and profile.preset_origin is not None
            for profile in proposed.profiles
        ):
            raise ConfigurationError(
                "Library Management preset identity cannot be assigned to a custom profile."
            )

    @staticmethod
    def _find_profile(
        settings: LibraryManagementSettings | LibraryManagementSettingsResponse,
        profile_id: str,
    ) -> LibraryManagementProfile:
        for profile in settings.profiles:
            if profile.id == profile_id:
                return profile
        raise ConfigurationError("The Library Management profile does not exist.")

    @staticmethod
    def _effective_profile(
        settings: LibraryManagementSettings,
        assignment: LibraryManagementRootAssignment,
    ) -> LibraryManagementProfile:
        profile_id = assignment.profile_id or settings.default_profile_id
        source = next(
            profile for profile in settings.profiles if profile.id == profile_id
        )
        effective = msgspec.convert(
            msgspec.to_builtins(source), type=LibraryManagementProfile
        )
        overrides = assignment.overrides
        if overrides is None:
            return effective
        for field in (
            "metadata_enabled",
            "genres_enabled",
            "rename_enabled",
            "move_enabled",
            "move_sidecars",
            "preserve_timestamps",
        ):
            value = getattr(overrides, field)
            if value is None:
                continue
            target, name = {
                "metadata_enabled": (effective.metadata, "enabled"),
                "genres_enabled": (effective.genres, "enabled"),
                "rename_enabled": (effective.organization, "rename_enabled"),
                "move_enabled": (effective.organization, "move_enabled"),
                "move_sidecars": (effective.organization, "move_sidecars"),
                "preserve_timestamps": (
                    effective.file_behavior,
                    "preserve_timestamps",
                ),
            }[field]
            setattr(target, name, value)
        if overrides.embedded_artwork_enabled is not None:
            effective.artwork.embedded_enabled = overrides.embedded_artwork_enabled
        if overrides.external_artwork_enabled is not None:
            effective.artwork.external_enabled = overrides.external_artwork_enabled
        if overrides.source_cleanup is not None:
            effective.organization.source_cleanup = overrides.source_cleanup
        # D-EDITION-AUTO S-3: per-root override of the profile-level opt-in.
        if overrides.automatic_edition_acceptance_enabled is not None:
            effective.identity.automatic_edition_acceptance_enabled = (
                overrides.automatic_edition_acceptance_enabled
            )
        if overrides.naming_script_id is not None:
            effective.organization.naming_script_id = overrides.naming_script_id
        if overrides.multi_disc_naming_mode == "standard":
            effective.organization.multi_disc_naming_script_id = None
        elif overrides.multi_disc_naming_mode == "script":
            effective.organization.multi_disc_naming_script_id = (
                overrides.multi_disc_naming_script_id
            )
        effective.revision = profile_revision(effective)
        return effective

    @classmethod
    def _pin_effective_profile(
        cls,
        settings: LibraryManagementSettings,
        assignment: LibraryManagementRootAssignment,
    ) -> PinnedLibraryManagementProfile:
        try:
            return pin_library_management_profile(
                settings, cls._effective_profile(settings, assignment)
            )
        except ValueError as error:
            raise ConfigurationError(str(error)) from error

    @classmethod
    def _effective_scope_payload(
        cls,
        settings: LibraryManagementSettings,
        assignment: LibraryManagementRootAssignment,
    ) -> dict:
        profile = cls._effective_profile(settings, assignment)
        payload = _profile_scope_payload(profile)
        naming_scripts = {script.id: script for script in settings.naming_scripts}
        tagging_scripts = {script.id: script for script in settings.tagging_scripts}
        payload["_naming_script_revision"] = naming_scripts[
            profile.organization.naming_script_id
        ].revision
        multi_disc_script_id = profile.organization.multi_disc_naming_script_id
        payload["_multi_disc_naming_script_revision"] = (
            naming_scripts[multi_disc_script_id].revision
            if multi_disc_script_id is not None
            else None
        )
        payload["_naming_policy_revision"] = naming_policy_revision(
            cls._pin_effective_profile(settings, assignment)
        )
        external_script_id = profile.artwork.external_naming_script_id
        payload["_external_artwork_script_revision"] = (
            naming_scripts[external_script_id].revision
            if external_script_id is not None
            else None
        )
        payload["_tagging_script_revisions"] = [
            tagging_scripts[script_id].revision
            for script_id in profile.metadata.tagging_script_ids
        ]
        return payload

    @classmethod
    def _classify(
        cls,
        current: LibraryManagementSettings,
        proposed: LibraryManagementSettings,
    ) -> LibraryManagementChangeImpact:
        current_revision = settings_revision(current)
        proposed_revision = settings_revision(proposed)
        if current_revision == proposed_revision:
            return LibraryManagementChangeImpact(
                current_settings_revision=current_revision,
                proposed_settings_revision=proposed_revision,
            )

        current_assignments = {
            assignment.root_id: assignment for assignment in current.root_assignments
        }
        proposed_assignments = {
            assignment.root_id: assignment for assignment in proposed.root_assignments
        }
        destructive: list[str] = []
        restrictive: list[str] = []
        harmless: list[str] = []
        affected: set[str] = set()
        for root_id in sorted(set(current_assignments) | set(proposed_assignments)):
            old_assignment = current_assignments.get(root_id)
            new_assignment = proposed_assignments.get(root_id)
            old_active = _active_automatic(old_assignment)
            new_active = _active_automatic(new_assignment)
            if not old_active and new_active:
                destructive.append(
                    f"Automatic Library Management is enabled for root {root_id}."
                )
                affected.add(root_id)
                continue
            if old_active and not new_active:
                restrictive.append(
                    f"Automatic Library Management is reduced for root {root_id}."
                )
                affected.add(root_id)
                continue
            if not old_active or old_assignment is None or new_assignment is None:
                continue

            added_trigger = any(
                getattr(new_assignment, field) and not getattr(old_assignment, field)
                for field in (
                    "automatic_acquisitions",
                    "automatic_drop_imports",
                    "automatic_scan_discovered",
                )
            )
            removed_trigger = any(
                getattr(old_assignment, field) and not getattr(new_assignment, field)
                for field in (
                    "automatic_acquisitions",
                    "automatic_drop_imports",
                    "automatic_scan_discovered",
                )
            )
            old_profile = cls._effective_profile(current, old_assignment)
            new_profile = cls._effective_profile(proposed, new_assignment)
            old_payload = cls._effective_scope_payload(current, old_assignment)
            new_payload = cls._effective_scope_payload(proposed, new_assignment)
            if added_trigger:
                harmless.append(
                    f"An automatic trigger is enabled for root {root_id}; the "
                    "authorized write profile is unchanged."
                )
                affected.add(root_id)
            old_custom_automatic = (
                old_assignment.enabled
                and old_assignment.automatic_scan_discovered
                and old_assignment.automatic_custom_editions
            )
            new_custom_automatic = (
                new_assignment.enabled
                and new_assignment.automatic_scan_discovered
                and new_assignment.automatic_custom_editions
            )
            if new_custom_automatic and not old_custom_automatic:
                destructive.append(
                    f"Automatic Custom edition management is enabled for root {root_id}."
                )
                affected.add(root_id)
            if old_payload != new_payload:
                affected.add(root_id)
                if _is_restrictive_profile_change(old_profile, new_profile):
                    restrictive.append(
                        f"The effective profile is restricted for root {root_id}."
                    )
                else:
                    destructive.append(
                        f"The effective profile changes write scope for root {root_id}."
                    )
            elif removed_trigger:
                restrictive.append(
                    f"An automatic trigger is disabled for root {root_id}."
                )
                affected.add(root_id)

        if destructive:
            classification = "destructive"
            reasons = destructive + restrictive
        elif restrictive:
            classification = "restrictive"
            reasons = restrictive + harmless
        else:
            classification = "harmless"
            reasons = harmless or [
                "No enabled automatic root gains file-writing scope."
            ]
        return LibraryManagementChangeImpact(
            current_settings_revision=current_revision,
            proposed_settings_revision=proposed_revision,
            classification=classification,
            preview_required=bool(destructive),
            affected_root_ids=sorted(affected),
            reasons=reasons,
        )
