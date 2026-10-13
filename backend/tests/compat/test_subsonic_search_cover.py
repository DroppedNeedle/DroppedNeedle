"""T1.4 - Subsonic search3/search2 + getCoverArt prefix resolution."""

import json

import pytest

from api.v1.schemas.advanced_settings import AdvancedSettings
from services.compat.view_models import ViewAlbum, ViewArtist, ViewTrack

pytestmark = pytest.mark.asyncio


def _sub(resp):
    assert resp.status_code == 200
    return json.loads(resp.content)["subsonic-response"]


def _get(env, endpoint, **extra):
    q = {"v": "1.16.1", "c": "pytest", "f": "json", "apiKey": env.secret, **extra}
    return env.client.get(f"/subsonic/rest/{endpoint}", params=q)


async def test_search3_finds_artist_album_song(compat_env):
    body = _sub(_get(compat_env, "search3", query="Radiohead"))
    res = body["searchResult3"]
    assert any(a["name"] == "Radiohead" for a in res.get("artist", []))
    assert any(a["name"] == "OK Computer" for a in res.get("album", []))
    assert res.get("song")  # tracks by Radiohead


async def test_search3_empty_query_pages_everything(compat_env):
    body = _sub(_get(compat_env, "search3", query=""))
    res = body["searchResult3"]
    assert len(res["artist"]) == 1
    assert len(res["album"]) == 1
    assert len(res["song"]) == 2


async def test_search3_missing_query_pages_everything(compat_env):
    # Arpeggi's "Songs" view sends search3 with songCount but no query param; a missing
    # query must match everything (not error 10) exactly like an empty query.
    res = _sub(_get(compat_env, "search3", songCount="100"))["searchResult3"]
    assert len(res["song"]) == 2


async def test_search3_literal_quoted_empty_query_pages_everything(compat_env):
    # Symfonium's full-library sync sends query=%22%22 - the literal two-character
    # string '""' (sentriz/gonic#229 request logs). Treating it as a search term
    # matched nothing and native-mode sync came back empty (issue #129).
    res = _sub(_get(compat_env, "search3", query='""', songCount="100"))["searchResult3"]
    assert len(res["artist"]) == 1
    assert len(res["album"]) == 1
    assert len(res["song"]) == 2


async def test_search3_quoted_term_still_matches(compat_env):
    res = _sub(_get(compat_env, "search3", query='"Radiohead"'))["searchResult3"]
    assert any(a["name"] == "Radiohead" for a in res.get("artist", []))


async def test_search2_file_structure(compat_env):
    res = _sub(_get(compat_env, "search2", query=""))["searchResult2"]
    assert res["album"][0]["isDir"] is True
    assert res["song"][0]["isDir"] is False


async def test_search3_counts_respected(compat_env):
    res = _sub(_get(compat_env, "search3", query="", artistCount="0", albumCount="0"))[
        "searchResult3"
    ]
    assert "artist" not in res or res["artist"] == []
    assert "album" not in res or res["album"] == []
    assert len(res["song"]) == 2


def _large_search_library(size: int = 2_000):
    """Independent collections make each search count/offset contract observable."""
    artists = [
        ViewArtist(artist_mbid=f"artist-{index:04d}", name=f"Artist {index:04d}")
        for index in range(size)
    ]
    albums = [
        ViewAlbum(
            rg_mbid=f"album-{index:04d}",
            title=f"Album {index:04d}",
            artist_name=f"Artist {index:04d}",
        )
        for index in range(size)
    ]
    tracks = [
        ViewTrack(
            file_id=f"track-{index:04d}",
            title=f"Track {index:04d}",
            album_title=f"Album {index:04d}",
            artist_name=f"Artist {index:04d}",
        )
        for index in range(size)
    ]
    return artists, albums, tracks


async def _paged(items, *, limit: int, offset: int, **_kwargs):
    return items[offset : offset + limit], len(items)


@pytest.mark.parametrize(
    ("endpoint", "result_key"),
    [("search3", "searchResult3"), ("search2", "searchResult2")],
)
async def test_search_honors_1000_item_pages_without_overlap(
    compat_env, endpoint: str, result_key: str
):
    """Advancing each offset by 1000 must not skip or repeat any records."""
    artists, albums, tracks = _large_search_library()

    async def get_artists(**kwargs):
        return await _paged(artists, **kwargs)

    async def get_albums_offset(**kwargs):
        return await _paged(albums, **kwargs)

    async def get_tracks_page(**kwargs):
        return await _paged(tracks, **kwargs)

    compat_env.view.get_artists = get_artists
    compat_env.view.get_albums_offset = get_albums_offset
    compat_env.view.get_tracks_page = get_tracks_page

    pages = []
    for offset in (0, 1_000):
        pages.append(
            _sub(
                _get(
                    compat_env,
                    endpoint,
                    query="",
                    artistCount="1000",
                    artistOffset=str(offset),
                    albumCount="1000",
                    albumOffset=str(offset),
                    songCount="1000",
                    songOffset=str(offset),
                )
            )[result_key]
        )

    for collection, prefix in (
        ("artist", "ar-artist-"),
        ("album", "al-album-"),
        ("song", "tr-track-"),
    ):
        ids = [item["id"] for page in pages for item in page[collection]]
        assert ids == [f"{prefix}{index:04d}" for index in range(2_000)]


@pytest.mark.parametrize("field", ["artistCount", "albumCount", "songCount"])
@pytest.mark.parametrize("value", ["1001", "-1", "not-an-integer"])
async def test_search_rejects_counts_outside_configured_ceiling(
    compat_env, field: str, value: str
):
    body = _sub(_get(compat_env, "search3", query="", **{field: value}))
    assert body["error"]["code"] == 10
    assert body["error"]["message"] == f"Invalid parameter '{field}'"


async def test_search_uses_persisted_custom_page_ceiling(compat_env):
    artists, _albums, _tracks = _large_search_library()

    async def get_artists(**kwargs):
        return await _paged(artists, **kwargs)

    compat_env.view.get_artists = get_artists
    compat_env.preferences.save_advanced_settings(
        AdvancedSettings(subsonic_search_page_limit=1_200)
    )

    result = _sub(
        _get(
            compat_env,
            "search3",
            query="",
            artistCount="1200",
            albumCount="0",
            songCount="0",
        )
    )["searchResult3"]
    assert [artist["id"] for artist in result["artist"]] == [
        f"ar-artist-{index:04d}" for index in range(1_200)
    ]


@pytest.mark.parametrize(
    ("endpoint", "result_key"),
    [("search3", "searchResult3"), ("search2", "searchResult2")],
)
async def test_search_omitted_counts_respect_small_persisted_ceiling(
    compat_env, endpoint: str, result_key: str
):
    artists, albums, tracks = _large_search_library()

    async def get_artists(**kwargs):
        return await _paged(artists, **kwargs)

    async def get_albums_offset(**kwargs):
        return await _paged(albums, **kwargs)

    async def get_tracks_page(**kwargs):
        return await _paged(tracks, **kwargs)

    compat_env.view.get_artists = get_artists
    compat_env.view.get_albums_offset = get_albums_offset
    compat_env.view.get_tracks_page = get_tracks_page
    compat_env.preferences.save_advanced_settings(
        AdvancedSettings(subsonic_search_page_limit=5)
    )

    result = _sub(_get(compat_env, endpoint, query=""))[result_key]
    for collection, prefix in (
        ("artist", "ar-artist-"),
        ("album", "al-album-"),
        ("song", "tr-track-"),
    ):
        assert [item["id"] for item in result[collection]] == [
            f"{prefix}{index:04d}" for index in range(5)
        ]


async def test_get_cover_art_album_returns_image(compat_env):
    album_id = _sub(_get(compat_env, "getAlbumList2", type="newest"))["albumList2"]["album"][0]["id"]
    r = _get(compat_env, "getCoverArt", id=album_id)
    assert r.status_code == 200
    assert r.headers["content-type"].startswith("image/")


async def test_get_cover_art_track_resolves_to_album(compat_env):
    album = _sub(_get(compat_env, "getAlbumList2", type="newest"))["albumList2"]["album"][0]
    song_id = _sub(_get(compat_env, "getAlbum", id=album["id"]))["album"]["song"][0]["id"]
    r = _get(compat_env, "getCoverArt", id=song_id)
    assert r.status_code == 200
    assert r.headers["content-type"].startswith("image/")


async def test_get_cover_art_artist_returns_image(compat_env):
    artist_id = _sub(_get(compat_env, "getArtists"))["artists"]["index"][0]["artist"][0]["id"]
    r = _get(compat_env, "getCoverArt", id=artist_id)
    assert r.status_code == 200
    assert r.headers["content-type"].startswith("image/")


async def test_get_cover_art_unknown_playlist_is_404(compat_env):
    r = _get(compat_env, "getCoverArt", id="pl-anything")
    assert r.status_code == 404


async def test_get_cover_art_unknown_prefix_404(compat_env):
    r = _get(compat_env, "getCoverArt", id="zz-bogus")
    assert r.status_code == 404  # binary endpoint: 70 -> real HTTP 404


async def test_advertised_cover_url_resolves_under_base_scope(compat_env):
    """The URLs getArtistInfo2 advertises must survive the base-path scope:
    only id (+size) in the query - no credentials - and fetchable at the
    stripped route a client would hit."""
    from urllib.parse import parse_qs, urlparse

    from fastapi.testclient import TestClient

    client = TestClient(compat_env.app, root_path="/dn")
    q = {"v": "1.16.1", "c": "pytest", "f": "json", "apiKey": compat_env.secret}
    artists = _sub(client.get("/subsonic/rest/getArtists", params=q))["artists"]
    artist_id = artists["index"][0]["artist"][0]["id"]
    info = _sub(client.get("/subsonic/rest/getArtistInfo2", params={**q, "id": artist_id}))[
        "artistInfo2"
    ]
    medium = urlparse(info["mediumImageUrl"])
    assert info["mediumImageUrl"].startswith("http://testserver/dn/subsonic/")
    params = parse_qs(medium.query)
    assert set(params) == {"id", "size"}
    assert params["size"] == ["500"]

    fetched = client.get(
        "/subsonic/rest/getCoverArt",
        params={"id": params["id"][0], "size": params["size"][0], **q},
    )
    assert fetched.status_code == 200
    assert fetched.headers["content-type"].startswith("image/")


async def test_get_cover_art_unknown_prefix_404_under_base(compat_env):
    from fastapi.testclient import TestClient

    client = TestClient(compat_env.app, root_path="/dn")
    q = {"v": "1.16.1", "c": "pytest", "f": "json", "apiKey": compat_env.secret}
    r = client.get("/subsonic/rest/getCoverArt", params={**q, "id": "zz-bogus"})
    assert r.status_code == 404
