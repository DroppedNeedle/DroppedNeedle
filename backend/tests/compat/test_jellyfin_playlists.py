"""T5.3 - Jellyfin playlists CRUD + discovery (Similar/InstantMix)."""

import json

import pytest

pytestmark = pytest.mark.asyncio


def _h(env):
    return {"Authorization": f'MediaBrowser Token="{env.secret}", Client="pytest"'}


def _jget(env, path, **params):
    r = env.client.get(f"/jellyfin{path}", params=params, headers=_h(env))
    assert r.status_code == 200, r.content
    return json.loads(r.content)


def _track_ids(env):
    return [t["Id"] for t in _jget(env, "/Items", IncludeItemTypes="Audio")["Items"]]


async def test_playlist_create_discover_items_roundtrip(compat_env):
    tracks = _track_ids(compat_env)
    # create with two tracks
    r = compat_env.client.post(
        "/jellyfin/Playlists", headers=_h(compat_env),
        json={"Name": "JF Mix", "Ids": tracks, "UserId": "user-alice"},
    )
    assert r.status_code == 200
    pid = json.loads(r.content)["Id"]
    assert pid.startswith  # 32-hex jf id

    # discovered via /Items?IncludeItemTypes=Playlist
    pls = _jget(compat_env, "/Items", IncludeItemTypes="Playlist")["Items"]
    assert any(p["Id"] == pid and p["Type"] == "Playlist" for p in pls)

    # a single-item GET on the playlist resolves (clients open playlists this way)
    one = _jget(compat_env, f"/Items/{pid}")
    assert one["Id"] == pid and one["Type"] == "Playlist" and one["Name"] == "JF Mix"

    # GET /Playlists/{id}/Items -> entries carry PlaylistItemId
    items = _jget(compat_env, f"/Playlists/{pid}/Items")["Items"]
    assert len(items) == 2
    assert all(it.get("PlaylistItemId") for it in items)
    assert items[0]["Id"] == tracks[0]  # order preserved

    # remove the first entry by its PlaylistItemId
    entry_id = items[0]["PlaylistItemId"]
    d = compat_env.client.delete(
        f"/jellyfin/Playlists/{pid}/Items", params={"entryIds": entry_id},
        headers=_h(compat_env),
    )
    assert d.status_code == 204
    after = _jget(compat_env, f"/Playlists/{pid}/Items")["Items"]
    assert len(after) == 1

    # add it back
    a = compat_env.client.post(
        f"/jellyfin/Playlists/{pid}/Items", params={"ids": tracks[0]},
        headers=_h(compat_env),
    )
    assert a.status_code == 204
    assert len(_jget(compat_env, f"/Playlists/{pid}/Items")["Items"]) == 2


async def test_playlist_move(compat_env):
    tracks = _track_ids(compat_env)
    pid = json.loads(compat_env.client.post(
        "/jellyfin/Playlists", headers=_h(compat_env),
        json={"Name": "Move", "Ids": tracks}).content)["Id"]
    items = _jget(compat_env, f"/Playlists/{pid}/Items")["Items"]
    first_entry = items[0]["PlaylistItemId"]
    r = compat_env.client.post(
        f"/jellyfin/Playlists/{pid}/Items/{first_entry}/Move/1", headers=_h(compat_env)
    )
    assert r.status_code == 204
    reordered = _jget(compat_env, f"/Playlists/{pid}/Items")["Items"]
    assert reordered[1]["Id"] == items[0]["Id"]


async def test_get_playlist_metadata(compat_env):
    tracks = _track_ids(compat_env)
    pid = json.loads(compat_env.client.post(
        "/jellyfin/Playlists", headers=_h(compat_env),
        json={"Name": "Meta", "Ids": tracks}).content)["Id"]
    meta = _jget(compat_env, f"/Playlists/{pid}")
    assert meta["Name"] == "Meta"
    assert len(meta["ItemIds"]) == 2


async def test_similar_and_instant_mix_owned_only(compat_env):
    artist = next(a for a in _jget(compat_env, "/Artists/AlbumArtists")["Items"]
                  if a["Name"] == "Radiohead")
    sim = _jget(compat_env, f"/Items/{artist['Id']}/Similar", Limit="10")["Items"]
    assert {t["Name"] for t in sim} == {"Airbag", "Paranoid Android"}
    mix = _jget(compat_env, f"/Artists/{artist['Id']}/InstantMix", Limit="10")["Items"]
    assert mix  # owned same-artist tracks


async def test_create_playlist_unknown_track_ignored(compat_env):
    r = compat_env.client.post(
        "/jellyfin/Playlists", headers=_h(compat_env),
        json={"Name": "Empty", "Ids": ["00000000000000000000000000000000"]},
    )
    assert r.status_code == 200
    pid = json.loads(r.content)["Id"]
    assert _jget(compat_env, f"/Playlists/{pid}/Items")["Items"] == []


async def test_web_ui_local_playlist_visible(compat_env, auth_store):
    # web-UI shape: source_type='local' + track_source_id, no explicit
    # library_file_id - must still surface via /Playlists/{id}/Items (issue #181)
    fid = compat_env.ids["tracks"][0]
    user = await auth_store.get_user_by_id("user-alice")
    record = await compat_env.playlists.create_playlist("Web Mix", user_id="user-alice")
    await compat_env.playlists.add_tracks(record.id, user, [{
        "track_name": "Airbag", "artist_name": "Radiohead",
        "album_name": "OK Computer", "track_source_id": fid,
        "source_type": "local", "duration": 201,
    }])
    jf_pid = await compat_env.id_map.to_jf("playlist", record.id)
    items = _jget(compat_env, f"/Playlists/{jf_pid}/Items")["Items"]
    assert len(items) == 1
    assert items[0]["Name"] == "Airbag"
    # browse listing advertises the served count, like Subsonic getPlaylists
    pls = _jget(compat_env, "/Items", IncludeItemTypes="Playlist")["Items"]
    mine = next(p for p in pls if p["Id"] == jf_pid)
    assert mine["ChildCount"] == 1
