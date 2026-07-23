"""Inert profile CRUD, assignment validation, and activation-impact policy."""

from __future__ import annotations

import copy
import os
from pathlib import Path
from collections.abc import Callable
import uuid

import msgspec

from api.v1.schemas.library_management import (
    LibraryManagementChangeImpact,
    ManagedFieldSettings,
    LibraryManagementPresetDiff,
    LibraryManagementProfile,
    LibraryManagementRootAssignment,
    LibraryManagementRootOverrides,
    LibraryManagementSettings,
    LibraryManagementSettingsResponse,
    normalize_library_management_settings,
    picard_style_organizer_profile,
    profile_revision,
    settings_revision,
)
from core.exceptions import (
    ConfigurationError,
    ScriptValidationError,
    StaleRevisionError,
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
        self._find_profile(settings, profile_id)
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
        return self.save_settings(
            settings, expected_settings_revision=expected_settings_revision
        )

    def preset_diff(self, profile_id: str) -> LibraryManagementPresetDiff:
        settings = self._preferences.get_library_management_settings_raw()
        profile = self._find_profile(settings, profile_id)
        if profile.preset_origin != "picard_style_organizer":
            return LibraryManagementPresetDiff(
                profile_id=profile.id,
                preset_origin=profile.preset_origin,
                preset_version=profile.preset_version,
            )
        preset = picard_style_organizer_profile()
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
        return LibraryManagementPresetDiff(
            profile_id=profile.id,
            preset_origin=profile.preset_origin,
            preset_version=profile.preset_version,
            differs=bool(changed),
            changed_groups=changed,
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
                if (
                    assignment.activation_profile_revision != effective_profile_revision
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
        if (
            assignment.activation_profile_revision != profile_revision(effective)
            or assignment.activation_policy_revision != policy.policy_revision
            or not assignment.activation_preview_token
            or not assignment.activation_preview_hash
            or assignment.activation_confirmed_at is None
        ):
            raise StaleRevisionError(
                "Library Management activation is stale; run and confirm a new dry run."
            )
        return settings, assignment, effective, policy

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
        if overrides.naming_script_id is not None:
            effective.organization.naming_script_id = overrides.naming_script_id
        return effective

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
                destructive.append(
                    f"An automatic trigger is enabled for root {root_id}."
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
            reasons = restrictive
        else:
            classification = "harmless"
            reasons = ["No enabled automatic root gains file-writing scope."]
        return LibraryManagementChangeImpact(
            current_settings_revision=current_revision,
            proposed_settings_revision=proposed_revision,
            classification=classification,
            preview_required=bool(destructive),
            affected_root_ids=sorted(affected),
            reasons=reasons,
        )
