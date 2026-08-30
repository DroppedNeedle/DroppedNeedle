"""Centralized cache key generation for consistent, sorted, testable cache keys."""

import hashlib

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
MB_ARTIST_RGS_PREFIX = "mb:artist_rgs:"
MB_ARTISTS_BY_TAG_PREFIX = "mb_artists_by_tag:"
MB_RG_BY_TAG_PREFIX = "mb_rg_by_tag:"
MB_URL_RESOLUTION_PREFIX = "mb:url:resolution:"
MB_RELEASE_VERIFY_PREFIX = "mb:release:verify:"
MB_DUPLICATE_SEARCH_PREFIX = "mb:release:duplicate-search:"
MB_RELEASE_EDITION_SEARCH_PREFIX = "mb:release:edition-search:"
MB_MANAGEMENT_RELEASE_PREFIX = "mb:management:release:"
CAA_MANAGEMENT_PREFIX = "caa:management:"

LB_PREFIX = "lb_"

LFM_PREFIX = "lfm_"

LRCLIB_PREFIX = "lrclib:"

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
LIBRARY_POLICY_TREE_PREFIX = "library_policy_tree:"
LIBRARY_POLICY_IMPACT_PREFIX = "library_policy_impact:"
LIBRARY_REVIEW_PREFIX = "library_review:"
LIBRARY_IDENTIFICATION_PREFIX = "library_identification:"
COMPAT_LIBRARY_PREFIX = "compat_library:"

LOCAL_FILES_PREFIX = "local_files_"

HOME_RESPONSE_PREFIX = "home_response:"
DISCOVER_RESPONSE_PREFIX = "discover_response:"
DAILY_MIX_PREFIX = "daily_mix:"
TOP_PICKS_PREFIX = "top_picks:"
GENRE_ARTIST_PREFIX = "genre_artist:"
GENRE_SECTION_PREFIX = "genre_section:"
GENRE_ARTWORK_PREFIX = "genre_artwork:v2:"

SOURCE_RESOLUTION_PREFIX = "source_resolution"

ARTIST_INFO_PREFIX = "artist_info:"
ALBUM_INFO_PREFIX = "album_info:"
ALBUM_TRACKS_INFO_PREFIX = "album_tracks_info:"

ARTIST_DISCOVERY_PREFIX = "artist_discovery:"
ARTIST_DISCOVERY_TOP_SONGS_PREFIX = f"{ARTIST_DISCOVERY_PREFIX}top_songs:"
ARTIST_DISCOVERY_TOP_ALBUMS_PREFIX = f"{ARTIST_DISCOVERY_PREFIX}top_albums:"
DISCOVER_QUEUE_ENRICH_PREFIX = "discover_queue_enrich:"

ARTIST_WIKIDATA_PREFIX = "artist_wikidata:"
WIKIDATA_IMAGE_PREFIX = "wikidata:image:"
WIKIDATA_URL_PREFIX = "wikidata:url:"
WIKIPEDIA_PREFIX = "wikipedia:extract:"

PREFERENCES_PREFIX = "preferences:"

GITHUB_RELEASES_PREFIX = "github:releases:"

AUDIODB_PREFIX = "audiodb_"

DISCOGS_RELEASE_PREFIX = "discogs:release:"
DISCOGS_SEARCH_PREFIX = "discogs:search:"

GETIT_OPTIONS_PREFIX = "getit:v2:options:"
GETIT_ARTIST_OPTIONS_PREFIX = "getit:v2:artist_options:"


def _mbid_key(value: str) -> str:
    """Canonicalize a MusicBrainz entity ID for cache identity only."""
    return str(value).strip().casefold()


def library_policy_prefixes() -> list[str]:
    return [LIBRARY_POLICY_TREE_PREFIX, LIBRARY_POLICY_IMPACT_PREFIX]


def library_identification_prefixes() -> list[str]:
    # ST1 note: LIBRARY_ARTIST_IMAGE_PREFIX / LIBRARY_ALBUM_IMAGE_PREFIX were
    # removed - repo-wide grep found zero writers (only sweeps), so sweeping
    # them was dead weight. Restore alongside a real writer if one appears.
    return [
        LIBRARY_PREFIX,
        LIBRARY_ARTIST_DETAILS_PREFIX,
        LIBRARY_ARTIST_ALBUMS_PREFIX,
        LIBRARY_ALBUM_DETAILS_PREFIX,
        LIBRARY_ALBUM_TRACKS_PREFIX,
        LIBRARY_TRACKFILE_PREFIX,
        LIBRARY_ALBUM_TRACKFILES_PREFIX,
        LIBRARY_REVIEW_PREFIX,
        LIBRARY_IDENTIFICATION_PREFIX,
        HOME_RESPONSE_PREFIX,
        DISCOVER_RESPONSE_PREFIX,
        DAILY_MIX_PREFIX,
        TOP_PICKS_PREFIX,
        GENRE_ARTIST_PREFIX,
        GENRE_SECTION_PREFIX,
        GENRE_ARTWORK_PREFIX,
        COMPAT_LIBRARY_PREFIX,
        LOCAL_FILES_PREFIX,
        ARTIST_INFO_PREFIX,
        ALBUM_INFO_PREFIX,
        ALBUM_TRACKS_INFO_PREFIX,
        ARTIST_DISCOVERY_PREFIX,
        DISCOVER_QUEUE_ENRICH_PREFIX,
        SOURCE_RESOLUTION_PREFIX,
    ]


def catalog_entity_prefixes() -> list[str]:
    """ST1: the slice of library_identification_prefixes() whose keys are
    shaped ``{prefix}{mbid}`` and therefore deletable per entity. Shape
    verified against every remaining delete/write site (e.g.
    album_service refresh_album, service_providers import hook); MBID values
    never contain ':', so the concatenation is unambiguous."""
    return [
        ARTIST_INFO_PREFIX,
        ALBUM_INFO_PREFIX,
        ALBUM_TRACKS_INFO_PREFIX,
        LIBRARY_ARTIST_DETAILS_PREFIX,
        LIBRARY_ARTIST_ALBUMS_PREFIX,
        LIBRARY_ALBUM_DETAILS_PREFIX,
    ]


def catalog_list_prefixes() -> list[str]:
    """ST1 complement: cheap locally-rebuilt snapshots plus the legacy
    LIBRARY_ALBUM_TRACKS/trackfile prefixes (kept bulk because their writers
    could not be confirmed to use bare ``{id}`` key shapes - see grep notes in
    the ST1 ledger entry). Partition of library_identification_prefixes()
    once the two dead image prefixes are excluded."""
    entity = set(catalog_entity_prefixes())
    return [
        prefix for prefix in library_identification_prefixes() if prefix not in entity
    ]


def getit_prefixes() -> list[str]:
    """ "Get it" purchase-option keys; swept when Get-it settings change."""
    return [GETIT_OPTIONS_PREFIX, GETIT_ARTIST_OPTIONS_PREFIX]


def getit_options_key(release_group_mbid: str, region: str) -> str:
    return f"{GETIT_OPTIONS_PREFIX}{release_group_mbid}:{region}"


def getit_artist_options_key(artist_mbid: str) -> str:
    return f"{GETIT_ARTIST_OPTIONS_PREFIX}{artist_mbid}"


def musicbrainz_prefixes() -> list[str]:
    """MusicBrainz source-dependent cache prefixes for bulk invalidation."""
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
        MB_ARTIST_RGS_PREFIX,
        MB_ARTISTS_BY_TAG_PREFIX,
        MB_RG_BY_TAG_PREFIX,
        MB_URL_RESOLUTION_PREFIX,
        MB_RELEASE_VERIFY_PREFIX,
        MB_DUPLICATE_SEARCH_PREFIX,
        MB_RELEASE_EDITION_SEARCH_PREFIX,
        MB_MANAGEMENT_RELEASE_PREFIX,
        # Provider-bearing composite responses must cold-clear with the
        # endpoint-specific MusicBrainz entries on a source change.
        ARTIST_INFO_PREFIX,
        HOME_RESPONSE_PREFIX,
        DISCOVER_RESPONSE_PREFIX,
        ALBUM_INFO_PREFIX,
        ALBUM_TRACKS_INFO_PREFIX,
        DISCOVER_QUEUE_ENRICH_PREFIX,
        ARTIST_DISCOVERY_TOP_SONGS_PREFIX,
        ARTIST_DISCOVERY_TOP_ALBUMS_PREFIX,
    ]


def mb_release_edition_search_key(
    title: str, artist: str, limit: int, offset: int
) -> str:
    normalized = "\x00".join(
        " ".join(value.casefold().split()) for value in (title, artist)
    )
    digest = hashlib.sha256(normalized.encode("utf-8")).hexdigest()
    return f"{MB_RELEASE_EDITION_SEARCH_PREFIX}{digest}:{limit}:{offset}"


def mb_recording_canonical_id_key(recording_mbid: str) -> str:
    return f"{MB_RECORDING_PREFIX}{_mbid_key(recording_mbid)}:canonical-id"


def listenbrainz_prefixes() -> list[str]:
    return [LB_PREFIX]


def listenbrainz_management_genres_key(release_group_mbid: str) -> str:
    return f"{LB_PREFIX}management:release-group-genres:{release_group_mbid}"


def coverart_prefixes() -> list[str]:
    return [CAA_MANAGEMENT_PREFIX]


def coverart_management_key(entity_kind: str, mbid: str, download_size: str) -> str:
    return f"{CAA_MANAGEMENT_PREFIX}{entity_kind}:{mbid}:{download_size}"


def discogs_prefixes() -> list[str]:
    return [DISCOGS_RELEASE_PREFIX, DISCOGS_SEARCH_PREFIX]


def discogs_release_key(release_id: str) -> str:
    return f"{DISCOGS_RELEASE_PREFIX}{release_id}"


def discogs_search_key(query: str, limit: int) -> str:
    normalized = " ".join(query.casefold().split())
    return f"{DISCOGS_SEARCH_PREFIX}{normalized}:{limit}"


def lastfm_prefixes() -> list[str]:
    return [LFM_PREFIX]


def _lastfm_management_key(kind: str, *values: str) -> str:
    normalized = "\x00".join(" ".join(value.casefold().split()) for value in values)
    digest = hashlib.sha256(normalized.encode()).hexdigest()
    return f"{LFM_PREFIX}management:{kind}:{digest}"


def lastfm_management_album_genres_key(artist_name: str, album_title: str) -> str:
    return _lastfm_management_key("album-genres", artist_name, album_title)


def lastfm_management_artist_genres_key(artist_name: str) -> str:
    return _lastfm_management_key("artist-genres", artist_name)


def lrclib_exact_lyrics_key(
    track_name: str,
    artist_name: str,
    album_name: str,
    duration_seconds: int,
) -> str:
    normalized = "\x00".join(
        " ".join(value.casefold().split())
        for value in (track_name, artist_name, album_name)
    )
    digest = hashlib.sha256(
        f"{normalized}\x00{duration_seconds}".encode("utf-8")
    ).hexdigest()
    return f"{LRCLIB_PREFIX}exact:{digest}"


def lrclib_prefixes() -> list[str]:
    return [LRCLIB_PREFIX]


def home_prefixes() -> list[str]:
    """Cache prefixes cleared on home/discover invalidation."""
    return [
        HOME_RESPONSE_PREFIX,
        DISCOVER_RESPONSE_PREFIX,
        DAILY_MIX_PREFIX,
        TOP_PICKS_PREFIX,
        GENRE_ARTIST_PREFIX,
        GENRE_SECTION_PREFIX,
        GENRE_ARTWORK_PREFIX,
    ]


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
    types_str = (
        ",".join(sorted(included_secondary_types))
        if included_secondary_types
        else "none"
    )
    primary_str = (
        ",".join(sorted(included_primary_types)) if included_primary_types else "none"
    )
    return f"{MB_ALBUM_SEARCH_PREFIX}{query}:{limit}:{offset}:{types_str}:{primary_str}"


def mb_artist_detail_key(mbid: str, *, include_releases: bool = True) -> str:
    suffix = "" if include_releases else ":basic"
    return f"{MB_ARTIST_DETAIL_PREFIX}{_mbid_key(mbid)}{suffix}"


def mb_artist_release_groups_key(artist_mbid: str) -> str:
    return f"{MB_ARTIST_RGS_PREFIX}{_mbid_key(artist_mbid)}"


def mb_artist_rgs_browse_key(artist_mbid: str, limit: int) -> str:
    """Key for the single-page artist->release-groups browse (QW1).

    Extends MB_ARTIST_RGS_PREFIX, so it is swept by musicbrainz_prefixes()
    consumers without any new prefix-list membership. Collision-safe: the
    sibling builder above stores bare casefolded MBIDs and MBIDs never
    contain ':', so the ':browse:' segment cannot collide.
    """
    return f"{MB_ARTIST_RGS_PREFIX}{_mbid_key(artist_mbid)}:browse:{limit}"


def mb_release_group_key(mbid: str, includes: Optional[list[str]] = None) -> str:
    includes_str = ",".join(sorted(includes)) if includes else "default"
    return f"{MB_RG_DETAIL_PREFIX}{_mbid_key(mbid)}:{includes_str}"


def mb_release_key(release_id: str, includes: Optional[list[str]] = None) -> str:
    includes_str = ",".join(sorted(includes)) if includes else "default"
    return f"{MB_RELEASE_DETAIL_PREFIX}{_mbid_key(release_id)}:{includes_str}"


def mb_management_release_key(
    release_id: str,
    includes: tuple[str, ...],
    preferred_locales: tuple[str, ...],
    artist_standardization: str,
) -> str:
    includes_part = ",".join(sorted(set(includes)))
    locales_part = ",".join(locale.strip().casefold() for locale in preferred_locales)
    standardization_part = artist_standardization.strip().casefold()
    return (
        f"{MB_MANAGEMENT_RELEASE_PREFIX}{_mbid_key(release_id)}:"
        f"inc={includes_part}:locales={locales_part}:artists={standardization_part}"
    )


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
