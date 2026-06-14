"""Regression tests for categorize_lidarr_albums de-duplication.

Lidarr can report the same release group more than once (or with no
foreignAlbumId at all). When those reach the frontend they collide as
duplicate keys in the keyed {#each} that renders releases, which throws
svelte's each_key_duplicate and blanks the whole artist page. The
categoriser must therefore drop id-less and duplicate albums, mirroring the
MusicBrainz path's `if item.id and item.id not in seen_mbids` guard.
"""

from services.artist_utils import categorize_lidarr_albums

_PRIMARY = {"album", "single", "ep"}
_NO_SECONDARY_FILTER: set[str] = set()


def _album(mbid: str | None, title: str, album_type: str = "Album") -> dict:
    return {
        "mbid": mbid,
        "title": title,
        "album_type": album_type,
        "secondary_types": [],
        "release_date": "2020-01-01",
        "year": 2020,
        "monitored": False,
        "track_file_count": 0,
    }


def test_duplicate_mbids_are_deduplicated():
    albums, singles, eps = categorize_lidarr_albums(
        [
            _album("rg-1", "Greatest Hits"),
            _album("rg-1", "Greatest Hits (duplicate entry)"),
            _album("rg-2", "Second Album"),
        ],
        _PRIMARY,
        _NO_SECONDARY_FILTER,
    )

    assert [a.id for a in albums] == ["rg-1", "rg-2"]
    assert singles == []
    assert eps == []


def test_duplicate_mbids_are_case_insensitive():
    albums, _singles, _eps = categorize_lidarr_albums(
        [
            _album("RG-1", "Album"),
            _album("rg-1", "Album (lower-cased id)"),
        ],
        _PRIMARY,
        _NO_SECONDARY_FILTER,
    )

    assert len(albums) == 1
    assert albums[0].id == "RG-1"


def test_albums_without_mbid_are_skipped():
    albums, _singles, _eps = categorize_lidarr_albums(
        [
            _album(None, "No id at all"),
            _album("", "Empty id"),
            _album("rg-3", "Has id"),
        ],
        _PRIMARY,
        _NO_SECONDARY_FILTER,
    )

    assert [a.id for a in albums] == ["rg-3"]


def test_distinct_albums_across_types_are_all_kept():
    albums, singles, eps = categorize_lidarr_albums(
        [
            _album("rg-a", "An Album", album_type="Album"),
            _album("rg-s", "A Single", album_type="Single"),
            _album("rg-e", "An EP", album_type="EP"),
        ],
        _PRIMARY,
        _NO_SECONDARY_FILTER,
    )

    assert [a.id for a in albums] == ["rg-a"]
    assert [s.id for s in singles] == ["rg-s"]
    assert [e.id for e in eps] == ["rg-e"]
