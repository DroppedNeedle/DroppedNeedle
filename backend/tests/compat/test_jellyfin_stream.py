"""T4.4 - Jellyfin images, PlaybackInfo, and audio streaming."""

import json

import pytest

pytestmark = pytest.mark.asyncio


def _h(env):
    return {"Authorization": f'MediaBrowser Token="{env.secret}", Client="pytest"'}


def _jget(env, path, **params):
    r = env.client.get(f"/jellyfin{path}", params=params, headers=_h(env))
    assert r.status_code == 200, r.content
    return json.loads(r.content)


# ----- Images (compat_env: cover returns JPEG bytes) -----

async def test_album_image_primary(compat_env):
    album_id = _jget(compat_env, "/Items", IncludeItemTypes="MusicAlbum")["Items"][0]["Id"]
    r = compat_env.client.get(f"/jellyfin/Items/{album_id}/Images/Primary")  # unauth
    assert r.status_code == 200
    assert r.headers["content-type"].startswith("image/")


async def test_track_image_resolves_to_album(compat_env):
    album_id = _jget(compat_env, "/Items", IncludeItemTypes="MusicAlbum")["Items"][0]["Id"]
    track_id = _jget(compat_env, "/Items", ParentId=album_id)["Items"][0]["Id"]
    r = compat_env.client.get(f"/jellyfin/Items/{track_id}/Images/Primary")
    assert r.status_code == 200
    assert r.headers["content-type"].startswith("image/")


async def test_image_unknown_item_404(compat_env):
    r = compat_env.client.get("/jellyfin/Items/00000000000000000000000000000000/Images/Primary")
    assert r.status_code == 404


# ----- PlaybackInfo (compat_env) -----

# Finamp hard-casts these; a missing key throws in its generated parser and
# playback never starts (issue #438). Field lists verified against Finamp main
# lib/models/jellyfin_models.g.dart (_$MediaSourceInfoFromJson / _$MediaStreamFromJson).
_FINAMP_REQUIRED_SOURCE = {
    "Protocol": str, "Type": str, "IsRemote": bool,
    "SupportsTranscoding": bool, "SupportsDirectStream": bool,
    "SupportsDirectPlay": bool, "IsInfiniteStream": bool,
    "RequiresOpening": bool, "RequiresClosing": bool,
    "RequiresLooping": bool, "SupportsProbing": bool,
    "MediaStreams": list, "ReadAtNativeFramerate": bool,
    "IgnoreDts": bool, "IgnoreIndex": bool, "GenPtsInput": bool,
}
_FINAMP_REQUIRED_STREAM = {
    "IsInterlaced": bool, "IsDefault": bool, "IsForced": bool,
    "Type": str, "Index": int, "IsExternal": bool,
    "IsTextSubtitleStream": bool, "SupportsExternalStream": bool,
}


def _track_id(env):
    album_id = _jget(env, "/Items", IncludeItemTypes="MusicAlbum")["Items"][0]["Id"]
    return _jget(env, "/Items", ParentId=album_id)["Items"][0]["Id"]


async def test_playback_info_get(compat_env):
    tid = _track_id(compat_env)
    r = compat_env.client.get(f"/jellyfin/Items/{tid}/PlaybackInfo",
                              params={"userId": "user-alice"}, headers=_h(compat_env))
    assert r.status_code == 200
    body = json.loads(r.content)
    src = body["MediaSources"][0]
    assert src["SupportsDirectPlay"] is True
    # Native players fetch DirectStreamUrl without auth headers, so the token must be
    # embedded or the stream 401s (real-client regression: Jellify/Finamp/Manet).
    assert "static=true" in src["DirectStreamUrl"]
    assert f"api_key={compat_env.secret}" in src["DirectStreamUrl"]
    assert tid in src["DirectStreamUrl"]
    assert body["PlaySessionId"]
    assert src["MediaStreams"][0]["Type"] == "Audio"


async def test_playback_info_post_with_device_profile(compat_env):
    tid = _track_id(compat_env)
    r = compat_env.client.post(
        f"/jellyfin/Items/{tid}/PlaybackInfo", headers=_h(compat_env),
        json={"DeviceProfile": {"Name": "x"}, "MaxStreamingBitrate": 999999999},
    )
    assert r.status_code == 200
    body = json.loads(r.content)
    assert body["MediaSources"][0]["Id"] == tid
    assert body["PlaySessionId"]


async def test_playback_info_survives_finamp_parsing(compat_env):
    # Strict-client simulation: Finamp json_serializable hard-casts these source
    # and stream fields, so a missing key (KeyError/None here) is the playback
    # crash (issue #438).
    tid = _track_id(compat_env)
    r = compat_env.client.get(f"/jellyfin/Items/{tid}/PlaybackInfo",
                              params={"userId": "user-alice"}, headers=_h(compat_env))
    assert r.status_code == 200
    src = json.loads(r.content)["MediaSources"][0]
    for field, typ in _FINAMP_REQUIRED_SOURCE.items():
        assert isinstance(src.get(field), typ), field
    assert src["MediaStreams"], "expected at least one audio stream"
    for field, typ in _FINAMP_REQUIRED_STREAM.items():
        assert isinstance(src["MediaStreams"][0].get(field), typ), field


# ----- streaming (streaming_env: real FLAC on disk) -----

async def test_universal_direct_play(streaming_env):
    r = streaming_env.client.get(
        f"/jellyfin/Audio/{streaming_env.jf_track_id}/universal",
        params={"ApiKey": streaming_env.secret, "MaxStreamingBitrate": "999999999"},
    )
    assert r.status_code == 200
    assert r.content == streaming_env.raw
    assert r.headers.get("Content-Encoding", "identity") == "identity"


async def test_stream_static_range_206(streaming_env):
    r = streaming_env.client.get(
        f"/jellyfin/Audio/{streaming_env.jf_track_id}/stream",
        params={"static": "true", "ApiKey": streaming_env.secret},
        headers={"Range": "bytes=0-99"},
    )
    assert r.status_code == 206
    assert r.headers["Content-Range"] == f"bytes 0-99/{len(streaming_env.raw)}"
    assert r.content == streaming_env.raw[0:100]


async def test_stream_static_full_200(streaming_env):
    r = streaming_env.client.get(
        f"/jellyfin/Audio/{streaming_env.jf_track_id}/stream.flac",
        params={"static": "true", "ApiKey": streaming_env.secret},
    )
    assert r.status_code == 200
    assert r.content == streaming_env.raw
    assert r.headers["Content-Type"] == "audio/flac"


async def test_playback_info_direct_stream_url_plays_without_auth_header(streaming_env):
    # End-to-end: a player takes DirectStreamUrl from PlaybackInfo and fetches it with
    # NO auth header. The embedded api_key must carry the request to a 200 with bytes.
    from urllib.parse import urlsplit

    pb = streaming_env.client.post(
        f"/jellyfin/Items/{streaming_env.jf_track_id}/PlaybackInfo",
        headers={"X-Emby-Token": streaming_env.secret}, json={},
    )
    assert pb.status_code == 200
    url = json.loads(pb.content)["MediaSources"][0]["DirectStreamUrl"]
    assert f"api_key={streaming_env.secret}" in url
    parts = urlsplit(url)
    r = streaming_env.client.get(f"{parts.path}?{parts.query}")  # no headers
    assert r.status_code == 200
    assert r.content == streaming_env.raw


async def test_stream_is_anonymous_like_real_jellyfin(streaming_env):
    # Native players fetch /Audio/{id}/stream with NO api_key and NO auth header; real
    # Jellyfin's AudioController has no [Authorize], so it serves anonymously. Must not 401.
    r = streaming_env.client.get(
        f"/jellyfin/Audio/{streaming_env.jf_track_id}/stream",
        params={"static": "true"},  # deliberately no ApiKey, no Authorization header
    )
    assert r.status_code == 200
    assert r.content == streaming_env.raw


async def test_anonymous_stream_ranges_are_exempt_from_public_limiter(streaming_env):
    from api.compat.common.ratelimit import compat_rate_limits

    compat_rate_limits.reset()
    path = f"/jellyfin/Audio/{streaming_env.jf_track_id}/stream"
    responses = [
        streaming_env.client.get(path, headers={"Range": "bytes=0-0"})
        for _ in range(25)
    ]
    assert all(response.status_code == 206 for response in responses)
    assert all(response.content == streaming_env.raw[:1] for response in responses)


async def test_stream_head(streaming_env):
    r = streaming_env.client.head(
        f"/jellyfin/Audio/{streaming_env.jf_track_id}/stream",
        params={"ApiKey": streaming_env.secret},
    )
    assert r.status_code == 200
    assert r.headers["Accept-Ranges"] == "bytes"


async def test_stream_unknown_track_404(streaming_env):
    r = streaming_env.client.get(
        "/jellyfin/Audio/00000000000000000000000000000000/universal",
        params={"ApiKey": streaming_env.secret},
    )
    assert r.status_code == 404
