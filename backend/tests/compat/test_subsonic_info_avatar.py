import json

import pytest

from api.compat.subsonic.ids import encode
from tests.compat.conftest import subsonic_query


def _body(response):
    return json.loads(response.content)["subsonic-response"]


@pytest.mark.asyncio
async def test_album_and_artist_info_validate_target_and_return_truthful_mbid(compat_env):
    query = subsonic_query(compat_env.secret, "alice")
    album_id = encode("album", compat_env.ids["rg"])
    album = _body(compat_env.client.get(
        "/subsonic/rest/getAlbumInfo2", params={**query, "id": album_id}
    ))
    assert album["albumInfo"]["musicBrainzId"] == compat_env.ids["rg"]
    assert "getCoverArt" in album["albumInfo"]["smallImageUrl"]

    artists, _ = await compat_env.view.get_artists()
    artist_id = encode("artist", artists[0].artist_mbid)
    artist = _body(compat_env.client.get(
        "/subsonic/rest/getArtistInfo", params={**query, "id": artist_id}
    ))
    if "-" in artists[0].artist_mbid:
        assert artist["artistInfo"]["musicBrainzId"] == artists[0].artist_mbid
    else:
        assert "musicBrainzId" not in artist["artistInfo"]
    assert artist["artistInfo"]["similarArtist"] == []

    missing = _body(compat_env.client.get(
        "/subsonic/rest/getAlbumInfo", params={**query, "id": encode("album", "missing")}
    ))
    assert missing["status"] == "failed"
    assert missing["error"]["code"] == 70


@pytest.mark.asyncio
async def test_avatar_is_self_only_binary_and_head_safe(compat_env):
    compat_env.avatar_dir.mkdir()
    payload = b"\x89PNG\r\n\x1a\nprivate-avatar"
    (compat_env.avatar_dir / "user-alice.png").write_bytes(payload)
    query = subsonic_query(compat_env.secret, "alice")

    response = compat_env.client.get(
        "/subsonic/rest/getAvatar", params={**query, "username": "ALICE"}
    )
    assert response.status_code == 200
    assert response.content == payload
    assert response.headers["content-type"] == "image/png"
    assert response.headers["cache-control"].startswith("private")

    head = compat_env.client.head(
        "/subsonic/rest/getAvatar", params={**query, "username": "alice"}
    )
    assert head.status_code == 200
    assert head.content == b""
    assert head.headers["content-length"] == str(len(payload))

    denied = compat_env.client.get(
        "/subsonic/rest/getAvatar", params={**query, "username": "bob"}
    )
    assert denied.status_code == 403
    assert b"private-avatar" not in denied.content
