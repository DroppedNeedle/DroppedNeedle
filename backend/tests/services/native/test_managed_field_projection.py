from __future__ import annotations

from api.v1.schemas.library_management import (
    MANAGED_FIELD_NAMES,
    picard_style_organizer_profile,
)
from models.library_management import LibraryManagementOverride
from core.exceptions import ValidationError
import pytest
from services.native.effective_metadata_projection_service import (
    EffectiveMetadataProjectionService,
    normalize_managed_field_value,
)
from services.native.managed_field_registry import (
    ADMITTED_MANAGEMENT_FORMATS,
    MANAGED_FIELD_REGISTRY,
)


def _setting(profile, name):
    return next(value for value in profile.metadata.fields if value.field == name)


def _override(
    *, subject_kind: str, field_name: str, value_json: str, mode: str = "replace"
) -> LibraryManagementOverride:
    return LibraryManagementOverride(
        id=f"{subject_kind}-{field_name}",
        subject_kind=subject_kind,
        local_album_id="album-1" if subject_kind == "album" else None,
        local_track_id="track-1" if subject_kind == "track" else None,
        field_name=field_name,
        value_json=value_json,
        mode=mode,
        subject_revision=1,
    )


def test_registry_exhaustively_declares_every_field_and_format_hook() -> None:
    assert set(MANAGED_FIELD_REGISTRY) == {*MANAGED_FIELD_NAMES, "genre"}
    for name, field in MANAGED_FIELD_REGISTRY.items():
        assert field.name == name
        assert field.canonical_source
        assert field.cardinality
        assert field.empty_canonical_behavior == "preserve_unless_explicit_clear"
        assert field.allow_override is True
        assert field.allow_preserve is True
        assert field.enabled_in_picard_preset is True
        assert {hook.audio_format for hook in field.adapter_hooks} == set(
            ADMITTED_MANAGEMENT_FORMATS
        )
        assert all(hook.adapter_hook for hook in field.adapter_hooks)
        assert all(
            hook.required_capability == f"field.{name}" for hook in field.adapter_hooks
        )
        assert field.merge_supported is (field.cardinality == "ordered_strings")


def test_profile_modes_apply_replace_fill_merge_and_preserve() -> None:
    profile = picard_style_organizer_profile()
    _setting(profile, "title").mode = "fill_missing"
    _setting(profile, "artist").mode = "merge"
    _setting(profile, "barcode").mode = "preserve"
    projection = EffectiveMetadataProjectionService().project(
        profile=profile,
        canonical_values={
            "album": "Canonical Album",
            "title": "Canonical Title",
            "artist": ("Canonical Artist", "Shared"),
            "barcode": "canonical-barcode",
        },
        existing_values={
            "album": "Old Album",
            "title": "Existing Title",
            "artist": ("Existing Artist", "shared"),
            "barcode": "existing-barcode",
        },
        canonical_available=True,
    )

    assert projection.value_for("album") == "Canonical Album"
    assert projection.value_for("title") == "Existing Title"
    assert projection.value_for("artist") == (
        "Canonical Artist",
        "Shared",
        "Existing Artist",
    )
    assert projection.value_for("barcode") == "existing-barcode"


def test_clear_missing_is_distinct_from_provider_outage() -> None:
    profile = picard_style_organizer_profile()
    _setting(profile, "title").clear_when_canonical_missing = True
    projector = EffectiveMetadataProjectionService()

    absent = projector.project(
        profile=profile,
        canonical_values={"title": None},
        existing_values={"title": "Existing"},
        canonical_available=True,
    )
    outage = projector.project(
        profile=profile,
        canonical_values={"title": None},
        existing_values={"title": "Existing"},
        canonical_available=False,
    )

    assert absent.value_for("title") is None
    assert absent.field_for("title").cleared is True
    assert absent.field_for("title").source == "explicit_clear"
    assert outage.value_for("title") == "Existing"
    assert outage.field_for("title").source == "existing"


def test_projection_order_ends_with_persistent_then_manual_overrides() -> None:
    profile = picard_style_organizer_profile()
    projection = EffectiveMetadataProjectionService().project(
        profile=profile,
        canonical_values={"album": "Canonical Album", "title": "Canonical Title"},
        enriched_values={"album": "Enriched Album", "title": "Enriched Title"},
        existing_values={"album": "Existing Album", "title": "Existing Title"},
        transformed_values={
            "album": "Transformed Album",
            "title": "Transformed Title",
        },
        album_overrides=(
            _override(
                subject_kind="album",
                field_name="album",
                value_json='"Persistent Album"',
            ),
        ),
        track_overrides=(
            _override(
                subject_kind="track",
                field_name="title",
                value_json='"Persistent Title"',
            ),
        ),
        manual_overrides={"album": "Manual Album", "title": "Manual Title"},
        canonical_available=True,
    )

    assert projection.value_for("album") == "Manual Album"
    assert projection.field_for("album").source == "manual_override"
    assert projection.value_for("title") == "Manual Title"
    assert projection.field_for("title").source == "manual_override"


def test_persistent_replace_clear_and_preserve_override_behavior() -> None:
    profile = picard_style_organizer_profile()
    projection = EffectiveMetadataProjectionService().project(
        profile=profile,
        canonical_values={
            "album": "Canonical Album",
            "title": "Canonical Title",
            "artist": ("Canonical Artist",),
        },
        existing_values={
            "album": "Existing Album",
            "title": "Existing Title",
            "artist": ("Existing Artist",),
        },
        album_overrides=(
            _override(
                subject_kind="album",
                field_name="album",
                value_json='"Override Album"',
            ),
        ),
        track_overrides=(
            _override(
                subject_kind="track",
                field_name="title",
                value_json="null",
                mode="preserve",
            ),
            _override(
                subject_kind="track",
                field_name="artist",
                value_json="null",
                mode="clear",
            ),
        ),
        canonical_available=True,
    )

    assert projection.value_for("album") == "Override Album"
    assert projection.field_for("album").source == "album_override"
    assert projection.value_for("title") == "Existing Title"
    assert projection.field_for("title").source == "track_override"
    assert projection.value_for("artist") == ()
    assert projection.field_for("artist").cleared is True


def test_preserve_fields_win_and_scrub_only_targets_unmanaged_fields() -> None:
    profile = picard_style_organizer_profile()
    profile.metadata.preserve_fields = ["title"]
    profile.metadata.scrub_unmanaged_tags = True
    projection = EffectiveMetadataProjectionService().project(
        profile=profile,
        canonical_values={"title": "Canonical"},
        existing_values={"title": "Existing"},
        canonical_available=True,
    )
    projector = EffectiveMetadataProjectionService()

    assert projection.value_for("title") == "Existing"
    assert projector.should_scrub_unmanaged_field("title", projection) is False
    assert projector.should_scrub_unmanaged_field("custom:mood", projection) is True


def _names(count: int) -> list[str]:
    return [f"performer-{i:04d}" for i in range(count)]


def _casefold_collisions(count: int) -> list[str]:
    base = "The Beatles"
    return [base.upper() if i % 2 else base.lower() for i in range(count)]


def test_performer_accepts_134_unique_values() -> None:
    field = MANAGED_FIELD_REGISTRY["performer"]
    assert field.max_unique_values == 256
    result = normalize_managed_field_value(field, _names(134))
    assert len(result) == 134


def test_performer_rejects_257_unique_values() -> None:
    field = MANAGED_FIELD_REGISTRY["performer"]
    with pytest.raises(ValidationError, match="bounded list"):
        normalize_managed_field_value(field, _names(257))


def test_non_performer_ordered_strings_rejects_101_unique_values() -> None:
    field = MANAGED_FIELD_REGISTRY["composer"]
    assert field.max_unique_values == 100
    with pytest.raises(ValidationError, match="bounded list"):
        normalize_managed_field_value(field, _names(101))


def test_non_performer_ordered_strings_accepts_after_dedup() -> None:
    field = MANAGED_FIELD_REGISTRY["composer"]
    # 102 inputs: 100 unique + 2 casefold duplicates of existing names
    redundant = _names(100) + ["PERFORMER-0000", "PERFORMER-0001"]
    result = normalize_managed_field_value(field, redundant)
    assert len(result) == 100


def test_performer_accepts_101_unique_values_under_new_limit() -> None:
    field = MANAGED_FIELD_REGISTRY["performer"]
    result = normalize_managed_field_value(field, _names(101))
    assert len(result) == 101


def test_bounded_list_dedup_is_casefold_first() -> None:
    field = MANAGED_FIELD_REGISTRY["performer"]
    result = normalize_managed_field_value(field, _casefold_collisions(300))
    assert len(result) == 1
