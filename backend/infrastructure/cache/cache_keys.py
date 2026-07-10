"""Centralized cache key generation for consistent, sorted, testable cache keys."""
from typing import Optional


MB_ARTIST_SEARCH_PREFIX = "mb:artist:search:"
MB_ARTIST_DETAIL_PREFIX = "mb:artist:detail:"
MB_ALBUM_SEARCH_PREFIX = "mb:album:search:"
MB_RG_DETAIL_PREFIX = "mb:rg:detail:"
MB_RELEASE_DETAIL_PREFIX = "mb:release:detail:"
MB_RELEASE_TO_RG_PREFIX = "mb:release_to_rg:"
MB_RELEASE_REC_PREFIX = "mb:release_rec_positions:"
MB_RECORDING_PREFIX = "mb:recording:"
MB_RECORDING_SEARCH_PREFIX = "mb:recording:search:"
MB_RECORDING_TO_RG_PREFIX = "mb:recording_to_rg:"
MB_ARTIST_RELS_PREFIX = "mb:artist_rels:"
MB_ARTISTS_BY_TAG_PREFIX = "mb_artists_by_tag:"
MB_RG_BY_TAG_PREFIX = "mb_rg_by_tag:"

LB_PREFIX = "lb_"

LFM_PREFIX = "lfm_"

JELLYFIN_PREFIX = "jellyfin_"

NAVIDROME_PREFIX = "navidrome:"

PLEX_PREFIX = "plex:"

LIBRARY_PREFIX = "library:"
LIBRARY_REQUESTED_PREFIX = "library_requested"
LIBRARY_ARTIST_IMAGE_PREFIX = "library_artist_image:"
LIBRARY_ARTIST_DETAILS_PREFIX = "library_artist_details:"
LIBRARY_ARTIST_ALBUMS_PREFIX = "library_artist_albums:"
LIBRARY_ALBUM_IMAGE_PREFIX = "library_album_image:"
LIBRARY_ALBUM_DETAILS_PREFIX = "library_album_details:"
LIBRARY_ALBUM_TRACKS_PREFIX = "library_album_tracks:"
LIBRARY_TRACKFILE_PREFIX = "library_trackfile:"
LIBRARY_ALBUM_TRACKFILES_PREFIX = "library_album_trackfiles_raw:"

LOCAL_FILES_PREFIX = "local_files_"

HOME_RESPONSE_PREFIX = "home_response:"
DISCOVER_RESPONSE_PREFIX = "discover_response:"
GENRE_ARTIST_PREFIX = "genre_artist:"
GENRE_SECTION_PREFIX = "genre_section:"

SOURCE_RESOLUTION_PREFIX = "source_resolution"

ARTIST_INFO_PREFIX = "artist_info:"
ALBUM_INFO_PREFIX = "album_info:"

ARTIST_DISCOVERY_PREFIX = "artist_discovery:"
DISCOVER_QUEUE_ENRICH_PREFIX = "discover_queue_enrich:"

ARTIST_WIKIDATA_PREFIX = "artist_wikidata:"
WIKIDATA_IMAGE_PREFIX = "wikidata:image:"
WIKIDATA_URL_PREFIX = "wikidata:url:"
WIKIPEDIA_PREFIX = "wikipedia:extract:"

PREFERENCES_PREFIX = "preferences:"

GITHUB_RELEASES_PREFIX = "github:releases:"

AUDIODB_PREFIX = "audiodb_"

GETIT_OPTIONS_PREFIX = "getit:options:"
GETIT_ARTIST_OPTIONS_PREFIX = "getit:artist_options:"


def getit_prefixes() -> list[str]:
    """"Get it" purchase-option keys; swept when Get-it settings change."""
    return [GETIT_OPTIONS_PREFIX, GETIT_ARTIST_OPTIONS_PREFIX]


def getit_options_key(release_group_mbid: str, region: str, decorated: bool) -> str:
    return f"{GETIT_OPTIONS_PREFIX}{release_group_mbid}:{region}:{int(decorated)}"


def getit_artist_options_key(artist_mbid: str, decorated: bool) -> str:
    return f"{GETIT_ARTIST_OPTIONS_PREFIX}{artist_mbid}:{int(decorated)}"


def musicbrainz_prefixes() -> list[str]:
    """All MusicBrainz cache key prefixes for bulk invalidation."""
    return [
        MB_ARTIST_SEARCH_PREFIX,
        MB_ARTIST_DETAIL_PREFIX,
        MB_ALBUM_SEARCH_PREFIX,
        MB_RG_DETAIL_PREFIX,
        MB_RELEASE_DETAIL_PREFIX,
        MB_RELEASE_TO_RG_PREFIX,
        MB_RELEASE_REC_PREFIX,
        MB_RECORDING_PREFIX,
        MB_RECORDING_SEARCH_PREFIX,
        MB_RECORDING_TO_RG_PREFIX,
        MB_ARTIST_RELS_PREFIX,
        MB_ARTISTS_BY_TAG_PREFIX,
        MB_RG_BY_TAG_PREFIX,
    ]


def listenbrainz_prefixes() -> list[str]:
    return [LB_PREFIX]


def lastfm_prefixes() -> list[str]:
    return [LFM_PREFIX]


def home_prefixes() -> list[str]:
    """Cache prefixes cleared on home/discover invalidation."""
    return [HOME_RESPONSE_PREFIX, DISCOVER_RESPONSE_PREFIX, GENRE_ARTIST_PREFIX, GENRE_SECTION_PREFIX]


def _sort_params(**kwargs) -> str:
    """Sort parameters for consistent key generation."""
    return ":".join(f"{k}={v}" for k, v in sorted(kwargs.items()) if v is not None)


def mb_artist_search_key(query: str, limit: int, offset: int) -> str:
    return f"{MB_ARTIST_SEARCH_PREFIX}{query}:{limit}:{offset}"


def mb_album_search_key(
    query: str,
    limit: int,
    offset: int,
    included_secondary_types: Optional[set[str]] = None,
    included_primary_types: Optional[set[str]] = None,
) -> str:
    types_str = ",".join(sorted(included_secondary_types)) if included_secondary_types else "none"
    primary_str = ",".join(sorted(included_primary_types)) if included_primary_types else "none"
    return f"{MB_ALBUM_SEARCH_PREFIX}{query}:{limit}:{offset}:{types_str}:{primary_str}"


def mb_artist_detail_key(mbid: str) -> str:
    return f"{MB_ARTIST_DETAIL_PREFIX}{mbid}"


def mb_release_group_key(mbid: str, includes: Optional[list[str]] = None) -> str:
    includes_str = ",".join(sorted(includes)) if includes else "default"
    return f"{MB_RG_DETAIL_PREFIX}{mbid}:{includes_str}"


def mb_release_key(release_id: str, includes: Optional[list[str]] = None) -> str:
    includes_str = ",".join(sorted(includes)) if includes else "default"
    return f"{MB_RELEASE_DETAIL_PREFIX}{release_id}:{includes_str}"


def library_albums_key(include_unmonitored: bool = False) -> str:
    suffix = "all" if include_unmonitored else "monitored"
    return f"{LIBRARY_PREFIX}library:albums:{suffix}"


def library_artists_key(include_unmonitored: bool = False) -> str:
    suffix = "all" if include_unmonitored else "monitored"
    return f"{LIBRARY_PREFIX}library:artists:{suffix}"


def library_mbids_key(include_release_ids: bool = False) -> str:
    suffix = "with_releases" if include_release_ids else "albums_only"
    return f"{LIBRARY_PREFIX}library:mbids:{suffix}"


def library_artist_mbids_key() -> str:
    return f"{LIBRARY_PREFIX}artists:mbids"


def library_raw_albums_key() -> str:
    return f"{LIBRARY_PREFIX}raw:albums"


def library_grouped_key() -> str:
    return f"{LIBRARY_PREFIX}library:grouped"


def library_requested_mbids_key() -> str:
    return f"{LIBRARY_REQUESTED_PREFIX}_mbids"


def library_status_key() -> str:
    return f"{LIBRARY_PREFIX}status"


def wikidata_artist_image_key(wikidata_id: str) -> str:
    return f"{WIKIDATA_IMAGE_PREFIX}{wikidata_id}"


def wikidata_url_key(artist_id: str) -> str:
    return f"{WIKIDATA_URL_PREFIX}{artist_id}"


def wikipedia_extract_key(url: str) -> str:
    return f"{WIKIPEDIA_PREFIX}{url}"


def preferences_key() -> str:
    return f"{PREFERENCES_PREFIX}current"
