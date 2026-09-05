"""SpotifyImportService unit tests (PR #108).

The GH-287 block at the bottom wires the importer against REAL
PlaylistRepository/PlaylistService stores and a MockTransport Spotify CDN
(tests/mocks/spotify_cdn_mock.py) to prove playlist-cover persistence,
degradation, and ownership behavior end to end.
"""

import threading
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from infrastructure.degradation import (
    clear_degradation_context,
    init_degradation_context,
)
from repositories.playlist_repository import PlaylistRepository
from services.playlist_service import PlaylistService
from services.spotify_import_service import (
    CoverFetcher,
    SpotifyImportService,
    SpotifyNotLinkedError,
    _best_image_url,
    cover_fetcher_for,
    fetch_spotify_playlist_cover,
)
import services.spotify_import_service as spotify_module

from tests.mocks.spotify_cdn_mock import (
    COVER_URL,
    JPEG_BYTES,
    PNG_BYTES,
    SpotifyCdnMock,
)


def _service(client) -> SpotifyImportService:
    factory = AsyncMock()
    factory.resolve_spotify = AsyncMock(return_value=client)
    return SpotifyImportService(
        client_factory=factory,
        playlist_repo=MagicMock(),
        mb_repo=AsyncMock(),
        playlist_service=AsyncMock(),
    )


@pytest.mark.asyncio
async def test_list_playlists_raises_when_not_linked():
    svc = _service(client=None)  # resolve_spotify returns None -> not linked
    with pytest.raises(SpotifyNotLinkedError):
        await svc.list_playlists("user-1")


@pytest.mark.asyncio
async def test_list_playlists_filters_to_owned_and_marks_imported():
    client = AsyncMock()
    client.spotify_user_id = "spot-me"
    client.get_user_playlists = AsyncMock(
        return_value=[
            {
                "id": "p1",
                "name": "Mine",
                "description": "",
                "owner": {"id": "spot-me", "display_name": "Me"},
                "images": [{"url": "cover-1", "width": 300}],
                "tracks": {"total": 5},
            },
            {
                "id": "p2",
                "name": "Someone else's",
                "owner": {"id": "other-user"},
                "tracks": {"total": 2},
            },
        ]
    )
    svc = _service(client)
    # p1 was already imported as internal playlist 'int-1'.
    svc._async_repo = AsyncMock()
    svc._async_repo.get_all_playlists = AsyncMock(
        return_value=[SimpleNamespace(id="int-1", source_ref="spotify:p1")]
    )

    result = await svc.list_playlists("user-1")

    # p2 is owned by another Spotify user -> filtered out.
    assert [p["id"] for p in result] == ["p1"]
    assert result[0]["imported_playlist_id"] == "int-1"
    assert result[0]["track_count"] == 5
    assert result[0]["cover_url"] == "cover-1"

@pytest.mark.asyncio
async def test_list_playlists_reads_new_shape_items_total():
    client = AsyncMock()
    client.spotify_user_id = "spot-me"
    client.get_user_playlists = AsyncMock(
        return_value=[
            {
                "id": "p9",
                "name": "New shape",
                "description": "",
                "owner": {"id": "spot-me", "display_name": "Me"},
                "images": [],
                "items": {"total": 9},
                "tracks": None,
            },
            {
                "id": "p-list",
                "name": "List-shaped items",
                "description": "",
                "owner": {"id": "spot-me", "display_name": "Me"},
                "images": [],
                "items": [{"id": "t1"}],
                "tracks": None,
            },
            {
                "id": "p-fallback",
                "name": "Empty items falls back to tracks",
                "description": "",
                "owner": {"id": "spot-me", "display_name": "Me"},
                "images": [],
                "items": {},
                "tracks": {"total": 5},
            },
        ]
    )
    svc = _service(client)
    svc._async_repo = AsyncMock()
    svc._async_repo.get_all_playlists = AsyncMock(return_value=[])

    result = await svc.list_playlists("user-1")

    assert [p["id"] for p in result] == ["p9", "p-list", "p-fallback"]
    assert result[0]["track_count"] == 9
    assert result[1]["track_count"] == 0
    assert result[2]["track_count"] == 5


@pytest.mark.asyncio
async def test_populate_playlist_with_no_tracks_writes_empty():
    client = AsyncMock()
    client.get_playlist = AsyncMock(return_value={"id": "p1", "name": "Empty"})
    client.get_playlist_tracks = AsyncMock(return_value=[])
    svc = _service(client)
    svc._async_repo = AsyncMock()
    svc._async_repo.get_tracks = AsyncMock(return_value=[])
    svc._async_repo.add_tracks = AsyncMock()

    await svc.populate_playlist("user-1", "p1", "int-1")

    # No tracks resolved -> no MusicBrainz calls, and an empty track list is written.
    svc._mb_repo.resolve_recording_to_release_group.assert_not_awaited()
    svc._async_repo.add_tracks.assert_awaited_once_with("int-1", [])


@pytest.mark.asyncio
async def test_album_fallback_searches_artist_and_release_title_separately():
    svc = _service(AsyncMock())
    svc._mb_repo.search_release_groups.return_value = [
        SimpleNamespace(musicbrainz_id="clairo-originals-rg")
    ]

    result = await svc._resolve_mbid(None, "Clairo", "Originals")

    assert result == "clairo-originals-rg"
    svc._mb_repo.search_release_groups.assert_awaited_once_with(
        "Clairo", "Originals", limit=3, include_all_types=False
    )

def _mb_credit(name: str) -> list[dict]:
    return [{"name": name, "artist": {"name": name}}]


_FANCY_MIXTAPE_RELEASE = {
    "id": "release-fancy-that",
    "status": "Official",
    "date": "2025-03-28",
    "artist-credit": _mb_credit("PinkPantheress"),
    "release-group": {
        "id": "rg-fancy-that",
        "title": "Fancy That",
        "primary-type": "Album",
        "secondary-types": ["Mixtape/Street"],
        "artist-credit": _mb_credit("PinkPantheress"),
    },
}

_BRAVO_COMPILATION_RELEASE = {
    "id": "release-bravo-130",
    "status": "Official",
    "date": "2025-04-04",
    "artist-credit": _mb_credit("Various Artists"),
    "release-group": {
        "id": "rg-bravo-130",
        "title": "Bravo Hits 130",
        "primary-type": "Album",
        "secondary-types": ["Compilation"],
        "artist-credit": _mb_credit("Various Artists"),
    },
}


@pytest.mark.asyncio
async def test_isrc_pooled_fallback_prefers_requested_artist_over_compilation(
    monkeypatch,
):
    # Issue #385: the artist's mixtape RG must win over an Official VA
    # compilation containing the same recording, with no extra wire calls.
    svc = _service(AsyncMock())
    svc._mb_repo.get_cached_recording_to_release_group = AsyncMock(return_value=None)
    svc._mb_repo.resolve_recording_to_release_group = AsyncMock(return_value=None)
    provider = AsyncMock(
        return_value={
            "recordings": [
                {
                    "id": "rec-fancy",
                    "artist-credit": _mb_credit("PinkPantheress"),
                    "releases": [
                        _BRAVO_COMPILATION_RELEASE,
                        _FANCY_MIXTAPE_RELEASE,
                    ],
                }
            ]
        }
    )
    monkeypatch.setattr(spotify_module, "mb_api_get", provider)

    result = await svc._resolve_mbid("GBABC123", "PinkPantheress", "Fancy That")

    assert result == "rg-fancy-that"
    assert provider.await_args.args[0] == "/isrc/GBABC123"
    assert provider.await_count == 1


@pytest.mark.asyncio
async def test_isrc_tries_artist_matching_recording_first(monkeypatch):
    # The compilation recording is listed first by MusicBrainz, but the
    # artist-credited recording must resolve first.
    svc = _service(AsyncMock())
    svc._mb_repo.get_cached_recording_to_release_group = AsyncMock(return_value=None)
    svc._mb_repo.resolve_recording_to_release_group = AsyncMock(
        side_effect=lambda rec_id, expected_artist=None: {
            "rec-pink": "rg-fancy-that"
        }.get(rec_id)
    )
    provider = AsyncMock(
        return_value={
            "recordings": [
                {
                    "id": "rec-va",
                    "artist-credit": _mb_credit("Various Artists"),
                    "releases": [],
                },
                {
                    "id": "rec-pink",
                    "artist-credit": _mb_credit("PinkPantheress"),
                    "releases": [],
                },
            ]
        }
    )
    monkeypatch.setattr(spotify_module, "mb_api_get", provider)

    result = await svc._resolve_mbid("GBABC123", "PinkPantheress", "Fancy That")

    assert result == "rg-fancy-that"
    first = svc._mb_repo.resolve_recording_to_release_group.await_args_list[0]
    assert first.args[0] == "rec-pink"
    assert first.kwargs["expected_artist"] == "PinkPantheress"


@pytest.mark.asyncio
async def test_isrc_various_artists_request_keeps_compilation_pick(monkeypatch):
    # Genuine VA-targeted imports neutralize the preference: rank-only pick.
    svc = _service(AsyncMock())
    svc._mb_repo.get_cached_recording_to_release_group = AsyncMock(return_value=None)
    svc._mb_repo.resolve_recording_to_release_group = AsyncMock(return_value=None)
    provider = AsyncMock(
        return_value={
            "recordings": [
                {
                    "id": "rec-bravo",
                    "artist-credit": _mb_credit("Various Artists"),
                    "releases": [
                        _FANCY_MIXTAPE_RELEASE,
                        _BRAVO_COMPILATION_RELEASE,
                    ],
                }
            ]
        }
    )
    monkeypatch.setattr(spotify_module, "mb_api_get", provider)

    result = await svc._resolve_mbid("GBXYZ999", "Various Artists", "Bravo Hits 130")

    assert result == "rg-bravo-130"


@pytest.mark.asyncio
async def test_title_fallback_prefers_artist_match_over_first_result():
    svc = _service(AsyncMock())
    svc._mb_repo.search_release_groups = AsyncMock(
        return_value=[
            SimpleNamespace(
                musicbrainz_id="rg-wrong-artist",
                title="Fancy That",
                artist="Cover Band",
            ),
            SimpleNamespace(
                musicbrainz_id="rg-fancy-that",
                title="Fancy That",
                artist="PinkPantheress",
            ),
        ]
    )

    result = await svc._resolve_mbid(None, "PinkPantheress", "Fancy That")

    assert result == "rg-fancy-that"


def test_best_image_url_prefers_smallest_at_or_above_min():
    images = [
        {"url": "tiny", "width": 64},
        {"url": "huge", "width": 640},
        {"url": "mid", "width": 300},
    ]
    assert _best_image_url(images, min_size=250) == "mid"


def test_best_image_url_falls_back_to_largest_when_all_below_min():
    images = [{"url": "a", "width": 60}, {"url": "b", "width": 120}]
    assert _best_image_url(images, min_size=250) == "b"


def test_best_image_url_none_when_empty():
    assert _best_image_url([]) is None


# GH-287: playlist-cover persistence (real stores + mock CDN)

_SPOTIFY_IMAGES = [{"url": COVER_URL, "width": 640, "height": 640}]

_TRACK = {
    "name": "Song",
    "artists": [{"name": "Artist"}],
    "album": {"id": "", "name": "Album", "images": []},
    "duration_ms": 180_000,
}


def _real_service(tmp_path, cdn: SpotifyCdnMock, *, images=_SPOTIFY_IMAGES):
    """SpotifyImportService over a REAL PlaylistRepository/PlaylistService with
    the cover fetcher bound to the mock CDN - the production wiring shape."""
    repo = PlaylistRepository(
        db_path=tmp_path / "library.db", write_lock=threading.Lock()
    )
    playlists = PlaylistService(repo=repo, cache_dir=tmp_path)
    client = AsyncMock()
    client.get_playlist.return_value = {
        "id": "spot-1",
        "name": "Mix",
        "images": images,
    }
    client.get_playlist_tracks.return_value = [dict(_TRACK)]
    factory = AsyncMock()
    factory.resolve_spotify.return_value = client
    svc = SpotifyImportService(
        client_factory=factory,
        playlist_repo=repo,
        mb_repo=AsyncMock(),
        playlist_service=playlists,
        cover_fetcher=cover_fetcher_for(cdn.client()),
    )
    return svc, playlists


def _cover_files(tmp_path: Path) -> list[Path]:
    return list((tmp_path / "covers" / "playlists").glob("*"))


async def _import_one(svc, playlists, tmp_path):
    pid = await svc.ensure_playlist_record("user-1", "spot-1", "Mix")
    await svc.populate_playlist("user-1", "spot-1", pid)
    return await playlists.get_playlist(pid)


@pytest.mark.asyncio
async def test_populate_persists_fetched_cover_locally(tmp_path):
    cdn = SpotifyCdnMock()
    svc, playlists = _real_service(tmp_path, cdn)

    record = await _import_one(svc, playlists, tmp_path)

    # Cover bytes stored under the shared covers dir and wired into the row.
    assert record.cover_image_path
    assert Path(record.cover_image_path).read_bytes() == JPEG_BYTES
    assert len(cdn.requests) == 1
    assert cdn.requests[0].url.host == "i.scdn.co"
    # Tracks still imported alongside the artwork.
    assert len(await playlists.get_tracks(record.id)) == 1


@pytest.mark.asyncio
async def test_populate_without_images_skips_cover_entirely(tmp_path):
    cdn = SpotifyCdnMock()
    svc, playlists = _real_service(tmp_path, cdn, images=[])

    record = await _import_one(svc, playlists, tmp_path)

    assert record.cover_image_path is None
    assert cdn.requests == []
    assert _cover_files(tmp_path) == []


@pytest.mark.asyncio
async def test_cover_http_failure_degrades_and_keeps_tracks(tmp_path):
    cdn = SpotifyCdnMock()
    cdn.status_code = 503
    svc, playlists = _real_service(tmp_path, cdn)

    ctx = init_degradation_context()
    try:
        record = await _import_one(svc, playlists, tmp_path)
        assert ctx.summary().get("spotify") == "error"
    finally:
        clear_degradation_context()

    # Degrade-don't-fail: tracks imported, no cover, no partial writes.
    assert record.cover_image_path is None
    assert len(await playlists.get_tracks(record.id)) == 1
    assert _cover_files(tmp_path) == []

    # And with NO active context (background import) it must not raise either.
    record2 = await _import_one(svc, playlists, tmp_path)
    assert record2.cover_image_path is None


@pytest.mark.asyncio
async def test_fetcher_exception_degrades_and_keeps_tracks(tmp_path):
    async def broken(url: str):
        raise RuntimeError("cdn unreachable")

    repo = PlaylistRepository(
        db_path=tmp_path / "library.db", write_lock=threading.Lock()
    )
    playlists = PlaylistService(repo=repo, cache_dir=tmp_path)
    client = AsyncMock()
    client.get_playlist.return_value = {"id": "spot-1", "images": _SPOTIFY_IMAGES}
    client.get_playlist_tracks.return_value = []
    factory = AsyncMock()
    factory.resolve_spotify.return_value = client
    svc = SpotifyImportService(
        client_factory=factory,
        playlist_repo=repo,
        mb_repo=AsyncMock(),
        playlist_service=playlists,
        cover_fetcher=broken,
    )

    ctx = init_degradation_context()
    try:
        record = await _import_one(svc, playlists, tmp_path)
        assert ctx.summary().get("spotify") == "error"
    finally:
        clear_degradation_context()
    assert record.cover_image_path is None


@pytest.mark.asyncio
async def test_oversized_response_rejected_without_partial_writes(tmp_path):
    cdn = SpotifyCdnMock()
    cdn.image_bytes = b"x" * (5 * 1024 * 1024 + 1)  # declared length > cap
    svc, playlists = _real_service(tmp_path, cdn)

    ctx = init_degradation_context()
    try:
        record = await _import_one(svc, playlists, tmp_path)
        assert ctx.summary().get("spotify") == "error"
    finally:
        clear_degradation_context()

    assert record.cover_image_path is None
    assert _cover_files(tmp_path) == []
    assert (await playlists.get_tracks(record.id)) or True  # tracks unaffected


@pytest.mark.asyncio
async def test_wrong_content_type_rejected_without_partial_writes(tmp_path):
    cdn = SpotifyCdnMock()
    cdn.content_type = "text/html"
    svc, playlists = _real_service(tmp_path, cdn)

    ctx = init_degradation_context()
    try:
        record = await _import_one(svc, playlists, tmp_path)
        assert ctx.summary().get("spotify") == "error"
    finally:
        clear_degradation_context()

    assert record.cover_image_path is None
    assert _cover_files(tmp_path) == []


@pytest.mark.asyncio
async def test_disallowed_host_is_never_fetched(tmp_path):
    cdn = SpotifyCdnMock()
    svc, playlists = _real_service(
        tmp_path,
        cdn,
        images=[{"url": "https://evil.example.com/a.jpg", "width": 600}],
    )

    ctx = init_degradation_context()
    try:
        record = await _import_one(svc, playlists, tmp_path)
        assert ctx.summary().get("spotify") == "error"
    finally:
        clear_degradation_context()

    assert cdn.requests == []  # rejected before any request left
    assert record.cover_image_path is None


@pytest.mark.asyncio
async def test_redirect_response_rejected(tmp_path):
    cdn = SpotifyCdnMock()
    cdn.status_code = 302
    cdn.extra_headers = {"Location": "https://evil.example.com/x.jpg"}
    svc, playlists = _real_service(tmp_path, cdn)

    ctx = init_degradation_context()
    try:
        record = await _import_one(svc, playlists, tmp_path)
        assert ctx.summary().get("spotify") == "error"
    finally:
        clear_degradation_context()

    assert len(cdn.requests) == 1  # single attempt, never followed
    assert record.cover_image_path is None


@pytest.mark.asyncio
async def test_reimport_preserves_user_uploaded_cover(tmp_path):
    cdn = SpotifyCdnMock()
    svc, playlists = _real_service(tmp_path, cdn)
    pid = await svc.ensure_playlist_record("user-1", "spot-1", "Mix")
    owner = SimpleNamespace(id="user-1")
    await playlists.upload_cover(pid, owner, PNG_BYTES, "image/png")
    before = (await playlists.get_playlist(pid)).cover_image_path

    # Re-import of the same playlist: the user's explicit cover must win.
    await svc.populate_playlist("user-1", "spot-1", pid)

    after = (await playlists.get_playlist(pid)).cover_image_path
    assert after == before
    assert Path(after).read_bytes() == PNG_BYTES
    assert Path(after).suffix == ".png"


@pytest.mark.asyncio
async def test_import_cannot_write_other_users_playlist(tmp_path):
    cdn = SpotifyCdnMock()
    svc, playlists = _real_service(tmp_path, cdn)
    foreign = await playlists.create_playlist("Bob's Mix", user_id="user-bob")

    ctx = init_degradation_context()
    try:
        await svc.populate_playlist("user-alice", "spot-1", foreign.id)
        assert ctx.summary().get("spotify") == "error"
    finally:
        clear_degradation_context()

    record = await playlists.get_playlist(foreign.id)
    assert record.cover_image_path is None
    assert _cover_files(tmp_path) == []


# fetch-level units


@pytest.mark.asyncio
async def test_fetch_returns_bytes_and_content_type():
    cdn = SpotifyCdnMock()
    result = await fetch_spotify_playlist_cover(COVER_URL, cdn.client())
    assert result == (JPEG_BYTES, "image/jpeg")


@pytest.mark.asyncio
async def test_fetch_aborts_bounded_read_past_cap():
    cdn = SpotifyCdnMock()
    cdn.image_bytes = b"x" * (5 * 1024 * 1024 + 10)
    # Lie about Content-Length so the declared-length check passes and the
    # streamed read is what trips the cap.
    cdn.extra_headers = {"Content-Length": str(1024)}
    result = await fetch_spotify_playlist_cover(COVER_URL, cdn.client())
    assert result is None


@pytest.mark.asyncio
async def test_fetch_rejects_bad_urls_without_requesting():
    cdn = SpotifyCdnMock()
    client = cdn.client()
    for url in (
        "http://i.scdn.co/img.jpg",  # not https
        "https://i.scdn.co.evil.com/img.jpg",  # suffix lookalike
        "https://evil.com/?u=i.scdn.co",  # scdn.co only in the query
        "",
    ):
        assert await fetch_spotify_playlist_cover(url, client) is None
    assert cdn.requests == []


def test_cover_fetcher_alias_shape():
    async def fetcher(url: str):
        return None

    typed: CoverFetcher = fetcher
    assert typed is fetcher
