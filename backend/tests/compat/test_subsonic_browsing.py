"""T1.3 - Subsonic system + browsing endpoints against a seeded library."""

import json
from pathlib import Path

import pytest

from infrastructure.persistence.library_db import _ALBUM_AGG_SORTS

pytestmark = pytest.mark.asyncio


def _sub(resp):
    assert resp.status_code == 200
    return json.loads(resp.content)["subsonic-response"]


def _get(env, endpoint, **extra):
    q = {"v": "1.16.1", "c": "pytest", "f": "json", "apiKey": env.secret, **extra}
    return _sub(env.client.get(f"/subsonic/rest/{endpoint}", params=q))


async def _add_album(env, *, rg: str, title: str, year: int, genre: str):
    from models.audio import AudioInfo, AudioTag

    return await env.lm.upsert_file(
        Path(f"/music/{title}.flac"),
        AudioTag(
            title=f"{title} Track",
            artist="Test Artist",
            album=title,
            album_artist="Test Artist",
            track_number=1,
            year=year,
            genre=genre,
        ),
        AudioInfo(
            duration_seconds=120,
            bitrate=900,
            sample_rate=44100,
            channels=2,
            file_format="flac",
            file_size_bytes=1000,
            bit_depth=16,
        ),
        release_group_mbid=rg,
        recording_mbid=f"recording-{rg}",
        file_mtime=1.0,
    )


async def test_get_license(compat_env):
    body = _get(compat_env, "getLicense")
    assert body["license"]["valid"] is True


async def test_get_music_folders(compat_env):
    body = _get(compat_env, "getMusicFolders")
    folders = body["musicFolders"]["musicFolder"]
    assert folders == [{"id": 1, "name": "DroppedNeedle"}]


async def test_get_artists_index_shape(compat_env):
    body = _get(compat_env, "getArtists")
    artists = body["artists"]
    assert artists["ignoredArticles"]
    # Radiohead -> index "R"
    letters = {idx["name"] for idx in artists["index"]}
    assert "R" in letters
    r = next(idx for idx in artists["index"] if idx["name"] == "R")
    assert isinstance(r["artist"], list)
    a = r["artist"][0]
    assert a["name"] == "Radiohead"
    assert a["id"].startswith("ar-")


async def test_get_artist_then_album_then_song_drilldown(compat_env):
    artists = _get(compat_env, "getArtists")["artists"]
    artist_id = artists["index"][0]["artist"][0]["id"]

    artist = _get(compat_env, "getArtist", id=artist_id)["artist"]
    assert isinstance(artist["album"], list) and artist["album"]
    album_id = artist["album"][0]["id"]
    assert album_id.startswith("al-")

    album = _get(compat_env, "getAlbum", id=album_id)["album"]
    songs = album["song"]
    assert len(songs) == 2
    # ordered by disc/track
    assert [s["track"] for s in songs] == [1, 2]
    s0 = songs[0]
    assert s0["id"].startswith("tr-")
    # OpenSubsonic-required song fields
    assert s0["mediaType"] == "song"
    assert s0["channelCount"] == 2
    assert s0["samplingRate"] == 44100
    assert s0["contentType"] == "audio/flac"

    song = _get(compat_env, "getSong", id=s0["id"])["song"]
    assert song["title"] == "Airbag"


async def test_get_album_list2_newest(compat_env):
    body = _get(compat_env, "getAlbumList2", type="newest")
    albums = body["albumList2"]["album"]
    assert len(albums) == 1
    assert albums[0]["name"] == "OK Computer"
    assert albums[0]["id"].startswith("al-")


async def test_get_album_list2_requires_type(compat_env):
    q = {"v": "1.16.1", "c": "pytest", "f": "json", "apiKey": compat_env.secret}
    body = json.loads(compat_env.client.get("/subsonic/rest/getAlbumList2", params=q).content)
    assert body["subsonic-response"]["error"]["code"] == 10


async def test_get_album_list_file_structure(compat_env):
    body = _get(compat_env, "getAlbumList", type="alphabeticalByName")
    album = body["albumList"]["album"][0]
    assert album["isDir"] is True
    assert album["id"].startswith("al-")


async def test_album_list_uses_exact_arbitrary_offset(compat_env):
    await _add_album(
        compat_env,
        rg="00000000-0000-0000-0000-000000000001",
        title="Alpha",
        year=1990,
        genre="Rock",
    )
    await _add_album(
        compat_env,
        rg="00000000-0000-0000-0000-000000000002",
        title="Beta",
        year=2000,
        genre="Jazz",
    )
    await _add_album(
        compat_env,
        rg="00000000-0000-0000-0000-000000000003",
        title="Gamma",
        year=2010,
        genre="Rock",
    )
    albums = _get(
        compat_env,
        "getAlbumList2",
        type="alphabeticalByName",
        size="2",
        offset="1",
    )["albumList2"]["album"]
    assert [album["name"] for album in albums] == ["Beta", "Gamma"]


async def test_album_list_by_year_direction_and_genre(compat_env):
    await _add_album(
        compat_env,
        rg="00000000-0000-0000-0000-000000000010",
        title="Older Rock",
        year=1990,
        genre="Rock",
    )
    await _add_album(
        compat_env,
        rg="00000000-0000-0000-0000-000000000011",
        title="Newer Jazz",
        year=2010,
        genre="Jazz",
    )
    ascending = _get(
        compat_env,
        "getAlbumList2",
        type="byYear",
        fromYear="1980",
        toYear="2020",
        size="20",
    )["albumList2"]["album"]
    descending = _get(
        compat_env,
        "getAlbumList2",
        type="byYear",
        fromYear="2020",
        toYear="1980",
        size="20",
    )["albumList2"]["album"]
    assert [album["year"] for album in ascending] == sorted(
        album["year"] for album in ascending
    )
    assert [album["year"] for album in descending] == sorted(
        (album["year"] for album in descending), reverse=True
    )
    rock = _get(
        compat_env, "getAlbumList2", type="byGenre", genre="rOcK", size="20"
    )["albumList2"]["album"]
    assert {album["name"] for album in rock} == {"Older Rock"}


async def test_album_list_by_year_zero_means_unbounded(compat_env):
    # Feishin (and other Subsonic clients) send fromYear/toYear=0 as their
    # convention for "no bound on this side" - a real year is never 0, so a
    # bare zero must not be rejected the way an out-of-range value would be.
    await _add_album(
        compat_env,
        rg="00000000-0000-0000-0000-000000000012",
        title="Old Enough",
        year=1990,
        genre="Rock",
    )
    await _add_album(
        compat_env,
        rg="00000000-0000-0000-0000-000000000013",
        title="Recent Enough",
        year=2010,
        genre="Rock",
    )
    open_upper = _get(
        compat_env,
        "getAlbumList2",
        type="byYear",
        fromYear="1980",
        toYear="0",
        size="20",
    )["albumList2"]["album"]
    assert {album["name"] for album in open_upper} >= {"Old Enough", "Recent Enough"}
    open_lower = _get(
        compat_env,
        "getAlbumList2",
        type="byYear",
        fromYear="0",
        toYear="2020",
        size="20",
    )["albumList2"]["album"]
    assert {album["name"] for album in open_lower} >= {"Old Enough", "Recent Enough"}


async def test_album_list_highest_and_unknown_fail_explicitly(compat_env):
    highest = _get(compat_env, "getAlbumList2", type="highest")
    assert highest["status"] == "failed"
    assert highest["error"]["code"] == 0
    unknown = _get(compat_env, "getAlbumList2", type="not-a-real-type")
    assert unknown["status"] == "failed"
    assert unknown["error"]["code"] == 10


async def test_album_list_starred_recent_and_frequent_are_user_scoped(compat_env):
    album_id = "al-" + compat_env.ids["rg"]
    _get(compat_env, "star", albumId=album_id)
    starred = _get(compat_env, "getAlbumList2", type="starred")["albumList2"]["album"]
    assert [album["id"] for album in starred] == [album_id]

    for played_at in ("2026-01-01T00:00:00+00:00", "2026-01-02T00:00:00+00:00"):
        await compat_env.phs.insert(
            "user-alice",
            track_name="Airbag",
            artist_name="Radiohead",
            album_name="OK Computer",
            release_group_mbid=compat_env.ids["rg"],
            played_at=played_at,
        )
    assert _get(compat_env, "getAlbumList2", type="recent")["albumList2"]["album"]
    assert _get(compat_env, "getAlbumList2", type="frequent")["albumList2"]["album"]


async def test_get_random_songs(compat_env):
    body = _get(compat_env, "getRandomSongs", size="10")
    songs = body["randomSongs"]["song"]
    assert {s["title"] for s in songs} == {"Airbag", "Paranoid Android"}


async def test_get_indexes_file_structure(compat_env):
    body = _get(compat_env, "getIndexes")
    idx = body["indexes"]
    assert idx["index"][0]["artist"][0]["name"] == "Radiohead"
    assert idx["lastModified"] > 0


async def test_get_indexes_if_modified_since_short_circuits(compat_env):
    first = _get(compat_env, "getIndexes")["indexes"]
    unchanged = _get(
        compat_env, "getIndexes", ifModifiedSince=str(first["lastModified"])
    )["indexes"]
    assert unchanged["lastModified"] == first["lastModified"]
    assert unchanged["index"] == []


async def test_hosted_music_folder_validation_accepts_repeated_one(compat_env):
    base = [
        ("v", "1.16.1"),
        ("c", "pytest"),
        ("f", "json"),
        ("apiKey", compat_env.secret),
        ("musicFolderId", "1"),
        ("musicFolderId", "1"),
    ]
    body = _sub(compat_env.client.get("/subsonic/rest/getArtists", params=base))
    assert body["status"] == "ok"


async def test_hosted_music_folder_validation_rejects_any_non_one(compat_env):
    base = [
        ("f", "json"),
        ("apiKey", compat_env.secret),
        ("musicFolderId", "1"),
        ("musicFolderId", "2"),
    ]
    body = _sub(compat_env.client.get("/subsonic/rest/getArtists", params=base))
    assert body["status"] == "failed"
    assert body["error"]["code"] == 70


async def test_get_music_directory_artist_then_album(compat_env):
    artist_id = _get(compat_env, "getArtists")["artists"]["index"][0]["artist"][0]["id"]
    d = _get(compat_env, "getMusicDirectory", id=artist_id)["directory"]
    assert d["child"][0]["isDir"] is True
    album_id = d["child"][0]["id"]
    d2 = _get(compat_env, "getMusicDirectory", id=album_id)["directory"]
    assert len(d2["child"]) == 2
    assert d2["child"][0]["isDir"] is False


async def test_get_music_directory_root_is_folder_one(compat_env):
    root = _get(compat_env, "getMusicDirectory", id="1")["directory"]
    assert root["id"] == "1"
    assert root["name"] == "DroppedNeedle"
    assert root["child"][0]["id"].startswith("ar-")
    assert root["child"][0]["isDir"] is True


async def test_unknown_album_id_is_70(compat_env):
    q = {"v": "1.16.1", "c": "pytest", "f": "json", "apiKey": compat_env.secret,
         "id": "al-does-not-exist"}
    body = json.loads(compat_env.client.get("/subsonic/rest/getAlbum", params=q).content)
    assert body["subsonic-response"]["error"]["code"] == 70


async def test_xml_format_drilldown(compat_env):
    q = {"v": "1.16.1", "c": "pytest", "f": "xml", "apiKey": compat_env.secret}
    r = compat_env.client.get("/subsonic/rest/getArtists", params=q)
    assert r.headers["content-type"].startswith("application/xml")
    assert b'<artists' in r.content and b'name="Radiohead"' in r.content
async def test_random_album_lists_use_sqlite_random_order():
    assert _ALBUM_AGG_SORTS["random"] == "RANDOM()"
