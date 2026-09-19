"""T2.1 - Subsonic playlists CRUD via the bridged PlaylistService."""

import json

import pytest

pytestmark = pytest.mark.asyncio


def _sub(resp):
    assert resp.status_code == 200
    return json.loads(resp.content)["subsonic-response"]


def _get(env, endpoint, **extra):
    q = {"v": "1.16.1", "c": "pytest", "f": "json", "apiKey": env.secret, **extra}
    return env.client.get(f"/subsonic/rest/{endpoint}", params=q)


def _track_ids(env):
    res = _sub(_get(env, "search3", query=""))["searchResult3"]["song"]
    return [s["id"] for s in res]


async def test_create_get_update_delete_roundtrip(compat_env):
    songs = _track_ids(compat_env)
    created = _sub(_get(compat_env, "createPlaylist", name="My Mix",
                        songId=songs))["playlist"]
    pid = created["id"]
    assert pid.startswith("pl-")
    assert created["name"] == "My Mix"
    assert created["songCount"] == 2
    assert [e["id"] for e in created["entry"]] == songs  # songId<->file_id order

    lists = _sub(_get(compat_env, "getPlaylists"))["playlists"]["playlist"]
    assert any(p["id"] == pid and p["songCount"] == 2 for p in lists)

    got = _sub(_get(compat_env, "getPlaylist", id=pid))["playlist"]
    assert len(got["entry"]) == 2
    assert got["owner"] == "alice"

    _sub(_get(compat_env, "updatePlaylist", playlistId=pid, name="Renamed",
              songIndexToRemove="0"))
    got2 = _sub(_get(compat_env, "getPlaylist", id=pid))["playlist"]
    assert got2["name"] == "Renamed"
    assert got2["songCount"] == 1
    assert got2["entry"][0]["id"] == songs[1]

    _sub(_get(compat_env, "updatePlaylist", playlistId=pid, songIdToAdd=songs[0]))
    got3 = _sub(_get(compat_env, "getPlaylist", id=pid))["playlist"]
    assert got3["songCount"] == 2

    _sub(_get(compat_env, "deletePlaylist", id=pid))
    after = _sub(_get(compat_env, "getPlaylists"))["playlists"].get("playlist", [])
    assert all(p["id"] != pid for p in after)


async def test_create_requires_name(compat_env):
    body = json.loads(_get(compat_env, "createPlaylist").content)["subsonic-response"]
    assert body["error"]["code"] == 10


async def test_create_replace_existing(compat_env):
    songs = _track_ids(compat_env)
    pid = _sub(_get(compat_env, "createPlaylist", name="Orig", songId=songs[0]))["playlist"]["id"]
    replaced = _sub(_get(compat_env, "createPlaylist", playlistId=pid, songId=songs[1]))["playlist"]
    assert replaced["songCount"] == 1
    assert replaced["entry"][0]["id"] == songs[1]


async def test_get_unknown_playlist_is_70(compat_env):
    body = json.loads(_get(compat_env, "getPlaylist", id="pl-nope").content)["subsonic-response"]
    assert body["error"]["code"] == 70


async def test_web_ui_local_playlist_visible_in_clients(compat_env, auth_store):
    # The web UI adds local tracks with source_type='local' and track_source_id set
    # to the library file id, never library_file_id itself. Before the add_tracks
    # auto-link those entries were invisible to compat clients (issue #181).
    songs = _track_ids(compat_env)
    fid = songs[0][3:]  # strip tr-
    user = await auth_store.get_user_by_id("user-alice")
    record = await compat_env.playlists.create_playlist("Web Mix", user_id="user-alice")
    await compat_env.playlists.add_tracks(record.id, user, [{
        "track_name": "Airbag", "artist_name": "Radiohead",
        "album_name": "OK Computer", "album_id": None, "artist_id": None,
        "track_source_id": fid, "cover_url": None, "source_type": "local",
        "available_sources": ["local"], "format": "flac",
        "track_number": 1, "disc_number": 1, "duration": 201,
    }])

    got = _sub(_get(compat_env, "getPlaylist", id=f"pl-{record.id}"))["playlist"]
    assert [e["id"] for e in got["entry"]] == [songs[0]]

    lists = _sub(_get(compat_env, "getPlaylists"))["playlists"]["playlist"]
    assert any(p["id"] == f"pl-{record.id}" and p["songCount"] == 1 for p in lists)


async def test_get_playlists_counts_only_streamable_entries(compat_env, auth_store):
    # An entry with no library file (e.g. an unresolved import) is skipped by
    # getPlaylist, so getPlaylists must not count it either - clients treat
    # "songCount > entries" as a broken playlist (issue #181).
    songs = _track_ids(compat_env)
    user = await auth_store.get_user_by_id("user-alice")
    record = await compat_env.playlists.create_playlist("Mixed", user_id="user-alice")
    await compat_env.playlists.add_tracks(record.id, user, [
        {
            "track_name": "Airbag", "artist_name": "Radiohead",
            "album_name": "OK Computer", "track_source_id": songs[0][3:],
            "source_type": "local", "duration": 201,
        },
        {
            "track_name": "Not Here", "artist_name": "Someone",
            "album_name": "Elsewhere", "track_source_id": "spotify:xyz",
            "source_type": "", "duration": 100,
        },
    ])
    lists = _sub(_get(compat_env, "getPlaylists"))["playlists"]["playlist"]
    mine = next(p for p in lists if p["id"] == f"pl-{record.id}")
    assert mine["songCount"] == 1
    got = _sub(_get(compat_env, "getPlaylist", id=f"pl-{record.id}"))["playlist"]
    assert len(got["entry"]) == 1


async def test_library_file_id_roundtrip_to_stream(compat_env):
    # songId (tr-file) -> createPlaylist -> getPlaylist entry id decodes back to the
    # same tr-file (Q11 library_file_id linkage)
    songs = _track_ids(compat_env)
    pid = _sub(_get(compat_env, "createPlaylist", name="X", songId=songs[0]))["playlist"]["id"]
    entry = _sub(_get(compat_env, "getPlaylist", id=pid))["playlist"]["entry"][0]
    assert entry["id"] == songs[0]
    tracks = await compat_env.playlists.get_tracks(pid[3:])
    assert tracks[0].library_file_id == songs[0][3:]
    assert tracks[0].source_type == "droppedneedle-local"


async def test_uploaded_playlist_cover_is_advertised_and_served(compat_env, auth_store):
    alice = await auth_store.get_user_by_id("user-alice")
    record = await compat_env.playlists.create_playlist("Covered", user_id=alice.id)
    data = b"\x89PNG\r\n\x1a\nplaylist-cover"
    await compat_env.playlists.upload_cover(record.id, alice, data, "image/png")

    playlist = _sub(_get(compat_env, "getPlaylist", id=f"pl-{record.id}"))["playlist"]
    assert playlist["coverArt"] == f"pl-{record.id}"
    response = _get(compat_env, "getCoverArt", id=playlist["coverArt"])
    assert response.status_code == 200
    assert response.content == data
    assert response.headers["content-type"].startswith("image/png")

    q = {"f": "json", "apiKey": compat_env.secret, "id": playlist["coverArt"]}
    head = compat_env.client.head("/subsonic/rest/getCoverArt", params=q)
    assert head.status_code == 200
    assert head.content == b""
    assert head.headers["content-length"] == str(len(data))


async def test_private_playlist_cover_is_hidden_from_another_user(
    compat_env, auth_store, app_password_service
):
    alice = await auth_store.get_user_by_id("user-alice")
    record = await compat_env.playlists.create_playlist("Private", user_id=alice.id)
    await compat_env.playlists.upload_cover(
        record.id, alice, b"\x89PNG\r\n\x1a\nprivate", "image/png"
    )
    _record, bob_secret = await app_password_service.create("user-bob", "Bob client")
    response = compat_env.client.get(
        "/subsonic/rest/getCoverArt",
        params={"f": "json", "apiKey": bob_secret, "id": f"pl-{record.id}"},
    )
    assert response.status_code == 404


async def test_public_playlist_cover_is_visible_to_another_user(
    compat_env, auth_store, app_password_service
):
    alice = await auth_store.get_user_by_id("user-alice")
    record = await compat_env.playlists.create_playlist("Public", user_id=alice.id)
    await compat_env.playlists.upload_cover(
        record.id, alice, b"\x89PNG\r\n\x1a\npublic", "image/png"
    )
    await compat_env.playlists.set_public(record.id, alice, True)
    _record, bob_secret = await app_password_service.create("user-bob", "Bob client")
    response = compat_env.client.get(
        "/subsonic/rest/getCoverArt",
        params={"f": "json", "apiKey": bob_secret, "id": f"pl-{record.id}"},
    )
    assert response.status_code == 200
    assert response.content.endswith(b"public")


async def test_spotify_imported_cover_advertised_and_served(
    compat_env, db_path, write_lock
):
    """GH-287: a Spotify-imported playlist cover must reach Feishin - coverArt
    advertised in the playlist list/detail and the bytes served by getCoverArt."""
    import threading
    from unittest.mock import AsyncMock

    from infrastructure.cache.memory_cache import InMemoryCache
    from repositories.async_playlist_repository import AsyncPlaylistRepository
    from repositories.playlist_repository import PlaylistRepository
    from services.spotify_import_service import (
        SpotifyImportService,
        cover_fetcher_for,
    )
    from tests.mocks.spotify_cdn_mock import COVER_URL, JPEG_BYTES, SpotifyCdnMock

    cdn = SpotifyCdnMock()
    spotify_client = AsyncMock()
    spotify_client.get_playlist.return_value = {
        "id": "spot-9",
        "name": "From Spotify",
        "images": [{"url": COVER_URL, "width": 640, "height": 640}],
    }
    spotify_client.get_playlist_tracks.return_value = []
    factory = AsyncMock()
    factory.resolve_spotify.return_value = spotify_client
    importer = SpotifyImportService(
        client_factory=factory,
        playlist_repo=None,
        mb_repo=AsyncMock(),
        playlist_service=compat_env.playlists,
        cache=InMemoryCache(),
        async_playlist_repo=AsyncPlaylistRepository(
            PlaylistRepository(db_path=db_path, write_lock=write_lock)
        ),
        cover_fetcher=cover_fetcher_for(cdn.client()),
    )

    pid = await importer.ensure_playlist_record("user-alice", "spot-9", "From Spotify")
    await importer.populate_playlist("user-alice", "spot-9", pid)

    lists = _sub(_get(compat_env, "getPlaylists"))["playlists"]["playlist"]
    mine = next(p for p in lists if p["id"] == f"pl-{pid}")
    assert mine["coverArt"] == f"pl-{pid}"

    detail = _sub(_get(compat_env, "getPlaylist", id=f"pl-{pid}"))["playlist"]
    assert detail["coverArt"] == f"pl-{pid}"

    response = _get(compat_env, "getCoverArt", id=f"pl-{pid}")
    assert response.status_code == 200
    assert response.content == JPEG_BYTES
    assert response.headers["content-type"].startswith("image/jpeg")
