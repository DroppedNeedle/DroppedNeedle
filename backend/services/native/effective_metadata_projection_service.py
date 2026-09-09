"""Apply profile ownership and override precedence without touching files."""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from typing import Any

import msgspec

from api.v1.schemas.library_management import LibraryManagementProfile
from core.exceptions import ValidationError
from models.library_management import LibraryManagementOverride
from models.library_management_projection import (
    EffectiveManagedField,
    EffectiveMetadataProjection,
    FieldProjectionSource,
)
from services.native.managed_field_registry import (
    MANAGED_FIELD_REGISTRY,
    ManagedFieldDefinition,
)

_MISSING = object()


def _is_empty(value: object) -> bool:
    return value is None or value == "" or value == () or value == []


def _clear_value(field: ManagedFieldDefinition) -> object:
    return () if field.cardinality == "ordered_strings" else None


def normalize_managed_field_value(
    field: ManagedFieldDefinition, value: object
) -> object:
    if value is None:
        return None
    if field.cardinality == "string":
        if not isinstance(value, str) or "\x00" in value or len(value) > 4096:
            raise ValidationError(f"{field.name} requires a valid text value.")
        return value
    if field.cardinality == "integer":
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            raise ValidationError(f"{field.name} requires a non-negative integer.")
        return value
    if field.cardinality == "boolean":
        if not isinstance(value, bool):
            raise ValidationError(f"{field.name} requires true or false.")
        return value
    if not isinstance(value, (list, tuple)):
        raise ValidationError(f"{field.name} requires a bounded list of text values.")
    result: list[str] = []
    seen: set[str] = set()
    for item in value:
        if not isinstance(item, str) or "\x00" in item or len(item) > 4096:
            raise ValidationError(f"{field.name} contains an invalid text value.")
        folded = item.casefold()
        if folded not in seen:
            seen.add(folded)
            result.append(item)
    if len(result) > field.max_unique_values:
        raise ValidationError(f"{field.name} requires a bounded list of text values.")
    return tuple(result)


def _merge_values(canonical: object, existing: object) -> tuple[str, ...]:
    result: list[str] = []
    seen: set[str] = set()
    for value in (canonical, existing):
        if not isinstance(value, (list, tuple)):
            continue
        for item in value:
            if not isinstance(item, str):
                continue
            folded = item.casefold()
            if folded not in seen:
                seen.add(folded)
                result.append(item)
    return tuple(result)


def _decode_override(
    override: LibraryManagementOverride, field: ManagedFieldDefinition
) -> object:
    try:
        value = msgspec.json.decode(override.value_json.encode("utf-8"))
    except msgspec.DecodeError as error:
        raise ValidationError(
            f"The persisted {field.name} override is invalid."
        ) from error
    return normalize_managed_field_value(field, value)


class EffectiveMetadataProjectionService:
    def project(
        self,
        *,
        profile: LibraryManagementProfile,
        canonical_values: Mapping[str, object],
        existing_values: Mapping[str, object],
        canonical_available: bool,
        enriched_values: Mapping[str, object] | None = None,
        transformed_values: Mapping[str, object] | None = None,
        album_overrides: Sequence[LibraryManagementOverride] = (),
        track_overrides: Sequence[LibraryManagementOverride] = (),
        manual_overrides: Mapping[str, object] | None = None,
        forced_clear_fields: frozenset[str] = frozenset(),
    ) -> EffectiveMetadataProjection:
        enriched = enriched_values or {}
        transformed = transformed_values or {}
        manual = manual_overrides or {}
        configured = {field.field: field for field in profile.metadata.fields}
        preserved = set(profile.metadata.preserve_fields)
        album_by_field = {value.field_name: value for value in album_overrides}
        track_by_field = {value.field_name: value for value in track_overrides}
        projected: list[EffectiveManagedField] = []

        for name, definition in MANAGED_FIELD_REGISTRY.items():
            setting = configured.get(name)
            mode = setting.mode if setting is not None else "disabled"
            existing = normalize_managed_field_value(
                definition, existing_values.get(name)
            )
            value = existing
            source: FieldProjectionSource = "existing"
            cleared = False
            provider_usable = (
                canonical_available or definition.source_provider != "musicbrainz"
            )
            candidate: object = _MISSING
            candidate_source: FieldProjectionSource = "canonical"
            if provider_usable and name in canonical_values:
                candidate = normalize_managed_field_value(
                    definition, canonical_values[name]
                )
            if name in enriched:
                candidate = normalize_managed_field_value(definition, enriched[name])
                candidate_source = "enrichment"

            if mode not in {"disabled", "preserve"} and name not in preserved:
                if mode == "replace":
                    if candidate is not _MISSING and not _is_empty(candidate):
                        value = candidate
                        source = candidate_source
                    elif (
                        provider_usable
                        and setting is not None
                        and setting.clear_when_canonical_missing
                    ):
                        value = _clear_value(definition)
                        source = "explicit_clear"
                        cleared = True
                elif mode == "fill_missing":
                    if _is_empty(existing) and candidate is not _MISSING:
                        value = candidate
                        source = candidate_source
                elif mode == "merge":
                    if not definition.merge_supported:
                        raise ValidationError(f"{name} does not support merge.")
                    value = _merge_values(candidate, existing)
                    source = candidate_source if value != existing else "existing"

            if name in transformed:
                value = normalize_managed_field_value(definition, transformed[name])
                source = "transformation"
                cleared = _is_empty(value)

            value, source, cleared = self._apply_override(
                definition,
                existing,
                value,
                source,
                cleared,
                album_by_field.get(name),
                "album_override",
            )
            value, source, cleared = self._apply_override(
                definition,
                existing,
                value,
                source,
                cleared,
                track_by_field.get(name),
                "track_override",
            )
            if name in manual:
                manual_value = normalize_managed_field_value(definition, manual[name])
                value = manual_value
                source = "manual_override"
                cleared = _is_empty(value)

            if name in forced_clear_fields:
                value = _clear_value(definition)
                source = "explicit_clear"
                cleared = True

            projected.append(
                EffectiveManagedField(
                    name=name,
                    value=value,
                    source=source,
                    mode=mode,
                    cleared=cleared,
                )
            )

        return EffectiveMetadataProjection(
            fields=tuple(projected),
            scrub_unmanaged_tags=profile.metadata.scrub_unmanaged_tags,
            preserved_fields=tuple(profile.metadata.preserve_fields),
        )

    @staticmethod
    def _apply_override(
        definition: ManagedFieldDefinition,
        existing: object,
        current: object,
        source: FieldProjectionSource,
        cleared: bool,
        override: LibraryManagementOverride | None,
        override_source: FieldProjectionSource,
    ) -> tuple[object, FieldProjectionSource, bool]:
        if override is None:
            return current, source, cleared
        if override.mode == "preserve":
            return existing, override_source, _is_empty(existing)
        if override.mode == "clear":
            return _clear_value(definition), "explicit_clear", True
        value = _decode_override(override, definition)
        return value, override_source, _is_empty(value)

    @staticmethod
    def should_scrub_unmanaged_field(
        field_name: str, projection: EffectiveMetadataProjection
    ) -> bool:
        if not projection.scrub_unmanaged_tags:
            return False
        return (
            field_name not in MANAGED_FIELD_REGISTRY
            and field_name not in projection.preserved_fields
        )
