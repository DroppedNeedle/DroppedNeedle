"""T1.4 - Subsonic search3/search2 + getCoverArt prefix resolution."""

import json

import pytest

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


async def test_get_cover_art_playlist_placeholder(compat_env):
    r = _get(compat_env, "getCoverArt", id="pl-anything")
    assert r.status_code == 200
    assert r.headers["content-type"].startswith("image/")


async def test_get_cover_art_unknown_prefix_404(compat_env):
    r = _get(compat_env, "getCoverArt", id="zz-bogus")
    assert r.status_code == 404  # binary endpoint: 70 -> real HTTP 404
