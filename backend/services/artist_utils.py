from typing import Any, Optional, Callable

from api.v1.schemas.artist import LifeSpan, ReleaseItem

_PLATFORM_PATTERNS: dict[str, tuple[str, str]] = {
    "instagram.com": ("Instagram", "social"),
    "twitter.com": ("Twitter", "social"),
    "x.com": ("Twitter", "social"),
    "facebook.com": ("Facebook", "social"),
    "youtube.com": ("YouTube", "music"),
    "youtu.be": ("YouTube", "music"),
    "spotify.com": ("Spotify", "music"),
    "deezer.com": ("Deezer", "music"),
    "apple.com/music": ("Apple Music", "music"),
    "music.apple.com": ("Apple Music", "music"),
    "tidal.com": ("Tidal", "music"),
    "amazon.com": ("Amazon", "music"),
    "bandcamp.com": ("Bandcamp", "music"),
    "soundcloud.com": ("SoundCloud", "music"),
    "last.fm": ("Last.fm", "info"),
    "lastfm.": ("Last.fm", "info"),
    "wikipedia.org": ("Wikipedia", "info"),
}

_LINK_TYPE_LABELS: dict[str, tuple[str, str]] = {
    "official homepage": ("Official Website", "info"),
    "wikipedia": ("Wikipedia", "info"),
    "last.fm": ("Last.fm", "info"),
    "bandcamp": ("Bandcamp", "music"),
    "youtube": ("YouTube", "music"),
    "soundcloud": ("SoundCloud", "music"),
    "instagram": ("Instagram", "social"),
    "twitter": ("Twitter", "social"),
    "facebook": ("Facebook", "social"),
}

_ALLOWED_LABELS = {
    "Spotify", "Apple Music", "YouTube", "Bandcamp", "SoundCloud",
    "Deezer", "Tidal", "Amazon",
    "Instagram", "Twitter", "Facebook",
    "Official Website", "Wikipedia", "Last.fm",
}


def detect_platform(url: str, rel_type: str) -> tuple[str, str]:
    """Return (label, category) for a URL + relation type."""
    url_lower = url.lower()
    for pattern, result in _PLATFORM_PATTERNS.items():
        if pattern in url_lower:
            return result
    return _LINK_TYPE_LABELS.get(rel_type, (rel_type.title(), "other"))


def extract_tags(mb_artist: dict[str, Any], limit: int = 10) -> list[str]:
    tags = []
    if mb_tags := mb_artist.get("tags", []):
        tags = list(dict.fromkeys(tag.get("name") for tag in mb_tags if tag.get("name")))[:limit]
    return tags


def extract_aliases(mb_artist: dict[str, Any], limit: int = 10) -> list[str]:
    aliases = []
    if mb_aliases := mb_artist.get("aliases", []):
        aliases = [
            alias.get("name")
            for alias in mb_aliases
            if alias.get("name")
        ][:limit]
    return aliases


def extract_life_span(mb_artist: dict[str, Any]) -> LifeSpan | None:
    if life_span := mb_artist.get("life-span"):
        ended = life_span.get("ended")
        return LifeSpan(
            begin=life_span.get("begin"),
            end=life_span.get("end"),
            ended=str(ended).lower() if ended is not None else None,
        )
    return None


def extract_external_links(mb_artist: dict[str, Any]) -> list[dict[str, str]]:
    external_links: list[dict[str, str]] = []
    seen_labels: set[str] = set()
    if url_rels := mb_artist.get("relations", []):
        for url_rel in url_rels:
            rel_type = url_rel.get("type", "")
            url_obj = url_rel.get("url", {})
            target_url = url_obj.get("resource", "") if isinstance(url_obj, dict) else ""
            if not target_url:
                continue
            label, category = detect_platform(target_url, rel_type)
            if label not in _ALLOWED_LABELS or label in seen_labels:
                continue
            external_links.append(
                {"type": rel_type, "url": target_url, "label": label, "category": category}
            )
            seen_labels.add(label)
    return external_links


def categorize_release_groups(
    mb_artist: dict[str, Any],
    album_mbids: set[str],
    included_primary_types: Optional[set[str]] = None,
    included_secondary_types: Optional[set[str]] = None,
    requested_mbids: Optional[set[str]] = None,
    monitored_mbids: Optional[set[str]] = None,
) -> tuple[list[dict[str, Any]], list[dict[str, Any]], list[dict[str, Any]]]:
    if included_primary_types is None:
        included_primary_types = {"album", "single", "ep", "broadcast", "other"}
    if requested_mbids is None:
        requested_mbids = set()
    if monitored_mbids is None:
        monitored_mbids = set()
    albums: list[ReleaseItem] = []
    singles: list[ReleaseItem] = []
    eps: list[ReleaseItem] = []
    if rg_list := mb_artist.get("release-group-list", []):
        for rg in rg_list:
            rg_id = rg.get("id")
            primary_type = (rg.get("primary-type") or "").lower()
            if primary_type not in included_primary_types:
                continue
            if included_secondary_types is not None:
                secondary_types = set(map(str.lower, rg.get("secondary-types", []) or []))
                if not secondary_types:
                    if "studio" not in included_secondary_types:
                        continue
                elif not secondary_types.intersection(included_secondary_types):
                    continue
            rg_id_lower = rg_id.lower() if rg_id else ""
            in_library = rg_id_lower in album_mbids if rg_id else False
            requested = rg_id_lower in requested_mbids if rg_id and not in_library else False
            is_monitored = rg_id_lower in monitored_mbids if rg_id and not in_library and not requested else False
            rg_data = ReleaseItem(
                id=rg_id,
                title=rg.get("title"),
                type=rg.get("primary-type"),
                first_release_date=rg.get("first-release-date"),
                in_library=in_library,
                requested=requested,
                monitored=is_monitored,
            )
            if date := rg_data.first_release_date:
                try:
                    rg_data.year = int(date.split("-")[0])
                except (ValueError, AttributeError):
                    pass
            if primary_type == "album":
                albums.append(rg_data)
            elif primary_type == "single":
                singles.append(rg_data)
            elif primary_type == "ep":
                eps.append(rg_data)
        for lst in [albums, singles, eps]:
            lst.sort(key=lambda x: (x.year is None, -(x.year or 0)))
    return albums, singles, eps


def categorize_lidarr_albums(
    lidarr_albums: list[dict[str, Any]],
    included_primary_types: set[str],
    included_secondary_types: set[str],
    library_cache_mbids: set[str] | None = None,
    requested_mbids: set[str] | None = None,
) -> tuple[list[ReleaseItem], list[ReleaseItem], list[ReleaseItem]]:
    albums: list[ReleaseItem] = []
    singles: list[ReleaseItem] = []
    eps: list[ReleaseItem] = []
    _cache_mbids = library_cache_mbids or set()
    _requested_mbids = requested_mbids or set()
    seen_mbids: set[str] = set()
    for album in lidarr_albums:
        album_type = (album.get("album_type") or "").lower()
        secondary_types = set(map(str.lower, album.get("secondary_types", []) or []))
        if album_type not in included_primary_types:
            continue
        if included_secondary_types:
            if not secondary_types:
                if "studio" not in included_secondary_types:
                    continue
            elif not secondary_types.intersection(included_secondary_types):
                continue
        mbid = album.get("mbid") or ""
        mbid_lower = mbid.lower() if mbid else ""
        # Skip albums without a stable MusicBrainz id and de-duplicate release
        # groups Lidarr can report more than once; both would otherwise surface as
        # duplicate keys when rendered (svelte each_key_duplicate). Mirrors the
        # MusicBrainz path's `if item.id and item.id not in seen_mbids` guard.
        if not mbid_lower or mbid_lower in seen_mbids:
            continue
        seen_mbids.add(mbid_lower)
        track_file_count = album.get("track_file_count", 0)
        in_library = track_file_count > 0 or (mbid_lower in _cache_mbids)
        requested = mbid_lower in _requested_mbids and not in_library
        is_monitored = album.get("monitored", False) and not in_library and not requested
        album_data = ReleaseItem(
            id=mbid,
            title=album.get("title"),
            type=album.get("album_type"),
            first_release_date=album.get("release_date"),
            year=album.get("year"),
            in_library=in_library,
            requested=requested,
            monitored=is_monitored,
        )
        if album_type == "album":
            albums.append(album_data)
        elif album_type == "single":
            singles.append(album_data)
        elif album_type == "ep":
            eps.append(album_data)
    for lst in [albums, singles, eps]:
        lst.sort(key=lambda x: (x.year is None, -(x.year or 0)))
    return albums, singles, eps


def extract_wiki_info(
    mb_artist: dict[str, Any],
    get_wikidata_id_fn: Callable[[str], Optional[str]]
) -> tuple[Optional[str], list[str]]:
    wikidata_id = None
    wiki_urls = []
    if url_rels := mb_artist.get("relations", []):
        for url_rel in url_rels:
            url_type = url_rel.get("type")
            url_obj = url_rel.get("url", {})
            wiki_url = url_obj.get("resource", "") if isinstance(url_obj, dict) else ""
            if not wiki_url:
                continue
            if url_type == "wikidata" and not wikidata_id:
                wikidata_id = get_wikidata_id_fn(wiki_url)
            if url_type in ("wikipedia", "wikidata"):
                wiki_urls.append(wiki_url)
    return wikidata_id, wiki_urls


def build_base_artist_info(
    mb_artist: dict[str, Any],
    artist_id: str,
    in_library: bool,
    tags: list[str],
    aliases: list[str],
    life_span: LifeSpan | None,
    external_links: list,
    albums: list[ReleaseItem],
    singles: list[ReleaseItem],
    eps: list[ReleaseItem],
    description: Optional[str] = None,
    image: Optional[str] = None,
    release_group_count: Optional[int] = None,
) -> dict[str, Any]:
    return {
        "name": mb_artist.get("name", "Unknown Artist"),
        "musicbrainz_id": artist_id,
        "disambiguation": mb_artist.get("disambiguation"),
        "type": mb_artist.get("type"),
        "country": mb_artist.get("country"),
        "life_span": life_span,
        "description": description,
        "image": image,
        "tags": tags,
        "aliases": aliases,
        "external_links": external_links,
        "in_library": in_library,
        "albums": albums,
        "singles": singles,
        "eps": eps,
        "release_group_count": release_group_count or mb_artist.get("release-group-count", 0),
    }
