"""JSON-safe round-trip for ``DiscoverResponse`` (persistent discover cache).

Encoding is plain ``msgspec.to_builtins``. Decoding needs care because
``HomeSection.items`` is an UNTAGGED union (``HomeArtist | HomeAlbum | HomeTrack |
HomeGenre``) which msgspec cannot decode directly - the section's ``type`` field
("artists"/"albums"/"tracks"/"genres") is the discriminator, so each section's items
are converted against the concrete type it declares.

Decoding is defensive throughout: a corrupt or schema-drifted payload yields ``None``
(callers fall back to a normal rebuild) rather than an exception.
"""

from __future__ import annotations

import logging
from typing import Any

import msgspec

from api.v1.schemas.discover import (
    BecauseYouListenTo,
    DiscoverIntegrationStatus,
    DiscoverResponse,
    TopPicksSection,
)
from api.v1.schemas.home import (
    HomeAlbum,
    HomeArtist,
    HomeGenre,
    HomeSection,
    HomeTrack,
    ServicePrompt,
)
from api.v1.schemas.weekly_exploration import WeeklyExplorationSection

logger = logging.getLogger(__name__)

_ITEM_TYPES: dict[str, type] = {
    "artists": HomeArtist,
    "albums": HomeAlbum,
    "tracks": HomeTrack,
    "genres": HomeGenre,
}

# every DiscoverResponse field typed ``HomeSection | None``
_SECTION_FIELDS = (
    "fresh_releases",
    "missing_essentials",
    "rediscover",
    "artists_you_might_like",
    "popular_in_your_genres",
    "genre_list",
    "globally_trending",
    "lastfm_weekly_artist_chart",
    "lastfm_weekly_album_chart",
    "lastfm_recent_scrobbles",
    "listeners_like_you",
    "anniversaries",
    "new_from_followed",
    "unexplored_genres",
)


def encode_discover_response(response: DiscoverResponse) -> dict[str, Any]:
    return msgspec.to_builtins(response)


def _opt_str(value: Any) -> str | None:
    return value if isinstance(value, str) else None


def _decode_section(raw: Any) -> HomeSection | None:
    # HomeSection itself carries the untagged union, so it is built by hand; only the
    # concrete item structs go through msgspec.convert.
    if not isinstance(raw, dict):
        return None
    section_type = str(raw.get("type") or "")
    section = HomeSection(
        title=str(raw.get("title") or ""),
        type=section_type,
        source=_opt_str(raw.get("source")),
        fallback_message=_opt_str(raw.get("fallback_message")),
        connect_service=_opt_str(raw.get("connect_service")),
        radio_seed_type=_opt_str(raw.get("radio_seed_type")),
        radio_seed_id=_opt_str(raw.get("radio_seed_id")),
    )
    item_type = _ITEM_TYPES.get(section_type)
    if item_type is None:
        return section
    items: list[Any] = []
    for item_raw in raw.get("items") or []:
        try:
            items.append(msgspec.convert(item_raw, type=item_type, strict=False))
        except (msgspec.ValidationError, TypeError, ValueError):
            continue
    section.items = items
    return section


def _decode_because(raw: Any) -> BecauseYouListenTo | None:
    if not isinstance(raw, dict):
        return None
    section = _decode_section(raw.get("section"))
    if section is None:
        return None
    return BecauseYouListenTo(
        seed_artist=str(raw.get("seed_artist") or ""),
        seed_artist_mbid=str(raw.get("seed_artist_mbid") or ""),
        section=section,
        listen_count=int(raw.get("listen_count") or 0),
        banner_url=raw.get("banner_url"),
        wide_thumb_url=raw.get("wide_thumb_url"),
        fanart_url=raw.get("fanart_url"),
    )


def _convert_opt(raw: Any, target: type) -> Any:
    if not isinstance(raw, dict):
        return None
    try:
        return msgspec.convert(raw, type=target, strict=False)
    except (msgspec.ValidationError, TypeError, ValueError):
        return None


def _str_map(raw: Any) -> dict[str, str | None]:
    if not isinstance(raw, dict):
        return {}
    return {
        str(k): (str(v) if isinstance(v, str) else None)
        for k, v in raw.items()
    }


def decode_discover_response(payload: Any) -> DiscoverResponse | None:
    # The DiscoverResponse type as a whole is NOT msgspec-decodable (its sections carry
    # the untagged items union), so it is reassembled field by field: union-free fields
    # via msgspec.convert, sections via the type-discriminated decoder above.
    if not isinstance(payload, dict):
        return None
    try:
        service_status = payload.get("service_status")
        response = DiscoverResponse(
            discover_queue_enabled=bool(payload.get("discover_queue_enabled", True)),
            genre_artists=_str_map(payload.get("genre_artists")),
            genre_artist_images=_str_map(payload.get("genre_artist_images")),
            service_status=service_status if isinstance(service_status, dict) else None,
            refreshing=False,
        )
        for field in _SECTION_FIELDS:
            setattr(response, field, _decode_section(payload.get(field)))
        response.daily_mixes = [
            s for s in map(_decode_section, payload.get("daily_mixes") or []) if s is not None
        ]
        response.radio_sections = [
            s for s in map(_decode_section, payload.get("radio_sections") or []) if s is not None
        ]
        response.because_you_listen_to = [
            b for b in map(_decode_because, payload.get("because_you_listen_to") or [])
            if b is not None
        ]
        response.top_picks = _convert_opt(payload.get("top_picks"), TopPicksSection)
        response.weekly_exploration = _convert_opt(
            payload.get("weekly_exploration"), WeeklyExplorationSection
        )
        response.integration_status = _convert_opt(
            payload.get("integration_status"), DiscoverIntegrationStatus
        )
        response.service_prompts = [
            p for p in (
                _convert_opt(raw, ServicePrompt)
                for raw in payload.get("service_prompts") or []
            )
            if p is not None
        ]
        return response
    except Exception as e:  # noqa: BLE001 - a bad payload means "no persisted cache"
        logger.debug("Failed to decode persisted discover response: %s", e)
        return None
