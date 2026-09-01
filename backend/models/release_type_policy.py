from collections.abc import Iterable
from typing import Any


def _normalize_release_type(value: Any) -> str:
    if value is None:
        return ""
    return str(value).strip().casefold()


def normalize_release_type_filters(
    primary_types: Iterable[Any] | None,
    secondary_types: Iterable[Any] | None,
) -> tuple[frozenset[str], frozenset[str]]:
    def normalize(values: Iterable[Any] | None) -> frozenset[str]:
        if values is None:
            return frozenset()
        if isinstance(values, str):
            values = (values,)
        return frozenset(
            normalized
            for value in values
            if (normalized := _normalize_release_type(value))
        )

    return normalize(primary_types), normalize(secondary_types)


def should_include_release(
    release_group: dict[str, Any],
    included_secondary_types: Iterable[Any] | None = None,
    included_primary_types: Iterable[Any] | None = None,
    *,
    apply_default_secondary_exclusions: bool = True,
) -> bool:
    if included_primary_types is not None:
        primary_type = _normalize_release_type(release_group.get("primary-type"))
        primary_filters, _ = normalize_release_type_filters(
            included_primary_types, None
        )
        if primary_type not in primary_filters:
            return False

    raw_secondary_types = release_group.get("secondary-types", []) or []
    secondary_types = (
        {_normalize_release_type(raw_secondary_types)}
        if isinstance(raw_secondary_types, str)
        else {
            normalized
            for value in raw_secondary_types
            if (normalized := _normalize_release_type(value))
        }
    )

    if included_secondary_types is None:
        if not apply_default_secondary_exclusions:
            return True
        exclude_types = {
            "compilation",
            "live",
            "remix",
            "soundtrack",
            "dj-mix",
            "mixtape/street",
            "demo",
        }
        return secondary_types.isdisjoint(exclude_types)

    _, secondary_filters = normalize_release_type_filters(
        None, included_secondary_types
    )
    if not secondary_types:
        return "studio" in secondary_filters
    return bool(secondary_types.intersection(secondary_filters))
