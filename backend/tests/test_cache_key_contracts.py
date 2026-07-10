"""Contract tests - every key function must produce keys that start with its prefix constant."""

import pytest

from infrastructure.cache.cache_keys import (
    GETIT_OPTIONS_PREFIX,
    LIBRARY_PREFIX,
    LIBRARY_REQUESTED_PREFIX,
    MB_ALBUM_SEARCH_PREFIX,
    MB_ARTIST_DETAIL_PREFIX,
    MB_ARTIST_SEARCH_PREFIX,
    MB_RELEASE_DETAIL_PREFIX,
    MB_RG_DETAIL_PREFIX,
    PREFERENCES_PREFIX,
    WIKIDATA_IMAGE_PREFIX,
    WIKIDATA_URL_PREFIX,
    WIKIPEDIA_PREFIX,
    getit_options_key,
    library_artist_mbids_key,
    library_albums_key,
    library_artists_key,
    library_grouped_key,
    library_mbids_key,
    library_raw_albums_key,
    library_requested_mbids_key,
    library_status_key,
    mb_album_search_key,
    mb_artist_detail_key,
    mb_artist_search_key,
    mb_release_group_key,
    mb_release_key,
    preferences_key,
    wikidata_artist_image_key,
    wikidata_url_key,
    wikipedia_extract_key,
)


@pytest.mark.parametrize(
    "generated_key, expected_prefix",
    [
        (mb_artist_search_key("test", 10, 0), MB_ARTIST_SEARCH_PREFIX),
        (mb_artist_detail_key("abc-123"), MB_ARTIST_DETAIL_PREFIX),
        (mb_album_search_key("test", 10, 0), MB_ALBUM_SEARCH_PREFIX),
        (mb_release_group_key("abc"), MB_RG_DETAIL_PREFIX),
        (mb_release_key("abc"), MB_RELEASE_DETAIL_PREFIX),
        (library_albums_key(), LIBRARY_PREFIX),
        (library_albums_key(include_unmonitored=True), LIBRARY_PREFIX),
        (library_artists_key(), LIBRARY_PREFIX),
        (library_mbids_key(), LIBRARY_PREFIX),
        (library_artist_mbids_key(), LIBRARY_PREFIX),
        (library_raw_albums_key(), LIBRARY_PREFIX),
        (library_grouped_key(), LIBRARY_PREFIX),
        (library_requested_mbids_key(), LIBRARY_REQUESTED_PREFIX),
        (library_status_key(), LIBRARY_PREFIX),
        (wikidata_artist_image_key("Q123"), WIKIDATA_IMAGE_PREFIX),
        (wikidata_url_key("artist-1"), WIKIDATA_URL_PREFIX),
        (wikipedia_extract_key("https://en.wikipedia.org/wiki/Test"), WIKIPEDIA_PREFIX),
        (preferences_key(), PREFERENCES_PREFIX),
        (getit_options_key("rg-1", "GB", True), GETIT_OPTIONS_PREFIX),
    ],
    ids=[
        "mb_artist_search",
        "mb_artist_detail",
        "mb_album_search",
        "mb_release_group",
        "mb_release",
        "library_albums_monitored",
        "library_albums_all",
        "library_artists",
        "library_mbids",
        "library_artist_mbids",
        "library_raw_albums",
        "library_grouped",
        "library_requested_mbids",
        "library_status",
        "wikidata_image",
        "wikidata_url",
        "wikipedia_extract",
        "preferences",
        "getit_options",
    ],
)
def test_key_starts_with_prefix(generated_key: str, expected_prefix: str):
    assert generated_key.startswith(expected_prefix), (
        f"Key {generated_key!r} does not start with prefix {expected_prefix!r}"
    )


@pytest.mark.parametrize(
    "group_fn",
    [
        pytest.param("musicbrainz_prefixes", id="musicbrainz"),
        pytest.param("listenbrainz_prefixes", id="listenbrainz"),
        pytest.param("lastfm_prefixes", id="lastfm"),
        pytest.param("home_prefixes", id="home"),
        pytest.param("getit_prefixes", id="getit"),
    ],
)
def test_invalidation_groups_return_list_of_strings(group_fn: str):
    from infrastructure.cache import cache_keys

    fn = getattr(cache_keys, group_fn)
    result = fn()
    assert isinstance(result, list)
    assert len(result) > 0, f"{group_fn}() returned an empty list"
    assert all(isinstance(p, str) for p in result), (
        f"{group_fn}() contains non-string entries"
    )
