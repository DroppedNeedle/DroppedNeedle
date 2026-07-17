from typing import Any

import msgspec

from api.v1.schemas.discover import (
    BecauseYouListenTo,
    DiscoverIntegrationStatus,
    DiscoverQueueEnrichment,
    DiscoverQueueItemFull,
    DiscoverQueueItemLight,
    DiscoverQueueResponse,
    DiscoverResponse,
    TopPickItem,
    TopPicksSection,
)
from api.v1.schemas.home import (
    GenreArtwork,
    HomeAlbum,
    HomeArtist,
    HomeGenre,
    HomeSection,
    HomeTrack,
    ServicePrompt,
)
from api.v1.schemas.weekly_exploration import WeeklyExplorationSection


_HOME_SECTION_FIELDS = (
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


def _convert(value: Any, target: Any) -> Any:
    return msgspec.convert(value, type=target, strict=False)


def _decode_home_item(value: dict[str, Any], section_type: str) -> Any:
    normalized = section_type.lower()
    if "genre" in normalized or "artist_count" in value:
        return _convert(value, HomeGenre)
    if "track" in normalized or "album_name" in value or "listened_at" in value:
        return _convert(value, HomeTrack)
    if "album" in normalized or "artist_name" in value or "release_date" in value:
        return _convert(value, HomeAlbum)
    return _convert(value, HomeArtist)


def _decode_home_section(value: dict[str, Any] | None) -> HomeSection | None:
    if value is None:
        return None
    section_type = str(value.get("type", ""))
    return HomeSection(
        title=str(value.get("title", "")),
        type=section_type,
        items=[
            _decode_home_item(item, section_type)
            for item in value.get("items", [])
            if isinstance(item, dict)
        ],
        source=value.get("source"),
        fallback_message=value.get("fallback_message"),
        connect_service=value.get("connect_service"),
        radio_seed_type=value.get("radio_seed_type"),
        radio_seed_id=value.get("radio_seed_id"),
    )


def decode_discover_response(payload: bytes) -> DiscoverResponse:
    raw = msgspec.json.decode(payload)
    if not isinstance(raw, dict):
        raise ValueError("Discover snapshot is not an object")

    response = DiscoverResponse(
        discover_queue_enabled=bool(raw.get("discover_queue_enabled", True)),
        genre_artwork_schema_version=str(raw.get("genre_artwork_schema_version", "v2")),
        generated_at=raw.get("generated_at"),
        refresh_started_at=raw.get("refresh_started_at"),
        section_status=dict(raw.get("section_status") or {}),
        refreshing=bool(raw.get("refreshing", False)),
        service_status=raw.get("service_status"),
    )
    for name in _HOME_SECTION_FIELDS:
        setattr(response, name, _decode_home_section(raw.get(name)))

    response.because_you_listen_to = [
        BecauseYouListenTo(
            seed_artist=str(item.get("seed_artist", "")),
            seed_artist_mbid=str(item.get("seed_artist_mbid", "")),
            section=_decode_home_section(item.get("section"))
            or HomeSection(title="", type="artists"),
            listen_count=int(item.get("listen_count") or 0),
            banner_url=item.get("banner_url"),
            wide_thumb_url=item.get("wide_thumb_url"),
            fanart_url=item.get("fanart_url"),
        )
        for item in raw.get("because_you_listen_to", [])
        if isinstance(item, dict)
    ]
    response.daily_mixes = [
        section
        for value in raw.get("daily_mixes", [])
        if isinstance(value, dict)
        if (section := _decode_home_section(value)) is not None
    ]
    response.radio_sections = [
        section
        for value in raw.get("radio_sections", [])
        if isinstance(value, dict)
        if (section := _decode_home_section(value)) is not None
    ]

    weekly = raw.get("weekly_exploration")
    if weekly is not None:
        response.weekly_exploration = _convert(weekly, WeeklyExplorationSection)
    integration = raw.get("integration_status")
    if integration is not None:
        response.integration_status = _convert(integration, DiscoverIntegrationStatus)
    response.service_prompts = [
        _convert(item, ServicePrompt)
        for item in raw.get("service_prompts", [])
        if isinstance(item, dict)
    ]
    response.genre_artwork = {
        str(name): _convert(value, GenreArtwork)
        for name, value in (raw.get("genre_artwork") or {}).items()
    }

    top_picks = raw.get("top_picks")
    if isinstance(top_picks, dict):
        response.top_picks = TopPicksSection(
            title=str(top_picks.get("title", "Top Picks for You")),
            items=[
                TopPickItem(
                    album=_convert(item.get("album"), HomeAlbum),
                    match_pct=int(item.get("match_pct") or 0),
                    reasons=list(item.get("reasons") or []),
                    seed_artist=item.get("seed_artist"),
                )
                for item in top_picks.get("items", [])
                if isinstance(item, dict) and isinstance(item.get("album"), dict)
            ],
            source=top_picks.get("source"),
            personalizing=bool(top_picks.get("personalizing", False)),
        )
    return response


def decode_discover_queue(payload: bytes) -> tuple[DiscoverQueueResponse, float]:
    raw = msgspec.json.decode(payload)
    if not isinstance(raw, dict) or not isinstance(raw.get("queue"), dict):
        raise ValueError("Discover queue snapshot is not an object")
    queue_raw = raw["queue"]
    items: list[DiscoverQueueItemLight | DiscoverQueueItemFull] = []
    for item in queue_raw.get("items", []):
        if not isinstance(item, dict):
            continue
        if item.get("enrichment") is not None:
            enrichment = _convert(item["enrichment"], DiscoverQueueEnrichment)
            base = {key: value for key, value in item.items() if key != "enrichment"}
            items.append(DiscoverQueueItemFull(**base, enrichment=enrichment))
        else:
            items.append(_convert(item, DiscoverQueueItemLight))
    return (
        DiscoverQueueResponse(items=items, queue_id=str(queue_raw.get("queue_id", ""))),
        float(raw.get("built_at") or 0.0),
    )
